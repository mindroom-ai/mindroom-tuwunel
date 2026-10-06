use futures::{future::try_join, pin_mut};
use ruma::{
	EventId, RoomId, UserId,
	events::{
		StateEventType, TimelineEventType,
		room::{
			history_visibility::{HistoryVisibility, RoomHistoryVisibilityEventContent},
			member::{MembershipState, RoomMemberEventContent},
			tombstone::RoomTombstoneEventContent,
		},
	},
};
use tuwunel_core::{
	Err, Result, at, implement,
	matrix::{Event, PduCount, StateKey},
	pdu::PduBuilder,
	utils::{FutureBoolExt, result::NotFound},
};

use crate::rooms::{short::ShortStateHash, state::RoomMutexGuard};

/// How many of a former member's membership events are walked back to find
/// their last join; past that they keep only the events they were joined for.
const MEMBERSHIP_STEPS: usize = 16;

/// Checks if a given user can redact a given event
///
/// If federation is true, it allows redaction events from any user of the
/// same server as the original event sender
#[implement(super::Service)]
pub async fn user_can_redact(
	&self,
	redacts: &EventId,
	sender: &UserId,
	room_id: &RoomId,
	federation: bool,
) -> Result<bool> {
	let redacting_event = self.services.timeline.get_pdu(redacts).await;

	if redacting_event
		.as_ref()
		.is_ok_and(|pdu| pdu.room_id() != room_id)
	{
		return Ok(false);
	}

	if redacting_event
		.as_ref()
		.is_ok_and(|pdu| *pdu.kind() == TimelineEventType::RoomCreate)
	{
		return Err!(Request(Forbidden("Redacting m.room.create is not safe, forbidding.")));
	}

	if redacting_event
		.as_ref()
		.is_ok_and(|pdu| *pdu.kind() == TimelineEventType::RoomServerAcl)
	{
		return Err!(Request(Forbidden(
			"Redacting m.room.server_acl will result in the room being inaccessible for \
			 everyone (empty allow key), forbidding."
		)));
	}

	match self.get_power_levels(room_id).await {
		| Ok(power_levels) => Ok(power_levels.user_can_redact_event_of_other(sender)
			|| power_levels.user_can_redact_own_event(sender)
				&& match redacting_event {
					| Ok(redacting_event) =>
						if federation {
							redacting_event.sender().server_name() == sender.server_name()
						} else {
							redacting_event.sender() == sender
						},
					| _ => false,
				}),
		| _ => {
			// Falling back on m.room.create to judge power level
			match self
				.room_state_get(room_id, &StateEventType::RoomCreate, "")
				.await
			{
				| Ok(room_create) => Ok(room_create.sender() == sender
					|| redacting_event
						.as_ref()
						.is_ok_and(|redacting_event| redacting_event.sender() == sender)),
				| _ => Err!(Database(
					"No m.room.power_levels or m.room.create events in database for room"
				)),
			}
		},
	}
}

/// Reports whether a user may see an event under its historical visibility.
///
/// Missing event state is allowed, and missing or invalid history visibility
/// defaults to `shared`. The `shared` decision also accounts for the user's
/// membership intervals around the event. Under `joined` and `invited`, the
/// user's own membership event is visible when the membership it sets
/// qualifies, per the spec's before-or-after rule.
#[implement(super::Service)]
#[tracing::instrument(skip_all, level = "trace")]
pub async fn user_can_see_event<Pdu>(&self, user_id: &UserId, pdu: &Pdu) -> bool
where
	Pdu: Event,
{
	let Some((shortstatehash, history_visibility)) =
		self.history_visibility_at(pdu.event_id()).await
	else {
		return true;
	};

	match history_visibility {
		| HistoryVisibility::WorldReadable => true,

		// Allow the user's own invite or join, or a user at least invited at the event
		| HistoryVisibility::Invited =>
			matches!(
				pdu.membership_for(user_id),
				Some(MembershipState::Join | MembershipState::Invite)
			) || self
				.user_was_invited(shortstatehash, user_id)
				.await,

		// Allow the user's own join, or a user joined at the event
		| HistoryVisibility::Joined =>
			matches!(pdu.membership_for(user_id), Some(MembershipState::Join))
				|| self
					.user_was_joined(shortstatehash, user_id)
					.await,

		// An unrecognized value is treated as shared.
		| HistoryVisibility::Shared | _ =>
			self.user_shared_history(shortstatehash, pdu.room_id(), pdu.event_id(), user_id)
				.await,
	}
}

