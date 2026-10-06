use ruma::{
	RoomId, RoomVersionId,
	api::federation::membership::{
		RawStrippedState, create_knock_event::v1::Response as SendKnockResponse,
	},
	events::StateEventType,
	room_id, user_id,
};
use serde_json::{Value, json, value::to_raw_value};
use tuwunel_core::{Result, config::Figment};

use crate::test_utils::fixture;

/// The knock responder's state cannot set a local user's membership.
///
/// Knock state is the answering server's unverified summary of the room, yet
/// it becomes the room's state here. A join it claims for a local user is
/// dropped, while its other state still installs.
#[tokio::test]
async fn knock_state_does_not_set_local_memberships() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let room_id = room_id!("!knock:remote.invalid");
	let alice = user_id!("@alice:localhost");
	let name = pdu(room_id, "$name", "m.room.name", "", &json!({ "name": "Knock" }))?;
	let join = pdu(
		room_id,
		"$join",
		"m.room.member",
		alice.as_str(),
		&json!({ "membership": "join" }),
	)?;
	let response = SendKnockResponse::new(vec![name, join]);
	let state_lock = services.state.mutex.lock(room_id).await;

	services
		.short
		.get_or_create_shortroomid(room_id)
		.await;

	let state_map = services
		.membership
		.ingest_send_knock_state(room_id, &response, &RoomVersionId::V11)
		.await?;

	services
		.membership
		.apply_send_knock_state(room_id, &state_map, &state_lock)
		.await?;

	services
		.state_accessor
		.room_state_get(room_id, &StateEventType::RoomName, "")
		.await?;

	assert!(
		!services
			.state_cache
			.is_joined(alice, room_id)
			.await
	);

	Ok(())
}

/// A full PDU as the answering server would send it, with nothing checked.
///
/// It carries an `event_id`, which the stored outlier needs to load as a PDU.
fn pdu(
	room_id: &RoomId,
	event_id: &str,
	kind: &str,
	state_key: &str,
	content: &Value,
) -> Result<RawStrippedState> {
	let event = json!({
		"event_id": event_id,
		"type": kind,
		"state_key": state_key,
		"content": content,
		"room_id": room_id,
		"sender": "@bob:remote.invalid",
		"origin_server_ts": 1,
		"depth": 1,
		"hashes": { "sha256": "unchecked" },
		"prev_events": [],
		"auth_events": [],
	});

	Ok(RawStrippedState::Pdu(to_raw_value(&event)?))
}
