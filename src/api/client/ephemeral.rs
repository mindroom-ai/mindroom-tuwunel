use std::time::Duration;

use axum::{Json, extract::State};
use http::StatusCode;
use ruma::api::{
	client::message::send_message_event,
	error::{ErrorKind, LimitExceededErrorData, RetryAfter},
};
use serde_json::{Map, Value, json};
use tuwunel_core::{Err, Error, Result};
use tuwunel_service::rooms::ephemeral::SendOutcome;

use crate::Ruma;

/// Largest body, in bytes, one ephemeral event may carry.
const MAX_BODY_BYTES: usize = 65_536;

/// Longest event type, in bytes, one ephemeral event may have.
const MAX_EVENT_TYPE_BYTES: usize = 255;

/// # `PUT /_matrix/client/unstable/org.matrix.msc2477/rooms/{roomId}/ephemeral/{eventType}/{txnId}`
///
/// Sends a custom ephemeral event to the room's members whose `/sync` opts
/// into its type (MSC2477). The event is held in memory only and never
/// federated. The path and body match `/send`, whose request type this reuses.
pub(crate) async fn send_ephemeral_route(
	State(services): State<crate::State>,
	body: Ruma<send_message_event::v3::Request>,
) -> Result<Json<Value>> {
	if !services.config.mindroom_ephemeral_events_enabled {
		return Err!(Request(Unrecognized("Unrecognized request")));
	}

	let event_type = body.event_type.to_string();
	if event_type.len() > MAX_EVENT_TYPE_BYTES {
		return Err!(Request(InvalidParam("Ephemeral event type is longer than 255 bytes.")));
	}

	if event_type.starts_with("m.") {
		return Err!(Request(Unknown("Cannot send built-in ephemeral types with this endpoint")));
	}

	let sender_user = body.sender_user();
	if !services
		.state_cache
		.is_joined(sender_user, &body.room_id)
		.await
	{
		return Err!(Request(Forbidden("You are not in this room.")));
	}

	let content = body.body.body.json();
	if content.get().len() > MAX_BODY_BYTES {
		return Err!(Request(TooLarge("Ephemeral event body is larger than 65536 bytes.")));
	}

	if body
		.body
		.body
		.deserialize_as::<Map<String, Value>>()
		.is_err()
	{
		return Err!(Request(BadJson("Ephemeral event body must be a JSON object.")));
	}

	let outcome = services.ephemeral.send(
		&body.room_id,
		sender_user,
		body.sender_device.as_deref(),
		&event_type,
		&body.txn_id,
		content.to_owned(),
	);

	match outcome {
		| SendOutcome::Accepted | SendOutcome::Duplicate => Ok(Json(json!({}))),
		| SendOutcome::RateLimited { retry_after_ms } => Err(Error::Request(
			ErrorKind::LimitExceeded(LimitExceededErrorData {
				retry_after: Some(RetryAfter::Delay(Duration::from_millis(retry_after_ms))),
			}),
			"Too many ephemeral events in this room.".into(),
			StatusCode::TOO_MANY_REQUESTS,
		)),
	}
}
