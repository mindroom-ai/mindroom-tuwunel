#![cfg(test)]

use serde_json::{Value, json};
use tuwunel_core::{
	Result, implement,
	ruma::{RoomId, UserId},
};
use tuwunel_service::Services;

use self::{
	client::{Client, register},
	fixture::boot,
};

mod client;
mod fixture;

const OWNER_TOKEN: &str = "knock-withdrawal-history-owner-access-token";
const MEMBER_TOKEN: &str = "knock-withdrawal-history-member-access-token";

/// A former member's history ends at the leave that ended its join.
///
/// A kicked member who knocks and withdraws the knock still reads what was
/// sent before and while it was joined, and its own join, but nothing sent
/// after the kick.
#[test]
fn departure_bounds_history() -> Result {
	let options: [&str; 0] = [];

	boot("knock-withdrawal-history", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	register(services, "knockowner", OWNER_TOKEN).await?;

	let member_id = register(services, "knockmember", MEMBER_TOKEN).await?;

	let owner = Client { services, base, token: OWNER_TOKEN };
	let member = Client { services, base, token: MEMBER_TOKEN };

	withdrawn_knock(&owner, &member, &member_id).await
}

async fn withdrawn_knock(owner: &Client<'_>, member: &Client<'_>, member_id: &UserId) -> Result {
	let room_id = owner
		.create_room(&json!({
			"preset": "private_chat",
			"initial_state": [{
				"type": "m.room.join_rules",
				"state_key": "",
				"content": { "join_rule": "knock" },
			}],
		}))
		.await?;

	owner.send_text(&room_id, "before-join").await?;

	let target = json!({ "user_id": member_id });

	owner
		.post(&format!("rooms/{room_id}/invite"), &target)
		.await?;

	member
		.post(&format!("rooms/{room_id}/join"), &json!({}))
		.await?;

	owner.send_text(&room_id, "while-joined").await?;
	owner
		.post(&format!("rooms/{room_id}/kick"), &target)
		.await?;

	owner.send_text(&room_id, "after-kick").await?;
	member
		.post(&format!("knock/{room_id}"), &json!({}))
		.await?;

	member
		.post(&format!("rooms/{room_id}/leave"), &json!({}))
		.await?;

	let messages = member
		.get(&format!("rooms/{room_id}/messages"), &[("dir", "b")])
		.await?;

	let bodies = bodies(&messages);
	let own_join = messages["chunk"]
		.as_array()
		.into_iter()
		.flatten()
		.any(|event| {
			event["state_key"] == member_id.as_str() && event["content"]["membership"] == "join"
		});

	assert!(bodies.contains(&"before-join"), "member loses its pre-join history: {bodies:?}");
	assert!(bodies.contains(&"while-joined"), "member loses its joined history: {bodies:?}");
	assert!(!bodies.contains(&"after-kick"), "member reads past its kick: {bodies:?}");
	assert!(own_join, "member loses its own join: {messages}");

	Ok(())
}

/// The message bodies in one `/messages` reply.
fn bodies(messages: &Value) -> Vec<&str> {
	messages["chunk"]
		.as_array()
		.into_iter()
		.flatten()
		.filter_map(|event| event.pointer("/content/body"))
		.filter_map(Value::as_str)
		.collect()
}

/// Send a text message, using its body as the transaction id.
#[implement(Client, params = "<'_>")]
async fn send_text(&self, room_id: &RoomId, body: &str) -> Result {
	self.services
		.client
		.clients
		.default
		.put(self.url(&format!("rooms/{room_id}/send/m.room.message/{body}")))
		.bearer_auth(self.token)
		.json(&json!({ "msgtype": "m.text", "body": body }))
		.send()
		.await?
		.error_for_status()?;

	Ok(())
}

/// Get one endpoint path with a query as this user and parse the reply.
#[implement(Client, params = "<'_>")]
async fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Value> {
	let response = self
		.services
		.client
		.clients
		.default
		.get(self.url(path))
		.bearer_auth(self.token)
		.query(query)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(response)
}
