#![cfg(test)]

use serde_json::{json, value::to_raw_value};
use tuwunel_core::{
	Result,
	matrix::{StateKey, pdu::PduBuilder},
	ruma::{
		MilliSecondsSinceUnixEpoch, OwnedRoomId,
		events::{StateEventType, TimelineEventType},
		room_id,
	},
};
use tuwunel_service::Services;

use self::{client::register, fixture::boot};

#[expect(dead_code)] // Only registration is used from the client harness.
mod client;
mod fixture;

const TOKEN: &str = "create-room-same-millisecond-access-token";

/// A room version 12 room's ID is the hash of its create event, so one user
/// creating two rooms with the same create content in the same millisecond
/// would get one ID for both. Each create, built as `createRoom` builds it,
/// makes its own room, and the first room keeps its create event.
#[test]
fn same_millisecond_creates_make_distinct_rooms() -> Result {
	let options: [&str; 0] = [];

	boot("create-room-same-millisecond", options, exercise)
}

async fn exercise(services: &Services, _base: &str) -> Result {
	let user_id = register(services, "samemillisecond", TOKEN).await?;
	let placeholder = room_id!("!thiswillbereplaced");
	let timestamp = MilliSecondsSinceUnixEpoch::now();

	let mut create_ids = Vec::new();
	for _ in 0..2 {
		let state_lock = services.state.mutex.lock(placeholder).await;
		let create_id = services
			.timeline
			.build_and_append_pdu(
				PduBuilder {
					event_type: TimelineEventType::RoomCreate,
					content: to_raw_value(&json!({"room_version": "12"}))?.into(),
					state_key: Some(StateKey::new()),
					timestamp: Some(timestamp),
					..Default::default()
				},
				&user_id,
				placeholder,
				&state_lock,
			)
			.await?;

		create_ids.push(create_id);
	}

	assert_ne!(create_ids[0], create_ids[1], "both creates made the same room");

	for create_id in &create_ids {
		let room_id = OwnedRoomId::from_parts('!', create_id.localpart(), None)?;
		let current_id = services
			.state_accessor
			.room_state_get_id(&room_id, &StateEventType::RoomCreate, "")
			.await?;

		assert_eq!(&current_id, create_id, "a room's create was replaced");
	}

	Ok(())
}