/// The room state an event was sent in and the history visibility it carried.
///
/// Missing or invalid history visibility reads as `shared`, and `None` means
/// the event has no recorded state.
#[implement(super::Service)]
#[tracing::instrument(skip_all, level = "trace")]
async fn history_visibility_at(
	&self,
	event_id: &EventId,
) -> Option<(ShortStateHash, HistoryVisibility)> {
	let shortstatehash = self
		.services
		.state
		.pdu_shortstatehash(event_id)
		.await
		.ok()?;

	let history_visibility = self
		.state_get_content(shortstatehash, &StateEventType::RoomHistoryVisibility, "")
		.await
		.map_or(HistoryVisibility::Shared, |c: RoomHistoryVisibilityEventContent| {
			c.history_visibility
		});

	Some((shortstatehash, history_visibility))
}

/// Whether a user may see an event under `shared` history visibility.
///
/// A current member sees the whole room, which the first check answers without
/// touching room state. A former member keeps events up to their latest leave
/// that were sent before their last join, and lookup failures deny access.
#[implement(super::Service)]
async fn user_shared_history(
	&self,
	shortstatehash: ShortStateHash,
	room_id: &RoomId,
	event_id: &EventId,
	user_id: &UserId,
) -> bool {
	let state_cache = &self.services.state_cache;

	if state_cache.is_joined(user_id, room_id).await
		|| self
			.user_was_joined(shortstatehash, user_id)
			.await
	{
		return true;
	}

	if !state_cache.once_joined(user_id, room_id).await {
		return false;
	}

	let Ok(left_count) = state_cache.get_left_count(room_id, user_id).await else {
		return false;
	};

	let Ok(event_count) = self
		.services
		.timeline
		.get_pdu_count(event_id)
		.await
	else {
		return false;
	};

	let left_count = PduCount::from_unsigned(left_count);
	if event_count > left_count {
		return false;
	}

	// A knock or an invite since the user's last join drops their leave count,
	// and the next leave records a later one, so walk back from their current
	// member event to their last join; the event is shared with them if they
	// joined at or after it.
	let Ok(mut member) = self
		.room_state_get(room_id, &StateEventType::RoomMember, user_id.as_str())
		.await
	else {
		return false;
	};

	for _ in 0..MEMBERSHIP_STEPS {
		if member.membership_for(user_id) == Some(MembershipState::Join) {
			return self
				.services
				.timeline
				.get_pdu_count(member.event_id())
				.await
				.is_ok_and(|join_count| join_count >= event_count);
		}

		let Ok(before) = self
			.services
			.state
			.pdu_shortstatehash(member.event_id())
			.await
		else {
			return false;
		};

		let Ok(previous) = self
			.state_get(before, &StateEventType::RoomMember, user_id.as_str())
			.await
		else {
			return false;
		};

		member = previous;
	}

	false
}

/// Whether a user is allowed to see an event, based on
/// the room's history_visibility at that event's state.
#[implement(super::Service)]
#[tracing::instrument(skip_all, level = "trace")]
pub async fn user_can_see_state_events(&self, user_id: &UserId, room_id: &RoomId) -> bool {
	if self
		.services
		.state_cache
		.is_joined(user_id, room_id)
		.await
	{
		return true;
	}

	let history_visibility = self
		.room_state_get_content(room_id, &StateEventType::RoomHistoryVisibility, "")
		.await
		.map_or(HistoryVisibility::Shared, |c: RoomHistoryVisibilityEventContent| {
			c.history_visibility
		});

	match history_visibility {
		| HistoryVisibility::WorldReadable => true,

		| HistoryVisibility::Invited =>
			self.services
				.state_cache
				.is_invited(user_id, room_id)
				.await,

		| HistoryVisibility::Shared =>
			self.services
				.state_cache
				.once_joined(user_id, room_id)
				.await,

		| _ => false,
	}
}

