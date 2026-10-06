#![cfg(test)]

use serde_json::{Value, json};
use tuwunel_core::{Result, implement};
use tuwunel_service::Services;

use self::{
	client::{Client, field, register},
	fixture::boot,
};

mod client;
mod fixture;

const OWNER_TOKEN: &str = "sync-v5-departed-profiles-owner-access-token";
const MEMBER_TOKEN: &str = "sync-v5-departed-profiles-member-access-token";

const PROFILES: &str = "org.matrix.msc4262.profiles";

/// A room the requester has left carries no profile changes of its members.
///
/// A kicked member's connection keeps the room, first as the departure it
/// still owes and then as a room it once delivered, but the members' profile
/// changes are no longer theirs to follow.
#[test]
fn departed_room_carries_no_profile_changes() -> Result {
	let options: [&str; 0] = [];

	boot("sync-v5-departed-profiles", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let owner_id = register(services, "profilesowner", OWNER_TOKEN).await?;
	let member_id = register(services, "profilesmember", MEMBER_TOKEN).await?;

	let owner = Client { services, base, token: OWNER_TOKEN };
	let member = Client { services, base, token: MEMBER_TOKEN };

	let room_id = owner
		.create_room(&json!({ "preset": "private_chat" }))
		.await?;

	let target = json!({ "user_id": member_id });

	owner
		.post(&format!("rooms/{room_id}/invite"), &target)
		.await?;

	member
		.post(&format!("rooms/{room_id}/join"), &json!({}))
		.await?;

	let request = json!({
		"lists": { "all": { "ranges": [[0, 9]], "timeline_limit": 1 } },
		"extensions": { PROFILES: { "enabled": true } },
	});

	let joined = member.sliding_sync(None, &request).await?;

	owner
		.post(&format!("rooms/{room_id}/kick"), &target)
		.await?;

	let displayname = format!("profile/{owner_id}/displayname");

	owner
		.put(&displayname, &json!({ "displayname": "after-kick" }))
		.await?;

	let departed = member
		.sliding_sync(Some(field(&joined, "pos")?), &request)
		.await?;

	let room = room_id.as_str();
	let owner_key = owner_id.as_str();

	assert_eq!(
		departed["rooms"][room]["membership"], "leave",
		"departure missing from the response: {departed}"
	);
	assert!(
		departed["extensions"][PROFILES]["users"][owner_key].is_null(),
		"departing member follows the room's profiles: {departed}"
	);

	owner
		.put(&displayname, &json!({ "displayname": "later" }))
		.await?;

	let later = member
		.sliding_sync(Some(field(&departed, "pos")?), &request)
		.await?;

	assert!(later["rooms"][room].is_null(), "departure delivered again: {later}");
	assert!(
		later["extensions"][PROFILES]["users"][owner_key].is_null(),
		"departed member follows the room's profiles: {later}"
	);

	Ok(())
}

/// Post a JSON body to one endpoint path as this user and parse the reply.
#[implement(Client, params = "<'_>")]
async fn post(&self, path: &str, body: &Value) -> Result<Value> {
	self.post_url(&self.url(path), body).await
}

/// Put a JSON body to one endpoint path as this user and parse the reply.
#[implement(Client, params = "<'_>")]
async fn put(&self, path: &str, body: &Value) -> Result<Value> {
	let response = self
		.services
		.client
		.clients
		.default
		.put(self.url(path))
		.bearer_auth(self.token)
		.json(body)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(response)
}

/// Post one simplified sliding sync request as this user and parse the reply.
#[implement(Client, params = "<'_>")]
async fn sliding_sync(&self, pos: Option<&str>, body: &Value) -> Result<Value> {
	let pos = pos
		.map(|pos| format!("&pos={pos}"))
		.unwrap_or_default();

	let url = format!(
		"{}/_matrix/client/unstable/org.matrix.simplified_msc3575/sync?timeout=0{pos}",
		self.base
	);

	self.post_url(&url, body).await
}

/// Post a JSON body to a full URL as this user and parse the reply.
#[implement(Client, params = "<'_>")]
async fn post_url(&self, url: &str, body: &Value) -> Result<Value> {
	let response = self
		.services
		.client
		.clients
		.default
		.post(url)
		.bearer_auth(self.token)
		.json(body)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(response)
}
