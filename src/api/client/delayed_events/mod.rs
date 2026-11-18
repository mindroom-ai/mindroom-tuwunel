//! Delayed events (MSC4140): scheduling, management and lookup.
//!
//! Scheduling applies the policy an ordinary send would at the time of the
//! request, then checks the event against room auth without storing it. The
//! service sends it once due through the ordinary append path.
//!
//! MindRoom: a client can delay only its own MatrixRTC call leave, the one
//! delayed event its clients send. The send path rechecks suspension alone, so
//! any other event would skip what a send at that time is held to: the
//! account's lock, a deactivation that raced the scheduling, and the state
//! route's checks on memberships and aliases. A leave that skips them only
//! takes its sender out of a call, and its record holds no content.

mod delay_query;
mod get_delayed_event;
mod get_delayed_events;
mod send_delayed_event;
mod update_delayed_event;

#[cfg(test)]
mod tests;

use std::time::Duration;

use axum::extract::MatchedPath;
use futures::future::OptionFuture;
use ruma::{
	DeviceId, MilliSecondsSinceUnixEpoch, RoomId, TransactionId, UserId,
	events::{
		AnyMessageLikeEventContent, AnyStateEventContent, MessageLikeEventType, StateEventType,
		TimelineEventType,
	},
	serde::{JsonObject, Raw},
};
use serde_json::{from_str, value::RawValue as RawJsonValue};
use tuwunel_core::{
	Err, Result, err, implement,
	utils::time::{millis, now_millis},
};
use tuwunel_matrix::pdu::PduBuilder;
use tuwunel_service::{
	Services,
	delayed_events::{DelayId, Event, Limits},
	rooms::timeline::RoomMutexGuard,
	transaction_ids::RoomTxn,
};

pub(crate) use self::{
	delay_query::{send_message_event_or_delayed_route, send_state_event_or_delayed_route},
	get_delayed_event::get_delayed_event_route,
	get_delayed_events::get_delayed_events_route,
	send_delayed_event::send_delayed_event_route,
	update_delayed_event::{update_delayed_event_by_body_route, update_delayed_event_route},
};
use crate::{
	send::{check_send_content, check_send_policy},
	state::{allowed_to_send_state_event, check_member_content_size},
};

/// The type of the only event a client can delay, a MatrixRTC call membership.
const CALL_MEMBER: &str = "org.matrix.msc3401.call.member";

struct Schedule<'a> {
	sender: &'a UserId,
	device: Option<&'a DeviceId>,
	room_id: &'a RoomId,
	kind: Kind<'a>,
	delay: Duration,
	txn_id: Option<&'a TransactionId>,
	timestamp: Option<MilliSecondsSinceUnixEpoch>,

	/// Selects the stable error codes over the unstable ones.
	stable: bool,
}

#[derive(Clone, Copy)]
enum Kind<'a> {
	Message {
		event_type: &'a MessageLikeEventType,
		content: &'a Raw<AnyMessageLikeEventContent>,
	},
	State {
		event_type: &'a StateEventType,
		state_key: &'a str,
		content: &'a Raw<AnyStateEventContent>,
	},
}

/// Schedules an event and returns its `delay_id`.
///
/// Anything but the sender's own call leave is refused first.
///
/// A retried transaction returns the `delay_id` it first produced before the
/// sender policy runs, so a policy change since cannot fail it. The user's lock
/// is held from the capacity check through the insert, and the room's lock
/// from after that check as an ordinary send holds it, so room auth sees the
/// state the event would be sent against now.
#[tracing::instrument(level = "debug", skip_all)]
async fn schedule(
	services: &Services,
	Schedule {
		sender,
		device,
		room_id,
		kind,
		delay,
		txn_id,
		timestamp,
		stable,
	}: Schedule<'_>,
) -> Result<DelayId> {
	if !kind.is_own_call_leave(sender) {
		return Err!(Request(Forbidden("Only the sender's own call leave can be delayed.")));
	}

	let limits = services
		.delayed_events
		.limits()
		.ok_or_else(|| err!(Request(Forbidden("Delayed events are disabled on this server."))))?;

	let delay_ms = check_delay(delay, limits, stable)?;
	let event_type = kind.event_type();
	let txn = txn_id.map(|txn_id| RoomTxn {
		user_id: sender,
		device_id: device,
		txn_id,
		room_id,
	});

	let txn_type = event_type.to_cow_str();
	let existing = || {
		let lookup = txn.map(|txn| {
			services
				.delayed_events
				.existing_txn(txn, &txn_type)
		});

		OptionFuture::from(lookup)
	};

	if let Some(delay_id) = existing().await.transpose()?.flatten() {
		return Ok(delay_id);
	}

	let redacts = match kind {
		| Kind::Message { event_type, content } =>
			check_send_policy(services, sender, event_type, content).await?,
		| Kind::State { event_type, state_key, content } => {
			allowed_to_send_state_event(
				services, sender, room_id, event_type, state_key, content,
			)
			.await?;

			None
		},
	};

	let _user_lock = services.delayed_events.lock_user(sender).await;

	// A concurrent retry may have scheduled it while the policy ran.
	if let Some(delay_id) = existing().await.transpose()?.flatten() {
		return Ok(delay_id);
	}

	if let Kind::State { event_type, state_key, content } = kind
		&& *event_type == StateEventType::RoomMember
	{
		check_member_content_size(services, sender, room_id, state_key, content).await?;
	}

	services
		.delayed_events
		.check_capacity(sender, limits.max_scheduled)
		.await?;

	let state_lock = services.state.mutex.lock(room_id).await;

	if let Kind::Message { event_type, content } = kind {
		check_send_content(services, sender, room_id, event_type, content).await?;
	}

	let content = from_str(kind.json().get())
		.map_err(|e| err!(Request(BadJson("Invalid JSON body: {e}"))))?;

	let event = Event {
		user_id: sender.to_owned(),
		room_id: room_id.to_owned(),
		event_type,
		state_key: kind.state_key().map(Into::into),
		content,
		redacts,
		timestamp,
		delay_ms,
		due_ms: now_millis().saturating_add(delay_ms),
		finalised: None,
	};

	check_room_auth(services, &event, &state_lock).await?;

	Ok(services.delayed_events.schedule(&event, txn))
}

