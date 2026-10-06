#![cfg(test)]

use serde_json::{Value, json};
use tuwunel_core::{Result, implement};
use tuwunel_service::Services;

use self::{
	client::{Client, register},
	fixture::boot,
};

mod client;
mod fixture;

const OWNER_TOKEN: &str = "sync-v5-knock-timeline-owner-access-token";
const KNOCKER_TOKEN: &str = "sync-v5-knock-timeline-knocker-access-token";

/// A knocked room reaches a sliding sync list without its timeline.
///
/// A knocker has not been admitted, so, like an invitee, it sees the room
/// listed but none of its events.
#[test]
fn knocked_room_carries_no_timeline() -> Result {
	let options: [&str; 0] = [];

	boot("sync-v5-knock-timeline", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	register(services, "knockowner", OWNER_TOKEN).await?;
	register(services, "knocker", KNOCKER_TOKEN).await?;

	let owner = Client { services, base, token: OWNER_TOKEN };
	let knocker = Client { services, base, token: KNOCKER_TOKEN };

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

	knocker
		.post(&format!("knock/{room_id}"), &json!({}))
		.await?;

	let request = json!({
		"lists": { "all": { "ranges": [[0, 9]], "timeline_limit": 10 } },
	});

	let response = knocker.sliding_sync(&request).await?;
	let room = &response["rooms"][room_id.as_str()];

	assert_eq!(room["membership"], "knock", "knocked room missing from the list: {response}");
	assert!(
		room["timeline"]
			.as_array()
			.is_none_or(Vec::is_empty),
		"knocker reads the timeline: {room}"
	);

	Ok(())
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
