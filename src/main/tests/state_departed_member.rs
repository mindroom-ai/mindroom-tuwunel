#![cfg(test)]

use reqwest::Method;
use serde_json::{Value, json};
use tuwunel_core::{Result, implement, ruma::UserId};
use tuwunel_service::Services;

use self::{
	client::{Client, register},
	fixture::boot,
};

mod client;
mod fixture;

const OWNER_TOKEN: &str = "state-departed-member-owner-access-token";
const MEMBER_TOKEN: &str = "state-departed-member-member-access-token";
const NEWCOMER_TOKEN: &str = "state-departed-member-newcomer-access-token";

/// A former member reads the room state from when they left.
///
/// After the member is kicked, the room is renamed and another user invited.
/// The kicked member's `/state` and `/members` still show the room as it was
/// at the kick, while the owner reads the current state.
#[test]
fn departed_member_reads_state_at_departure() -> Result {
	let options: [&str; 0] = [];

	boot("state-departed-member", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	register(services, "stateowner", OWNER_TOKEN).await?;

	let member_id = register(services, "statemember", MEMBER_TOKEN).await?;
	let newcomer_id = register(services, "statenewcomer", NEWCOMER_TOKEN).await?;

	let owner = Client { services, base, token: OWNER_TOKEN };
	let member = Client { services, base, token: MEMBER_TOKEN };

	let room_id = owner
		.create_room(&json!({ "preset": "private_chat", "name": "before" }))
		.await?;

	let member_target = json!({ "user_id": member_id });
	let newcomer_target = json!({ "user_id": newcomer_id });

	owner
		.send(Method::POST, &format!("rooms/{room_id}/invite"), &member_target)
		.await?;
	member
		.send(Method::POST, &format!("rooms/{room_id}/join"), &json!({}))
		.await?;
	owner
		.send(Method::POST, &format!("rooms/{room_id}/kick"), &member_target)
		.await?;

	let name_path = format!("rooms/{room_id}/state/m.room.name/");

	owner
		.send(Method::PUT, &name_path, &json!({ "name": "after" }))
		.await?;
	owner
		.send(Method::POST, &format!("rooms/{room_id}/invite"), &newcomer_target)
		.await?;

	let member_name = member.get(&name_path).await?;
	let owner_name = owner.get(&name_path).await?;

	let state = member
		.get(&format!("rooms/{room_id}/state"))
		.await?;
	let names: Vec<_> = events(&state)
		.filter(|event| event["type"] == "m.room.name")
		.filter_map(|event| event["content"]["name"].as_str())
		.collect();

	let members = member
		.get(&format!("rooms/{room_id}/members"))
		.await?;

	let membership = |user_id: &UserId| {
		events(&members["chunk"])
			.find(|event| event["state_key"] == user_id.as_str())
			.and_then(|event| event["content"]["membership"].as_str())
	};

	assert_eq!(member_name["name"], "before", "kicked member reads the new name");
	assert_eq!(names, ["before"], "kicked member's state has the new name: {state}");
	assert_eq!(membership(&member_id), Some("leave"), "members lack the kick: {members}");
	assert_eq!(membership(&newcomer_id), None, "members show a later invite: {members}");
	assert_eq!(owner_name["name"], "after", "owner reads the old name");

	Ok(())
}

/// The events of a JSON array, or none for anything else.
fn events(value: &Value) -> impl Iterator<Item = &Value> {
	value.as_array().into_iter().flatten()
}

/// Get one endpoint path as this user and parse the reply.
#[implement(Client, params = "<'_>")]
async fn get(&self, path: &str) -> Result<Value> {
	let response = self
		.services
		.client
		.clients
		.default
		.get(self.url(path))
		.bearer_auth(self.token)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(response)
}

/// Send a JSON body to one endpoint path as this user.
///
/// A non-success status is the error, so the exercise stops at the first
/// request the server refuses.
#[implement(Client, params = "<'_>")]
async fn send(&self, method: Method, path: &str, body: &Value) -> Result {
	self.services
		.client
		.clients
		.default
		.request(method, self.url(path))
		.bearer_auth(self.token)
		.json(body)
		.send()
		.await?
		.error_for_status()?;

	Ok(())
}
