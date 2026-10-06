#![cfg(test)]

use serde_json::{Value, json};
use tuwunel_core::{Result, implement, ruma::RoomId};
use tuwunel_service::Services;

use self::{
	client::{Client, register},
	fixture::boot,
};

mod client;
mod fixture;

const OWNER_TOKEN: &str = "knock-withdrawal-history-owner-access-token";
const MEMBER_TOKEN: &str = "knock-withdrawal-history-member-access-token";

/// A withdrawn knock does not move a former member's departure forward.
///
/// Knocking replaces a kicked member's leave row, and withdrawing the knock
/// writes a new one; the member still reads what it was joined for, but
/// nothing sent after its kick.
#[test]
fn withdrawn_knock_keeps_departure() -> Result {
	let options: [&str; 0] = [];

	boot("knock-withdrawal-history", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	register(services, "knockowner", OWNER_TOKEN).await?;

	let member_id = register(services, "knockmember", MEMBER_TOKEN).await?;

	let owner = Client { services, base, token: OWNER_TOKEN };
	let member = Client { services, base, token: MEMBER_TOKEN };

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

	let bodies: Vec<_> = messages["chunk"]
		.as_array()
		.into_iter()
		.flatten()
		.filter_map(|event| event.pointer("/content/body"))
		.filter_map(Value::as_str)
		.collect();

	assert!(bodies.contains(&"while-joined"), "member loses its joined history: {bodies:?}");
	assert!(!bodies.contains(&"after-kick"), "member reads past its kick: {bodies:?}");

	Ok(())
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

/// Post a JSON body to one endpoint path as this user and parse the reply.
///
/// A non-success status is the error, so a caller only ever sees the body of
/// an accepted request.
#[implement(Client, params = "<'_>")]
async fn post(&self, path: &str, body: &Value) -> Result<Value> {
	let response = self
		.services
		.client
		.clients
		.default
		.post(self.url(path))
		.bearer_auth(self.token)
		.json(body)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(response)
}
