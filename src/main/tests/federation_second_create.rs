#![cfg(test)]

use serde_json::json;
use tuwunel_core::{
	Err, Result,
	matrix::pdu::into_outgoing_federation,
	ruma::{MilliSecondsSinceUnixEpoch, events::StateEventType},
	utils::to_canonical_object,
};
use tuwunel_service::Services;

use self::{
	client::{Client, register},
	fixture::boot,
};

mod client;
mod fixture;

const TOKEN: &str = "federation-second-create-access-token";

/// A room has one create event. Another create for the room, from the server
/// named in the room ID, is refused: it does not reach the timeline and the
/// room's create stays in its current state.
#[test]
fn second_create_is_refused() -> Result {
	let options: [&str; 0] = [];

	boot("federation-second-create", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let user_id = register(services, "secondcreate", TOKEN).await?;
	let client = Client { services, base, token: TOKEN };
	let room_id = client
		.create_room(&json!({"room_version": "10"}))
		.await?;

	let create_id = services
		.state_accessor
		.room_state_get_id(&room_id, &StateEventType::RoomCreate, "")
		.await?;

	let room_version = services.state.get_room_version(&room_id).await?;
	let mut pdu = to_canonical_object(json!({
		"type": "m.room.create",
		"state_key": "",
		"room_id": room_id,
		"sender": user_id,
		"origin_server_ts": MilliSecondsSinceUnixEpoch::now(),
		"depth": 1,
		"prev_events": [],
		"auth_events": [],
		"content": {"creator": user_id, "room_version": room_version},
	}))?;

	let event_id = services
		.server_keys
		.gen_id_hash_and_sign_event(&mut pdu, &room_version)?;

	let result = services
		.event_handler
		.handle_incoming_pdu(
			services.globals.server_name(),
			&room_id,
			&event_id,
			into_outgoing_federation(pdu, &room_version),
			true,
		)
		.await;

	let current_id = services
		.state_accessor
		.room_state_get_id(&room_id, &StateEventType::RoomCreate, "")
		.await?;

	assert_eq!(current_id, create_id, "the room's create was replaced");
	assert!(
		services
			.timeline
			.non_outlier_pdu_exists(&event_id)
			.await
			.is_err(),
		"the second create reached the timeline"
	);

	let Err(error) = result else {
		return Err!("the second create was accepted");
	};

	assert!(
		error
			.to_string()
			.contains("different create event"),
		"refused for another reason: {error}"
	);

	Ok(())
}
