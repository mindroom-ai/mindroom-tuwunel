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

const OWNER_TOKEN: &str = "redaction-prev-content-owner-access-token";
const MEMBER_TOKEN: &str = "redaction-prev-content-member-access-token";

/// Redacting a replaced state event redacts the copy its successor holds.
///
/// The event that replaced it carries its content in `unsigned.prev_content`,
/// so a member, even one who joins after the redaction, could otherwise still
/// read the removed content there.
#[test]
fn redaction_redacts_the_successor_prev_content() -> Result {
	let options: [&str; 0] = [];

	boot("redaction-prev-content", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	register(services, "topicowner", OWNER_TOKEN).await?;
	register(services, "topicmember", MEMBER_TOKEN).await?;

	let owner = Client { services, base, token: OWNER_TOKEN };
	let member = Client { services, base, token: MEMBER_TOKEN };

	let room_id = owner
		.create_room(&json!({ "preset": "public_chat" }))
		.await?;

	let topic = format!("rooms/{room_id}/state/m.room.topic");
	let secret = owner
		.put(&topic, &json!({ "topic": "secret" }))
		.await?;
	let secret = field(&secret, "event_id")?;

	let second = owner
		.put(&topic, &json!({ "topic": "second" }))
		.await?;
	let second = field(&second, "event_id")?;

	let current = owner
		.put(&topic, &json!({ "topic": "current" }))
		.await?;
	let current = field(&current, "event_id")?;

	owner
		.put(&format!("rooms/{room_id}/redact/{secret}/redaction"), &json!({}))
		.await?;

	member
		.post(&format!("rooms/{room_id}/join"), &json!({}))
		.await?;

	let event = format!("rooms/{room_id}/event");
	let second = member
		.get(&member.url(&format!("{event}/{second}")))
		.await?;

	assert_eq!(
		second["unsigned"]["prev_content"],
		json!({}),
		"successor kept the redacted content: {second}"
	);

	let current = member
		.get(&member.url(&format!("{event}/{current}")))
		.await?;

	assert_eq!(
		current["unsigned"]["prev_content"],
		json!({ "topic": "second" }),
		"unredacted predecessor lost its content: {current}"
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

/// Get an absolute URL as this user and parse the reply.
#[implement(Client, params = "<'_>")]
async fn get(&self, url: &str) -> Result<Value> {
	let response = self
		.services
		.client
		.clients
		.default
		.get(url)
		.bearer_auth(self.token)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(response)
}
