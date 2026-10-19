use std::{collections::HashSet, sync::Arc};

use futures::{
	FutureExt, StreamExt,
	future::{join, join3, join4},
};
use ruma::{
	EventId, RoomId, UserId,
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
	utils::{
		BoolExt, ReadyExt, future::TryExtExt, option::OptionExt, result::ErrLog, time::now_millis,
	},
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
	/// The position of the event a terminal MindRoom stream edit replaces.
	replaced: Option<PduId>,
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
	let (thread_root, related_events, replaced) = join3(
		self.services.threads.get_thread_id(pdu),
		self.related_events(pdu),
		self.replaced_pdu_id(&pdu_id, pdu),
	)
	.await;

	let appended = Appended {
		pdu_id: &pdu_id,
		pdu,
		power_levels: power_levels.as_ref(),
		serialized: &serialized,
		thread_root: thread_root.as_deref(),
		related_events: related_events.as_ref(),
		replaced,
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
		replaced,
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

	let notify = actions.iter().any(Action::should_notify);

	let highlight = actions.iter().any(|action| {
		matches!(action, Action::SetTweak(Tweak::Highlight(HighlightTweakValue::Yes)))
	});

	trace!(
		%user,
		event_id = %pdu.event_id(),
		actions = %actions.len(),
		notify,
		highlight,
		"Push rules evaluated",
	);

	// A terminal stream edit notifies in place of its suppressed original, so
	// once the user has read that original it is not unread again.
	let read = match replaced {
		| Some(replaced) if notify || highlight =>
			self.read_through(user, pdu.room_id(), thread_root, replaced)
				.await,
		| _ => false,
	};

	let (notify_unread, highlight_unread) = (notify && !read, highlight && !read);

	// Mutually-exclusive partition: each notify (and each highlight)
	// lands in either the room-level or thread bucket, never both.
	let main_notify = (notify_unread && thread_root.is_none())
		.then_async(|| self.increment_notificationcount(pdu.room_id(), user));

	let main_highlight = (highlight_unread && thread_root.is_none())
		.then_async(|| self.increment_highlightcount(pdu.room_id(), user));

	let thread_notify = thread_root
		.filter(|_| notify_unread)
		.map_async(|root| self.increment_thread_notificationcount(pdu.room_id(), user, root));

	let thread_highlight = thread_root
		.filter(|_| highlight_unread)
		.map_async(|root| self.increment_thread_highlightcount(pdu.room_id(), user, root));

	join4(main_notify, thread_notify, main_highlight, thread_highlight).await;

	if notify || highlight {
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

/// The position of the same-room event a terminal MindRoom stream edit
/// replaces.
#[implement(super::Service)]
async fn replaced_pdu_id(&self, pdu_id: &RawPduId, pdu: &Pdu) -> Option<PduId> {
	let target = super::mindroom_terminal_edit_target(pdu)?;
	let target: PduId = self
		.services
		.timeline
		.get_pdu_id(&target)
		.await
		.ok()?
		.into();

	(target.shortroomid == PduId::from(*pdu_id).shortroomid
		&& matches!(target.count, Count::Normal(_)))
	.then_some(target)
}

/// Whether the user has read the `replaced` event: the read watermark of the
/// bucket it counts in (its thread, or else the room) covers it, or the
/// user's own send in that same thread or timeline came after it. A
/// main-timeline receipt moves the room watermark but leaves thread counts,
/// so it covers no thread.
#[implement(super::Service)]
async fn read_through(
	&self,
	user: &UserId,
	room_id: &RoomId,
	thread_root: Option<&EventId>,
	replaced: PduId,
) -> bool {
	let count = replaced.count.into_unsigned();
	let bucket = async {
		match thread_root {
			| Some(root) =>
				self.last_thread_notification_read(user, room_id, root)
					.await,
			| None => self.last_notification_read(user, room_id).await,
		}
	};

	let (bucket, sent_after) =
		join(bucket, self.sent_after_in_thread(user, room_id, thread_root, replaced)).await;

	bucket.is_ok_and(|last_read| last_read >= count) || sent_after
}

/// Whether the unthreaded private read marker, which the user's own send
/// moves to its own position, sits after `replaced` on an event of the same
/// thread, such as the reaction that stops a stream. The own send's reset
/// deletes the thread's watermark, so only this marker records the read; an
/// own send elsewhere in the room says nothing about this thread.
#[implement(super::Service)]
async fn sent_after_in_thread(
	&self,
	user: &UserId,
	room_id: &RoomId,
	thread_root: Option<&EventId>,
	replaced: PduId,
) -> bool {
	let Ok((marker, _)) = self
		.services
		.read_receipt
		.private_read_get_count(room_id, user)
		.await
	else {
		return false;
	};

	if marker < replaced.count.into_unsigned() {
		return false;
	}

	let marker = PduId {
		shortroomid: replaced.shortroomid,
		count: Count::Normal(marker),
	};

	let Ok(event) = self
		.services
		.timeline
		.get_pdu_from_id(&marker.into())
		.await
	else {
		return false;
	};

	self.services
		.threads
		.get_thread_id(&event)
		.await
		.as_deref()
		== thread_root
}

#[implement(super::Service)]
async fn increment_notificationcount(&self, room_id: &RoomId, user_id: &UserId) {
	let db = &self.db.userroomid_notificationcount;
	let key = (room_id.to_owned(), user_id.to_owned());
	let _lock = self.notification_increment_mutex.lock(&key).await;

	increment(db, (user_id, room_id)).await;
}

#[implement(super::Service)]
async fn increment_highlightcount(&self, room_id: &RoomId, user_id: &UserId) {
	let db = &self.db.userroomid_highlightcount;
	let key = (room_id.to_owned(), user_id.to_owned());
	let _lock = self.highlight_increment_mutex.lock(&key).await;

	increment(db, (user_id, room_id)).await;
}

#[implement(super::Service)]
async fn increment_thread_notificationcount(
	&self,
	room_id: &RoomId,
	user_id: &UserId,
	thread_root: &EventId,
) {
	let db = &self.db.userroomid_notificationcount;
	let key = (room_id.to_owned(), user_id.to_owned());
	let _lock = self.notification_increment_mutex.lock(&key).await;

	increment_thread(db, (user_id, room_id, thread_root)).await;
}

#[implement(super::Service)]
async fn increment_thread_highlightcount(
	&self,
	room_id: &RoomId,
	user_id: &UserId,
	thread_root: &EventId,
) {
	let db = &self.db.userroomid_highlightcount;
	let key = (room_id.to_owned(), user_id.to_owned());
	let _lock = self.highlight_increment_mutex.lock(&key).await;

	increment_thread(db, (user_id, room_id, thread_root)).await;
}

async fn increment(db: &Arc<Map>, key: (&UserId, &RoomId)) {
	let old: u64 = db.qry(&key).await.deserialized().unwrap_or(0);
	let new = old.saturating_add(1);
	db.put(key, new);
}

async fn increment_thread(db: &Arc<Map>, key: (&UserId, &RoomId, &EventId)) {
	let old: u64 = db.qry(&key).await.deserialized().unwrap_or(0);
	let new = old.saturating_add(1);
	db.put(key, new);
}
