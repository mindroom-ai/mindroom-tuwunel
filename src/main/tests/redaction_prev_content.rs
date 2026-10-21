#![cfg(test)]

use serde_json::{Value, json};
use tuwunel_core::{
	Result, implement,
	ruma::{
		CanonicalJsonValue, EventId, OwnedRoomId, RoomId, canonical_json::to_canonical_value,
	},
};
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
/// read the removed content there. Each case below uses its own room.
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

	successor(&owner, &member).await?;
	repeated(services, &owner).await?;
	bound(&owner).await
}

async fn successor(owner: &Client<'_>, member: &Client<'_>) -> Result {
	let room_id = owner.public_room().await?;
	let secret = owner.topic(&room_id, "secret").await?;
	let second = owner.topic(&room_id, "second").await?;
	let current = owner.topic(&room_id, "current").await?;

	owner
		.redact(&room_id, &secret, "redaction")
		.await?;

	member
		.post(&format!("rooms/{room_id}/join"), &json!({}))
		.await?;

	assert_eq!(
		member.prev_content(&room_id, &second).await?,
		json!({}),
		"successor kept the redacted content"
	);

	assert_eq!(
		member.prev_content(&room_id, &current).await?,
		json!({ "topic": "second" }),
		"unredacted predecessor lost its content"
	);

	Ok(())
}

/// Redacting an already redacted state event leaves its successor alone.
///
/// The successor's copy is changed after the first redaction, so a second
/// search for the successor would show as that copy being redacted again.
async fn repeated(services: &Services, owner: &Client<'_>) -> Result {
	let room_id = owner.public_room().await?;
	let secret = owner.topic(&room_id, "secret").await?;
	let second = owner.topic(&room_id, "second").await?;

	owner.redact(&room_id, &secret, "first").await?;

	let planted = json!({ "topic": "planted" });
	let second_id: &EventId = second.as_str().try_into()?;
	let pdu_id = services.timeline.get_pdu_id(second_id).await?;
	let mut pdu = services
		.timeline
		.get_pdu_json_from_id(&pdu_id)
		.await?;

	if let Some(CanonicalJsonValue::Object(unsigned)) = pdu.get_mut("unsigned") {
		unsigned.insert("prev_content".into(), to_canonical_value(&planted)?);
	}

	services
		.timeline
		.replace_pdu(&pdu_id, &pdu)
		.await?;

	owner.redact(&room_id, &secret, "second").await?;

	assert_eq!(
		owner.prev_content(&room_id, &second).await?,
		planted,
		"repeated redaction searched for the successor again"
	);

	Ok(())
}

/// A successor more than 16 replaced states back keeps its copy.
///
/// The search for it stops at that bound, and the redaction still succeeds.
/// Of 18 topics, the first is 17 replaced states behind the current one and
/// the second 16.
async fn bound(owner: &Client<'_>) -> Result {
	let room_id = owner.public_room().await?;

	let mut topics = Vec::new();
	for n in 0..18 {
		let topic = format!("topic {n}");

		topics.push(owner.topic(&room_id, &topic).await?);
	}

	owner.redact(&room_id, &topics[0], "past").await?;

	let copies = [
		owner.prev_content(&room_id, &topics[1]).await?,
		owner.prev_content(&room_id, &topics[2]).await?,
	];

	assert_eq!(
		copies,
		[json!({ "topic": "topic 0" }), json!({ "topic": "topic 1" })],
		"a copy changed for a successor past the bound"
	);

	owner
		.redact(&room_id, &topics[1], "within")
		.await?;

	assert_eq!(
		owner.prev_content(&room_id, &topics[2]).await?,
		json!({}),
		"successor at the bound kept the redacted content"
	);

	Ok(())
}

/// Create a public room and return its id.
#[implement(Client, params = "<'_>")]
async fn public_room(&self) -> Result<OwnedRoomId> {
	self.create_room(&json!({ "preset": "public_chat" }))
		.await
}

/// Set the room's topic and return the new topic event's id.
#[implement(Client, params = "<'_>")]
async fn topic(&self, room_id: &RoomId, topic: &str) -> Result<String> {
	let path = format!("rooms/{room_id}/state/m.room.topic");
	let response = self
		.put(&path, &json!({ "topic": topic }))
		.await?;

	Ok(field(&response, "event_id")?.to_owned())
}

/// Redact one event under a transaction id.
#[implement(Client, params = "<'_>")]
async fn redact(&self, room_id: &RoomId, event_id: &str, txn: &str) -> Result {
	let path = format!("rooms/{room_id}/redact/{event_id}/{txn}");

	self.put(&path, &json!({})).await.map(drop)
}

/// The `unsigned.prev_content` of one event, as this user reads it.
#[implement(Client, params = "<'_>")]
async fn prev_content(&self, room_id: &RoomId, event_id: &str) -> Result<Value> {
	let url = self.url(&format!("rooms/{room_id}/event/{event_id}"));
	let event = self.get(&url).await?;

	Ok(event["unsigned"]["prev_content"].clone())
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
