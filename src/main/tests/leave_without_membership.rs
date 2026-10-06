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

const OWNER_TOKEN: &str = "leave-without-membership-owner-access-token";
const MEMBER_TOKEN: &str = "leave-without-membership-member-access-token";
const STRANGER_TOKEN: &str = "leave-without-membership-stranger-access-token";

/// A leave from a user with no membership to leave records no departure.
///
/// A stranger's leave must not list the room among its left rooms, and a
/// kicked member's leave must not move its departure past the kick; either
/// would serve history the room never shared with them.
#[test]
fn leave_without_membership_records_no_departure() -> Result {
	boot("leave-without-membership", ["client_sync_timeout_min=0"], exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	register(services, "leaveowner", OWNER_TOKEN).await?;
	register(services, "leavestranger", STRANGER_TOKEN).await?;

	let member_id = register(services, "leavemember", MEMBER_TOKEN).await?;

	let owner = Client { services, base, token: OWNER_TOKEN };
	let member = Client { services, base, token: MEMBER_TOKEN };
	let stranger = Client { services, base, token: STRANGER_TOKEN };

	let room_id = owner
		.create_room(&json!({ "preset": "private_chat" }))
		.await?;

	let invite = json!({ "user_id": member_id });

	owner
		.post(&format!("rooms/{room_id}/invite"), &invite)
		.await?;

	member.act(&room_id, "join").await?;
	owner
		.post(&format!("rooms/{room_id}/kick"), &invite)
		.await?;

	owner.send_text(&room_id, "after-kick").await?;
	stranger.act(&room_id, "leave").await?;
	member.act(&room_id, "leave").await?;

	let filter = json!({ "room": { "include_leave": true } }).to_string();
	let sync = stranger
		.get("sync", &[("filter", &filter), ("timeout", "0")])
		.await?;

	let stranger_left = sync["rooms"]["leave"]
		.get(room_id.as_str())
		.is_some();

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

	let member_reads_past_kick = bodies.contains(&"after-kick");

	assert!(!stranger_left, "stranger's sync lists the room as left: {sync}");
	assert!(!member_reads_past_kick, "kicked member reads past its kick: {bodies:?}");

	Ok(())
}

/// Apply a membership action to this user, such as a join or a leave.
#[implement(Client, params = "<'_>")]
async fn act(&self, room_id: &RoomId, action: &str) -> Result {
	self.post(&format!("rooms/{room_id}/{action}"), &json!({}))
		.await
		.map(drop)
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