/// Returns the delay in milliseconds once it is positive and within limits.
///
/// An over-limit delay is refused with the stable or the unstable error code,
/// as the request's path calls for.
fn check_delay(delay: Duration, limits: Limits, stable: bool) -> Result<u64> {
	let delay_ms = millis(delay).unwrap_or(u64::MAX);

	if delay_ms == 0 {
		return Err!(Request(InvalidParam(
			"The delay must be a positive number of milliseconds."
		)));
	}

	if delay_ms <= limits.max_delay_ms {
		return Ok(delay_ms);
	}

	match stable {
		| true => Err!(Request(DelayTooLarge("The delay exceeds the maximum allowed."))),
		| false => Err!(Request(UnstableDelayTooLarge("The delay exceeds the maximum allowed."))),
	}
}

/// Whether this is the sender's own call leave, as a MatrixRTC client delays
/// it: a call membership state event with empty content, in the sender's own
/// slot.
///
/// A slot's state key is the sender's user ID, an underscore and the rest
/// (matrix-js-sdk appends the device ID and the call). matrix-js-sdk puts
/// another underscore in front, except in the MSC3757 and MSC3779 room
/// versions, whose auth rules keep such a key for that user. Room auth does
/// not tie the key with the underscore in front to its sender, so it is tied
/// here.
#[implement(Kind, generics = "<'a>", params = "<'a>")]
fn is_own_call_leave(self, sender: &UserId) -> bool {
	let Self::State { event_type, state_key, content } = self else {
		return false;
	};

	let own_slot = state_key
		.strip_prefix('_')
		.unwrap_or(state_key)
		.strip_prefix(sender.as_str())
		.is_some_and(|rest| rest.starts_with('_'));

	let leave =
		from_str::<JsonObject>(content.json().get()).is_ok_and(|content| content.is_empty());

	event_type.to_cow_str() == CALL_MEMBER && own_slot && leave
}

#[implement(Kind, generics = "<'a>", params = "<'a>")]
fn event_type(self) -> TimelineEventType {
	match self {
		| Self::Message { event_type, .. } => event_type.clone().into(),
		| Self::State { event_type, .. } => event_type.clone().into(),
	}
}

#[implement(Kind, generics = "<'a>", params = "<'a>")]
fn state_key(self) -> Option<&'a str> {
	match self {
		| Self::Message { .. } => None,
		| Self::State { state_key, .. } => Some(state_key),
	}
}

#[implement(Kind, generics = "<'a>", params = "<'a>")]
fn json(self) -> &'a RawJsonValue {
	match self {
		| Self::Message { content, .. } => content.json(),
		| Self::State { content, .. } => content.json(),
	}
}

/// Builds and signs the event against the room's current state without
/// storing it, refusing one room auth would reject now.
///
/// A change to the sender's own membership skips the check when the server
/// configuration says so.
async fn check_room_auth(
	services: &Services,
	event: &Event,
	state_lock: &RoomMutexGuard,
) -> Result {
	let own_membership = event.event_type == TimelineEventType::RoomMember
		&& event.state_key.as_deref() == Some(event.user_id.as_str());

	if own_membership
		&& !services
			.config
			.rooms
			.delayed_events
			.check_own_membership
	{
		return Ok(());
	}

	let pdu = PduBuilder {
		event_type: event.event_type.clone(),
		content: event.content.clone(),
		unsigned: None,
		state_key: event.state_key.clone(),
		redacts: event.redacts.clone(),
		timestamp: event.timestamp,
	};

	services
		.timeline
		.create_hash_and_sign_event(pdu, &event.user_id, &event.room_id, state_lock)
		.await
		.map(|_| ())
}

fn is_stable(path: &MatchedPath) -> bool {
	!path
		.as_str()
		.starts_with("/_matrix/client/unstable/")
}
