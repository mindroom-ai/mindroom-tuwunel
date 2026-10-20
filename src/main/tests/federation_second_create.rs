#![cfg(test)]

use serde_json::{Value, json};
use tuwunel_core::{
	Err, Result,
	matrix::pdu::into_outgoing_federation,
	ruma::{
		CanonicalJsonObject, EventId, MilliSecondsSinceUnixEpoch, OwnedEventId, RoomId,
		RoomVersionId, events::StateEventType,
	},
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
/// named in the room ID, is refused: it is not stored, it does not reach the
/// timeline and the room's create stays in its current state. An event naming
/// another create as an auth event is refused too, even when that create is
/// already stored.
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
	let create = json!({
		"type": "m.room.create",
		"state_key": "",
		"room_id": room_id,
		"sender": user_id,
		"origin_server_ts": MilliSecondsSinceUnixEpoch::now(),
		"depth": 1,
		"prev_events": [],
		"auth_events": [],
		"content": {"creator": user_id, "room_version": room_version},
	});

	let (event_id, pdu) = sign(services, &room_version, create)?;

	let result = handle(services, &room_id, &event_id, pdu.clone(), &room_version, true).await;

	let current_id = services
		.state_accessor
		.room_state_get_id(&room_id, &StateEventType::RoomCreate, "")
		.await?;

	assert_eq!(current_id, create_id, "the room's create was replaced");
	assert!(!services.timeline.pdu_exists(&event_id).await, "the second create was stored");
	check_refused(result)?;

	// A second create stored before it was refused authorizes no event naming it.
	services.timeline.add_pdu_outlier(&event_id, &pdu);

	let join = json!({
		"type": "m.room.member",
		"state_key": user_id,
		"room_id": room_id,
		"sender": user_id,
		"origin_server_ts": MilliSecondsSinceUnixEpoch::now(),
		"depth": 2,
		"prev_events": [event_id],
		"auth_events": [event_id],
		"content": {"membership": "join"},
	});

	let (join_id, join) = sign(services, &room_version, join)?;

	let result = handle(services, &room_id, &join_id, join, &room_version, false).await;

	assert!(
		!services.timeline.pdu_exists(&join_id).await,
		"an event naming the second create was stored"
	);

	check_refused(result)
}

fn sign(
	services: &Services,
	room_version: &RoomVersionId,
	event: Value,
) -> Result<(OwnedEventId, CanonicalJsonObject)> {
	let mut pdu = to_canonical_object(event)?;
	let event_id = services
		.server_keys
		.gen_id_hash_and_sign_event(&mut pdu, room_version)?;

	Ok((event_id, pdu))
}

async fn handle(
	services: &Services,
	room_id: &RoomId,
	event_id: &EventId,
	pdu: CanonicalJsonObject,
	room_version: &RoomVersionId,
	is_timeline_event: bool,
) -> Result {
	services
		.event_handler
		.handle_incoming_pdu(
			services.globals.server_name(),
			room_id,
			event_id,
			into_outgoing_federation(pdu, room_version),
			is_timeline_event,
		)
		.await
		.map(|_| ())
}

fn check_refused(result: Result) -> Result {
	let Err(error) = result else {
		return Err!("the event was accepted");
	};

	assert!(
		error
			.to_string()
			.contains("different create event"),
		"refused for another reason: {error}"
	);

	Ok(())
}
