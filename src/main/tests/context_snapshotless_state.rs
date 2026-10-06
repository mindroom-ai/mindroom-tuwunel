#![cfg(test)]

use serde_json::{Value, json};
use tuwunel_core::{
	Result, implement,
	ruma::{EventId, RoomId, events::StateEventType},
};
use tuwunel_service::Services;

use self::{
	client::{Client, register},
	fixture::boot,
};

mod client;
mod fixture;

const OWNER_TOKEN: &str = "context-snapshotless-state-owner-access-token";
const MEMBER_TOKEN: &str = "context-snapshotless-state-member-access-token";
const STRANGER_TOKEN: &str = "context-snapshotless-state-stranger-access-token";

/// Context around an event without a state snapshot keeps the room's current
/// state from a requester who may not read it.
///
/// The create event has no snapshot of its own, so the current state stands in
/// for it, which a member still receives and a stranger does not. A kicked
/// member receives the state from when they were kicked.
#[test]
fn snapshotless_context_withholds_current_state() -> Result {
	let options: [&str; 0] = [];

	boot("context-snapshotless-state", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	register(services, "contextowner", OWNER_TOKEN).await?;
	register(services, "contextstranger", STRANGER_TOKEN).await?;

	let member_id = register(services, "contextmember", MEMBER_TOKEN).await?;

	let owner = Client { services, base, token: OWNER_TOKEN };
	let member = Client { services, base, token: MEMBER_TOKEN };
	let stranger = Client { services, base, token: STRANGER_TOKEN };

	let room_id = owner
		.create_room(&json!({ "preset": "private_chat", "topic": "private topic" }))
		.await?;

	let target = json!({ "user_id": member_id });

	owner
		.post(&format!("rooms/{room_id}/invite"), &target)
		.await?;
	member
		.post(&format!("rooms/{room_id}/join"), &json!({}))
		.await?;
	owner
		.post(&format!("rooms/{room_id}/kick"), &target)
		.await?;
	owner.set_topic(&room_id, "later topic").await?;

	let create_id = services
		.state_accessor
		.room_state_get_id(&room_id, &StateEventType::RoomCreate, "")
		.await?;

	let current = owner.context(&room_id, &create_id).await?;

	assert_eq!(topic(&current), Some("later topic"), "owner lost the current state: {current}");

	let departed = member.context(&room_id, &create_id).await?;

	assert_eq!(
		topic(&departed),
		Some("private topic"),
		"kicked member reads later state: {departed}"
	);

	let response = stranger.context(&room_id, &create_id).await?;

	assert!(state(&response).is_empty(), "stranger reads the room state: {response}");

	Ok(())
}

/// Get the context of one event, with no surrounding events, as this user.
#[implement(Client, params = "<'_>")]
async fn context(&self, room_id: &RoomId, event_id: &EventId) -> Result<Value> {
	let response = self
		.services
		.client
		.clients
		.default
		.get(self.url(&format!("rooms/{room_id}/context/{event_id}")))
		.bearer_auth(self.token)
		.query(&[("limit", "0")])
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(response)
}

/// Set the room topic as this user.
#[implement(Client, params = "<'_>")]
async fn set_topic(&self, room_id: &RoomId, topic: &str) -> Result {
	self.services
		.client
		.clients
		.default
		.put(self.url(&format!("rooms/{room_id}/state/m.room.topic/")))
		.bearer_auth(self.token)
		.json(&json!({ "topic": topic }))
		.send()
		.await?
		.error_for_status()?;

	Ok(())
}

/// The room topic among the state events a context response carries.
fn topic(response: &Value) -> Option<&str> {
	state(response)
		.iter()
		.find(|event| event["type"] == "m.room.topic")
		.and_then(|event| event["content"]["topic"].as_str())
}

/// The state events a context response carries.
fn state(response: &Value) -> &[Value] {
	response["state"]
		.as_array()
		.map(Vec::as_slice)
		.unwrap_or_default()
}
