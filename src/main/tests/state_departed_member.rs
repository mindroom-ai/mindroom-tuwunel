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
/// at the kick, while the owner reads the current state. An `at` token on
/// `/members` reads that token's snapshot, except that a former member reads
/// no later than their departure.
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

	let joined_at = services.globals.current_count();
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

	let membership = |user_id: &UserId| membership_in(&members, user_id);
	let latest_at = services.globals.current_count();
	let members_at = async |client: &Client<'_>, at: u64| {
		client
			.get(&format!("rooms/{room_id}/members?at={at}"))
			.await
	};

	let member_before = members_at(&member, joined_at).await?;
	let member_latest = members_at(&member, latest_at).await?;
	let owner_before = members_at(&owner, joined_at).await?;
	let owner_now = owner
		.get(&format!("rooms/{room_id}/members"))
		.await?;

	assert_eq!(
		membership_in(&member_before, &member_id),
		Some("join"),
		"a token before the kick reads that snapshot: {member_before}"
	);
	assert_eq!(
		membership_in(&member_latest, &member_id),
		Some("leave"),
		"a token after the kick reads the departure: {member_latest}"
	);
	assert_eq!(
		membership_in(&member_latest, &newcomer_id),
		None,
		"a token after the kick shows a later invite: {member_latest}"
	);
	assert_eq!(
		membership_in(&owner_before, &newcomer_id),
		None,
		"the owner's earlier token shows a later invite: {owner_before}"
	);
	assert_eq!(
		membership_in(&owner_before, &member_id),
		Some("join"),
		"the owner's earlier token lacks the member's join: {owner_before}"
	);
	assert_eq!(
		membership_in(&owner_now, &newcomer_id),
		Some("invite"),
		"the owner's current members lack the invite: {owner_now}"
	);

	assert_eq!(member_name["name"], "before", "kicked member reads the new name");
	assert_eq!(names, ["before"], "kicked member's state has the new name: {state}");
	assert_eq!(membership(&member_id), Some("leave"), "members lack the kick: {members}");
	assert_eq!(membership(&newcomer_id), None, "members show a later invite: {members}");
	assert_eq!(owner_name["name"], "after", "owner reads the old name");

	Ok(())
}

/// The membership a `/members` response gives a user, if it lists them.
fn membership_in<'a>(members: &'a Value, user_id: &UserId) -> Option<&'a str> {
	events(&members["chunk"])
		.find(|event| event["state_key"] == user_id.as_str())
		.and_then(|event| event["content"]["membership"].as_str())
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
