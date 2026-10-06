#![cfg(test)]

use serde_json::{Value, json};
use tuwunel_core::{Result, err, implement, ruma::RoomId};
use tuwunel_service::Services;

use self::{
	client::{Client, register},
	fixture::boot,
};

mod client;
mod fixture;

const OWNER_TOKEN: &str = "sync-left-room-state-owner-access-token";
const KNOCKER_TOKEN: &str = "sync-left-room-state-knocker-access-token";
const INVITEE_TOKEN: &str = "sync-left-room-state-invitee-access-token";
const MEMBER_TOKEN: &str = "sync-left-room-state-member-access-token";

/// A left room carries the room's state only for a user who once joined it.
///
/// A knocker who withdraws and an invitee who rejects were never admitted, so
/// their left room carries no state beyond their own membership, while a
/// member who joined and left still gets the room's state.
#[test]
fn left_room_state_requires_a_join() -> Result {
	boot("sync-left-room-state", ["client_sync_timeout_min=0"], exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	register(services, "leftstateowner", OWNER_TOKEN).await?;

	let knocker_id = register(services, "leftstateknocker", KNOCKER_TOKEN).await?;
	let invitee_id = register(services, "leftstateinvitee", INVITEE_TOKEN).await?;
	let member_id = register(services, "leftstatemember", MEMBER_TOKEN).await?;

	let owner = Client { services, base, token: OWNER_TOKEN };
	let knocker = Client { services, base, token: KNOCKER_TOKEN };
	let invitee = Client { services, base, token: INVITEE_TOKEN };
	let member = Client { services, base, token: MEMBER_TOKEN };

	let room_id = owner
		.create_room(&json!({
			"preset": "private_chat",
			"topic": "members-only",
			"initial_state": [{
				"type": "m.room.join_rules",
				"state_key": "",
				"content": { "join_rule": "knock" },
			}],
		}))
		.await?;

	knocker
		.post(&format!("knock/{room_id}"), &json!({}))
		.await?;

	knocker.act(&room_id, "leave").await?;

	for user_id in [&invitee_id, &member_id] {
		owner
			.post(&format!("rooms/{room_id}/invite"), &json!({ "user_id": user_id }))
			.await?;
	}

	invitee.act(&room_id, "leave").await?;
	member.act(&room_id, "join").await?;
	member.act(&room_id, "leave").await?;

	for (user, user_id) in [(&knocker, &knocker_id), (&invitee, &invitee_id)] {
		let state = user.left_room_state(&room_id).await?;
		let foreign: Vec<_> = state
			.iter()
			.filter(|event| event["state_key"] != user_id.as_str())
			.collect();

		assert!(foreign.is_empty(), "{user_id} reads the room's state: {foreign:?}");
	}

	let state = member.left_room_state(&room_id).await?;
	let topic = state
		.iter()
		.any(|event| event["type"] == "m.room.topic");

	assert!(topic, "{member_id} lacks the room's state: {state:?}");

	Ok(())
}

/// The state events of a left room in this user's initial `include_leave` sync.
///
/// The timeline holds only the newest event, the user's own leave, so the state
/// covers everything before it. A room missing from the left rooms is an error,
/// not an empty state.
#[implement(Client, params = "<'_>")]
async fn left_room_state(&self, room_id: &RoomId) -> Result<Vec<Value>> {
	let filter = json!({ "room": { "include_leave": true, "timeline": { "limit": 1 } } });
	let filter = filter.to_string();
	let response: Value = self
		.services
		.client
		.clients
		.default
		.get(self.url("sync"))
		.bearer_auth(self.token)
		.query(&[("filter", filter.as_str()), ("timeout", "0")])
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	let room = response["rooms"]["leave"]
		.get(room_id.as_str())
		.ok_or_else(|| err!("{room_id} is not a left room: {response}"))?;

	Ok(room["state"]["events"]
		.as_array()
		.cloned()
		.unwrap_or_default())
}

/// Apply a membership action to this user, such as a join or a leave.
#[implement(Client, params = "<'_>")]
async fn act(&self, room_id: &RoomId, action: &str) -> Result {
	self.post(&format!("rooms/{room_id}/{action}"), &json!({}))
		.await
		.map(drop)
}
