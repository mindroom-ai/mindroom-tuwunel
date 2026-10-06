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
const STRANGER_TOKEN: &str = "context-snapshotless-state-stranger-access-token";

/// Context around an event without a state snapshot keeps the room's current
/// state from a requester who may not read it.
///
/// The create event has no snapshot of its own, so the current state stands in
/// for it, which a member still receives and a stranger does not.
#[test]
fn snapshotless_context_withholds_current_state() -> Result {
	let options: [&str; 0] = [];

	boot("context-snapshotless-state", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	register(services, "contextowner", OWNER_TOKEN).await?;
	register(services, "contextstranger", STRANGER_TOKEN).await?;

	let owner = Client { services, base, token: OWNER_TOKEN };
	let stranger = Client { services, base, token: STRANGER_TOKEN };

	let room_id = owner
		.create_room(&json!({ "preset": "private_chat", "topic": "private topic" }))
		.await?;

	let create_id = services
		.state_accessor
		.room_state_get_id(&room_id, &StateEventType::RoomCreate, "")
		.await?;

	let member = owner.context(&room_id, &create_id).await?;

	assert!(!state(&member).is_empty(), "member lost the context state: {member}");

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

/// The state events a context response carries.
fn state(response: &Value) -> &[Value] {
	response["state"]
		.as_array()
		.map(Vec::as_slice)
		.unwrap_or_default()
}