/// The room state a user admitted by `user_can_see_state_events` reads.
///
/// A former member reads the state from when they left, as the spec requires,
/// and everyone else reads the current state.
#[implement(super::Service)]
pub async fn user_visible_shortstatehash(
	&self,
	user_id: &UserId,
	room_id: &RoomId,
) -> Result<ShortStateHash> {
	let current = self
		.services
		.state
		.get_room_shortstatehash(room_id)
		.await?;

	if self
		.services
		.state_cache
		.is_joined(user_id, room_id)
		.await
	{
		return Ok(current);
	}

	let Some(member) = self
		.state_get(current, &StateEventType::RoomMember, user_id.as_str())
		.await
		.optional()?
	else {
		return Ok(current);
	};

	let content: RoomMemberEventContent = member.get_content()?;
	if !matches!(content.membership, MembershipState::Leave | MembershipState::Ban) {
		return Ok(current);
	}

	self.departure_shortstatehash(room_id, member.event_id(), current)
		.await
		.map(at!(1))
}

/// The timeline count of a member's leave or ban, and the room state after it.
///
/// After the room's newest timeline event that state is `current`; after any
/// other it is the state the next timeline event was sent in.
#[implement(super::Service)]
pub async fn departure_shortstatehash(
	&self,
	room_id: &RoomId,
	departure: &EventId,
	current: ShortStateHash,
) -> Result<(PduCount, ShortStateHash)> {
	let timeline = &self.services.timeline;
	let count = timeline.get_pdu_count(departure);
	let latest = timeline.last_timeline_count(None, room_id, None);
	let (count, latest) = try_join(count, latest).await?;

	let shortstatehash = if count == latest {
		current
	} else {
		timeline
			.next_shortstatehash(room_id, count)
			.await?
	};

	Ok((count, shortstatehash))
}

/// Whether a user may see a room: a current or prior membership (joined,
/// invited, left), or a world-readable room. Forgetting a room clears the
/// user's left-state, so a forgotten room is not visible.
#[implement(super::Service)]
pub async fn user_can_see_room(&self, user_id: &UserId, room_id: &RoomId) -> bool {
	let state_cache = &self.services.state_cache;
	let joined = state_cache.is_joined(user_id, room_id);
	let invited = state_cache.is_invited(user_id, room_id);
	let left = state_cache.is_left(user_id, room_id);
	let world_readable = self.is_world_readable(room_id);

	pin_mut!(joined, invited, left, world_readable);
	joined
		.or(invited)
		.or(left)
		.or(world_readable)
		.await
}

/// Reports whether the room's history was world-readable at an event.
///
/// A peek may show only such events, so an event without recorded state, or
/// with missing or invalid history visibility, does not qualify. The event that
/// makes the room world-readable counts as well, as the spec requires.
#[implement(super::Service)]
#[tracing::instrument(skip_all, level = "trace")]
pub async fn is_world_readable_at<Pdu>(&self, pdu: &Pdu) -> bool
where
	Pdu: Event,
{
	let opens_history = pdu.is_type_and_state_key(&TimelineEventType::RoomHistoryVisibility, "")
		&& pdu
			.get_content()
			.is_ok_and(|c: RoomHistoryVisibilityEventContent| {
				c.history_visibility == HistoryVisibility::WorldReadable
			});

	opens_history
		|| self
			.history_visibility_at(pdu.event_id())
			.await
			.is_some_and(|(_, history_visibility)| {
				history_visibility == HistoryVisibility::WorldReadable
			})
}

#[implement(super::Service)]
pub async fn user_can_invite(
	&self,
	room_id: &RoomId,
	sender: &UserId,
	target_user: &UserId,
	state_lock: &RoomMutexGuard,
) -> bool {
	self.services
		.timeline
		.create_hash_and_sign_event(
			PduBuilder::state(
				target_user.as_str(),
				&RoomMemberEventContent::new(MembershipState::Invite),
			),
			sender,
			room_id,
			state_lock,
		)
		.await
		.is_ok()
}

#[implement(super::Service)]
pub async fn user_can_tombstone(
	&self,
	room_id: &RoomId,
	user_id: &UserId,
	state_lock: &RoomMutexGuard,
) -> bool {
	if !self
		.services
		.state_cache
		.is_joined(user_id, room_id)
		.await
	{
		return false;
	}

	self.services
		.timeline
		.create_hash_and_sign_event(
			PduBuilder::state(StateKey::new(), &RoomTombstoneEventContent {
				replacement_room: room_id.into(), // placeholder,
				body: "Not a valid m.room.tombstone.".into(),
			}),
			user_id,
			room_id,
			state_lock,
		)
		.await
		.is_ok()
}

#[cfg(test)]
mod tests;
