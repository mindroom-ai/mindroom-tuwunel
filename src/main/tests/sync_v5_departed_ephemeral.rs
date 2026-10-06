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

const OWNER_TOKEN: &str = "sync-v5-departed-ephemeral-owner-access-token";
const MEMBER_TOKEN: &str = "sync-v5-departed-ephemeral-member-access-token";
const HELPER_TOKEN: &str = "sync-v5-departed-ephemeral-helper-access-token";

/// A room the requester has left carries no later receipts and no typing.
///
/// A kicked member may keep the room subscribed, which still delivers its
/// departure, but the members' read receipts sent after it, and their typing,
/// are no longer theirs to see, even after they knock again and withdraw it.
#[test]
fn departed_room_carries_no_receipts_or_typing() -> Result {
	let options: [&str; 0] = [];

	boot("sync-v5-departed-ephemeral", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let owner_id = register(services, "ephemeralowner", OWNER_TOKEN).await?;
	let member_id = register(services, "ephemeralmember", MEMBER_TOKEN).await?;

	let helper_id = register(services, "ephemeralhelper", HELPER_TOKEN).await?;

	let owner = Client { services, base, token: OWNER_TOKEN };
	let member = Client { services, base, token: MEMBER_TOKEN };
	let helper = Client { services, base, token: HELPER_TOKEN };

	withdrawn_knock(&owner, &member, &helper, [
		owner_id.as_str(),
		member_id.as_str(),
		helper_id.as_str(),
	])
	.await?;

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

	owner
		.post(&format!("rooms/{room_id}/kick"), &target)
		.await?;

	let text = json!({ "msgtype": "m.text", "body": "after-kick" });
	let message = owner
		.put(&format!("rooms/{room_id}/send/m.room.message/after-kick"), &text)
		.await?;

	let event_id = field(&message, "event_id")?;

	owner
		.post(&format!("rooms/{room_id}/receipt/m.read/{event_id}"), &json!({}))
		.await?;

	let typing = json!({ "typing": true, "timeout": 30000 });

	owner
		.put(&format!("rooms/{room_id}/typing/{owner_id}"), &typing)
		.await?;

	let request = json!({
		"room_subscriptions": { room_id.as_str(): { "timeline_limit": 0 } },
		"extensions": {
			"receipts": { "enabled": true },
			"typing": { "enabled": true },
		},
	});

	let response = member.sliding_sync(&request).await?;
	let room = room_id.as_str();

	assert_eq!(
		response["rooms"][room]["membership"], "leave",
		"departed room missing from the window: {response}"
	);
	assert!(
		response["extensions"]["receipts"]["rooms"][room].is_null(),
		"departed member reads receipts: {response}"
	);
	assert!(
		response["extensions"]["typing"]["rooms"][room].is_null(),
		"departed member sees typing: {response}"
	);

	Ok(())
}

/// A kicked member who knocks and withdraws the knock keeps the receipts sent
/// before the kick and gets none sent between the kick and the withdrawal.
async fn withdrawn_knock(
	owner: &Client<'_>,
	member: &Client<'_>,
	helper: &Client<'_>,
	[owner_id, member_id, helper_id]: [&str; 3],
) -> Result {
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

	owner
		.post(&format!("rooms/{room_id}/invite"), &json!({ "user_id": helper_id }))
		.await?;

	helper
		.post(&format!("rooms/{room_id}/join"), &json!({}))
		.await?;

	let text = json!({ "msgtype": "m.text", "body": "before-kick" });
	let message = owner
		.put(&format!("rooms/{room_id}/send/m.room.message/before-kick"), &text)
		.await?;

	let event_id = field(&message, "event_id")?;

	helper
		.post(&format!("rooms/{room_id}/receipt/m.read/{event_id}"), &json!({}))
		.await?;

	owner
		.post(&format!("rooms/{room_id}/kick"), &target)
		.await?;

	let text = json!({ "msgtype": "m.text", "body": "before-knock" });
	let message = owner
		.put(&format!("rooms/{room_id}/send/m.room.message/before-knock"), &text)
		.await?;

	let event_id = field(&message, "event_id")?;

	owner
		.post(&format!("rooms/{room_id}/receipt/m.read/{event_id}"), &json!({}))
		.await?;

	member
		.post(&format!("knock/{room_id}"), &json!({}))
		.await?;

	member
		.post(&format!("rooms/{room_id}/leave"), &json!({}))
		.await?;

	let request = json!({
		"room_subscriptions": { room_id.as_str(): { "timeline_limit": 0 } },
		"extensions": { "receipts": { "enabled": true } },
	});

	let response = member.sliding_sync(&request).await?;
	let room = room_id.as_str();

	assert_eq!(
		response["rooms"][room]["membership"], "leave",
		"withdrawn knock's room missing from the window: {response}"
	);
	let receipts = response["extensions"]["receipts"]["rooms"][room].to_string();

	assert!(
		receipts.contains(helper_id),
		"withdrawn knock loses the receipts from before the kick: {response}"
	);
	assert!(
		!receipts.contains(owner_id),
		"withdrawn knock reads receipts sent after the kick: {response}"
	);

	Ok(())
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
async fn sliding_sync(&self, body: &Value) -> Result<Value> {
	let url = format!(
		"{}/_matrix/client/unstable/org.matrix.simplified_msc3575/sync?timeout=0",
		self.base
	);

	let response = self
		.post_url(&url, body)
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(response)
}
