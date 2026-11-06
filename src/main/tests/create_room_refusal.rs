#![cfg(test)]

use std::collections::BTreeSet;

use futures::StreamExt;
use reqwest::StatusCode;
use serde_json::{Value, json};
use tuwunel_core::{
	Result,
	ruma::{OwnedRoomId, RoomId},
};
use tuwunel_service::Services;

use self::{
	client::{Client, register},
	fixture::boot,
};

mod client;
mod fixture;

const TOKEN: &str = "create-room-refusal-access-token";

const OTHER: &str = "@other:example.org";

/// A `createRoom` the server refuses leaves no room behind.
///
/// Room version 12, the default, gives its creators unlimited power, and its
/// authorization rules refuse power levels that list a creator in `users`.
/// A request whose `power_level_content_override` lists one is refused before
/// the room exists. A request that fails at a later event, here a power levels
/// event in `initial_state` that lists the creator, takes the room it began
/// with it, so the creator has no room to find in either case.
#[test]
fn a_refused_create_room_leaves_no_room() -> Result {
	let options: [&str; 0] = [];

	boot("create-room-refusal", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let creator = register(services, "refusedcreator", TOKEN).await?;
	let client = Client { services, base, token: TOKEN };

	let created = client
		.create_room(&json!({
			"power_level_content_override": { "users": { OTHER: 50 } },
		}))
		.await?;

	let rooms = room_ids(services).await;

	let listing_creators = [
		json!({
			"power_level_content_override": { "users": { creator.as_str(): 100 } },
		}),
		json!({
			"creation_content": { "additional_creators": [OTHER] },
			"power_level_content_override": { "users": { OTHER: 100 } },
		}),
	];

	for body in &listing_creators {
		let response = client
			.post_url(&client.url("createRoom"), body)
			.await?;

		assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
		assert_eq!(room_ids(services).await, rooms, "a refused request made a room: {body}");

		let error: Value = response.json().await?;

		assert_eq!(error["errcode"], "M_INVALID_ROOM_STATE", "{error}");
	}

	let failing_later = json!({
		"initial_state": [{
			"type": "m.room.power_levels",
			"state_key": "",
			"content": { "users": { creator.as_str(): 100 } },
		}],
	});

	let response = client
		.post_url(&client.url("createRoom"), &failing_later)
		.await?;

	assert!(response.status().is_client_error(), "{}", response.status());
	assert_eq!(room_ids(services).await, rooms, "a failed request left its room");

	let joined: Vec<OwnedRoomId> = services
		.state_cache
		.rooms_joined(&creator)
		.map(RoomId::to_owned)
		.collect()
		.await;

	let left: Vec<OwnedRoomId> = services
		.state_cache
		.rooms_left(&creator)
		.map(RoomId::to_owned)
		.collect()
		.await;

	assert_eq!(joined, [created], "the creator is in a room it did not get");
	assert!(left.is_empty(), "the creator left rooms it did not get: {left:?}");

	Ok(())
}

async fn room_ids(services: &Services) -> BTreeSet<OwnedRoomId> {
	services
		.metadata
		.iter_ids()
		.map(RoomId::to_owned)
		.collect()
		.await
}
