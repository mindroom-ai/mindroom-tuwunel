use std::{collections::HashSet, fmt::Debug, sync::Arc};

use futures::{FutureExt, StreamExt, future::join};
use ruma::{
	EventId, UserId,
	api::client::push::ProfileTag,
	events::{
		AnySyncTimelineEvent, TimelineEventType,
		invite_permission_config::InvitePermission,
		room::{member::MembershipState, power_levels::RoomPowerLevels},
	},
	push::{Action, Actions, HighlightTweakValue, Tweak},
	serde::Raw,
};
use serde::{Deserialize, Serialize};
use tracing::Level;
use tuwunel_core::{
	Result, implement,
	matrix::{
		event::Event,
		pdu::{Count, Pdu, PduId, RawPduId},
	},
	trace,
	utils::{BoolExt, ReadyExt, future::TryExtExt, result::ErrLog, time::now_millis},
};
use tuwunel_database::{Deserialized, Json, Map};

use super::{Evaluate, RelatedEvents};
use crate::rooms::short::ShortRoomId;

/// Compact metadata stored for each notified event.
///
/// The database key supplies the user and PDU count. The stored `ShortRoomId`
/// combines with that count to reconstruct the event's `PduId`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Notified {
	/// Milliseconds time at which the event notification was sent.
	pub ts: u64,

	/// ShortRoomId
	pub sroomid: ShortRoomId,

	/// The profile tag of the rule that matched this event.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub tag: Option<ProfileTag>,

	/// Actions vector
	pub actions: Actions,
}

/// The values of one appended event shared by every recipient of it.
///
/// Each is resolved once per event and read once per recipient, so grouping
/// them keeps the per-recipient call from growing an argument per lookup.
#[derive(Clone, Copy)]
struct Appended<'a> {
	pdu_id: &'a RawPduId,
	pdu: &'a Pdu,
	power_levels: Option<&'a RoomPowerLevels>,
	serialized: &'a Raw<AnySyncTimelineEvent>,
	thread_root: Option<&'a EventId>,
	related_events: Option<&'a Arc<RelatedEvents>>,
}

/// Called by timeline append_pdu.
#[implement(super::Service)]
#[tracing::instrument(name = "append", level = "debug", skip_all)]
pub(crate) async fn append_pdu(&self, pdu_id: RawPduId, pdu: &Pdu) -> Result {
	let push_target = self
		.services
		.state_cache
		.active_local_users_in_room(pdu.room_id())
		.map(ToOwned::to_owned)
		.ready_filter(|user| *user != pdu.sender())
		.filter_map(async |recipient_user| {
			self.services
				.users
				.user_is_ignored(pdu.sender(), &recipient_user)
				.await
				.is_false()
				.then_some(recipient_user)
		})
		.collect::<HashSet<_>>();

	let power_levels = self
		.services
		.state_accessor
		.get_power_levels(pdu.room_id())
		.ok();

	let (mut push_target, power_levels) = join(push_target, power_levels).boxed().await;

	if *pdu.kind() == TimelineEventType::RoomMember
		&& let Some(Ok(target_user_id)) = pdu.state_key().map(UserId::parse)
		&& self
			.services
			.users
			.is_active_local(&target_user_id)
			.await
		&& self.invite_notifiable(pdu, &target_user_id).await
	{
		push_target.insert(target_user_id);
	}

	if push_target.is_empty() {
		return Ok(());
	}

	let serialized = pdu.to_format();
	let (thread_root, related_events) =
		join(self.services.threads.get_thread_id(pdu), self.related_events(pdu)).await;

	let appended = Appended {
		pdu_id: &pdu_id,
		pdu,
		power_levels: power_levels.as_ref(),
		serialized: &serialized,
		thread_root: thread_root.as_deref(),
		related_events: related_events.as_ref(),
	};

	let _cork = self.db.db.cork();
	for user in &push_target {
		self.append_pdu_for_user(user, appended).await;
	}

	Ok(())
}

/// MSC4155: whether an invite may notify the user it names.
///
/// Every other membership transition notifies as before; only an invite the
/// recipient ignores or blocks is withheld, matching what sync serves them.
#[implement(super::Service)]
async fn invite_notifiable(&self, pdu: &Pdu, user_id: &UserId) -> bool {
	#[derive(Deserialize)]
	struct Membership {
		membership: MembershipState,
	}

	let is_invite = pdu
		.get_content()
		.is_ok_and(|content: Membership| content.membership == MembershipState::Invite);

	!is_invite
		|| self
			.services
			.users
			.invite_permission(pdu.sender(), user_id)
			.await
			.eq(&InvitePermission::Allow)
}

#[implement(super::Service)]
async fn append_pdu_for_user(
	&self,
	user: &UserId,
	Appended {
		pdu_id,
		pdu,
		power_levels,
		serialized,
		thread_root,
		related_events,
	}: Appended<'_>,
) {
	let ruleset = self.ruleset(user).await;
	let actions = self
		.get_actions(Evaluate {
			user,
			ruleset: &ruleset,
			power_levels,
			pdu: serialized,
			room_id: pdu.room_id(),
			related_events,
		})
		.await;

	let (notify, highlight) = counted(actions);

	trace!(
		%user,
		event_id = %pdu.event_id(),
		actions = %actions.len(),
		notify,
		highlight,
		"Push rules evaluated",
	);

	if notify || highlight {
		// Publish actions and counts together. A recount scans without this
		// lock, then validates its count/read-stamp snapshot before applying it.
		let _lock = self
			.notification_increment_mutex
			.lock(&(pdu.room_id().to_owned(), user.to_owned()))
			.await;

		let id: PduId = (*pdu_id).into();
		let notified = Notified {
			ts: now_millis(),
			sroomid: id.shortroomid,
			tag: None,
			actions: actions.into(),
		};

		if matches!(id.count, Count::Normal(_)) {
			self.db
				.useridcount_notification
				.put((user, id.count.into_unsigned()), Json(notified));
		}

		// Mutually-exclusive partition: each notify (and each highlight)
		// lands in either the room-level or thread bucket, never both.
		let (notifications, highlights) =
			(&self.db.userroomid_notificationcount, &self.db.userroomid_highlightcount);

		match thread_root {
			| None => {
				let key = (user, pdu.room_id());
				join(
					notify.then_async(|| increment(notifications, key)),
					highlight.then_async(|| increment(highlights, key)),
				)
				.await;
			},
			| Some(root) => {
				let key = (user, pdu.room_id(), root);
				join(
					notify.then_async(|| increment(notifications, key)),
					highlight.then_async(|| increment(highlights, key)),
				)
				.await;
			},
		}
	}

	if notify || highlight || self.services.config.push_everything {
		self.get_pushkeys(user)
			.map(ToOwned::to_owned)
			.ready_for_each(|push_key| {
				self.services
					.sending
					.send_pdu_push(pdu_id, user, push_key)
					.log_err(Level::TRACE)
					.ok();
			})
			.await;
	}
}

/// Whether `actions` notify, and whether they highlight: what an event adds
/// to the unread notification and highlight counts.
pub(super) fn counted(actions: &[Action]) -> (bool, bool) {
	let notify = actions.iter().any(Action::should_notify);

	let highlight = actions.iter().any(|action| {
		matches!(action, Action::SetTweak(Tweak::Highlight(HighlightTweakValue::Yes)))
	});

	(notify, highlight)
}

async fn increment<K>(db: &Arc<Map>, key: K)
where
	K: Serialize + Debug + Copy,
{
	let old: u64 = db.qry(&key).await.deserialized().unwrap_or(0);
	let new = old.saturating_add(1);
	db.put(key, new);
}
