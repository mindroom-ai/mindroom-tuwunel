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

const CREATOR_TOKEN: &str = "federation-local-join-creator-token";

const MEMBER_TOKEN: &str = "federation-local-join-member-token";

/// A local user's join is sent by that user.
///
/// The authorization rules let anyone send the join of a create's creator when
/// that create is its only previous event: the create's `creator` up to room
/// version 10, its sender since room version 11. Such a join sent by another
/// user, even with content the rules do not read, does not join the creator
/// again after they left, and the creator can still join themselves.
#[test]
fn local_join_by_another_sender_is_refused() -> Result {
	let options: [&str; 0] = [];

	boot("federation-local-join", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let creator_id = register(services, "creator", CREATOR_TOKEN).await?;
	let member_id = register(services, "member", MEMBER_TOKEN).await?;
	let creator = Client { services, base, token: CREATOR_TOKEN };
	let member = Client { services, base, token: MEMBER_TOKEN };
	let room_id = creator
		.create_room(&json!({"room_version": "10", "preset": "public_chat"}))
		.await?;

	member
		.post(&format!("rooms/{room_id}/join"), &json!({}))
		.await?;

	creator
		.post(&format!("rooms/{room_id}/leave"), &json!({}))
		.await?;

	let create_id = services
		.state_accessor
		.room_state_get_id(&room_id, &StateEventType::RoomCreate, "")
		.await?;

	let room_version = services.state.get_room_version(&room_id).await?;
	let mut pdu = to_canonical_object(json!({
		"type": "m.room.member",
		"state_key": creator_id,
		"room_id": room_id,
		"sender": member_id,
		"origin_server_ts": MilliSecondsSinceUnixEpoch::now(),
		"depth": 2,
		"prev_events": [create_id],
		"auth_events": [create_id],
		"content": {"membership": "join", "displayname": 7},
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

	assert!(
		!services
			.state_cache
			.is_joined(&creator_id, &room_id)
			.await,
		"the creator was joined again"
	);
	assert!(!services.timeline.pdu_exists(&event_id).await, "the join was stored");

	let Err(error) = result else {
		return Err!("the join was accepted");
	};

	assert!(
		error
			.to_string()
			.contains("must be sent by that user"),
		"refused for another reason: {error}"
	);

	creator
		.post(&format!("rooms/{room_id}/join"), &json!({}))
		.await?;

	assert!(
		services
			.state_cache
			.is_joined(&creator_id, &room_id)
			.await,
		"the creator could not join again"
	);

	Ok(())
}
