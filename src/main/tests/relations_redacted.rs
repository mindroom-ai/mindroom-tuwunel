#![cfg(test)]

use serde_json::{Value, json};
use tuwunel_core::{
	Result, implement,
	ruma::{EventId, OwnedEventId, RoomId},
};
use tuwunel_service::Services;

use self::{
	client::{Client, field, register},
	fixture::boot,
};

mod client;
mod fixture;

const ACCESS_TOKEN: &str = "relations-redacted-test-access-token";

/// `/relations` leaves out redacted events.
///
/// Redaction strips an event's `m.relates_to`, so a redacted event relates to
/// nothing: the type-filtered call already left it out, and the unfiltered and
/// recursive calls now do too. A recursive walk does not reach the events
/// relating to a redacted thread reply either, since they relate to the root
/// only through it; asked about the redacted reply itself, `/relations` still
/// lists them.
#[test]
fn relations_leave_out_redacted_events() -> Result {
	boot("relations-redacted", ["log_enable=false"], exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	register(services, "relationsredacted", ACCESS_TOKEN).await?;

	let client = Client { services, base, token: ACCESS_TOKEN };
	let room_id = client
		.create_room(&json!({ "preset": "private_chat" }))
		.await?;

	let root = client
		.send(
			&room_id,
			"m.room.message",
			"root",
			&json!({ "msgtype": "m.text", "body": "root" }),
		)
		.await?;

	let reply = client.reply(&room_id, "kept", &root).await?;
	let stop = client.react(&room_id, "stop", &reply).await?;
	let deleted = client.reply(&room_id, "deleted", &root).await?;
	let thumbs = client.react(&room_id, "thumbs", &deleted).await?;

	client.redact(&room_id, &stop).await?;
	client.redact(&room_id, &deleted).await?;

	let cases: [(&EventId, &str, &[&str]); 6] = [
		(&root, "?recurse=true&dir=b", &[reply.as_str()]),
		(&root, "?recurse=true&dir=f", &[reply.as_str()]),
		(&root, "?dir=b", &[reply.as_str()]),
		(&reply, "?dir=b", &[]),
		(&reply, "/m.annotation?dir=b", &[]),
		(&deleted, "?dir=b", &[thumbs.as_str()]),
	];

	for (target, path, expected) in cases {
		let chunk = client.relations(&room_id, target, path).await?;

		assert_eq!(chunk, expected, "/relations/{target}{path}");
	}

	Ok(())
}

/// The event ids of one `/relations` page; `path` follows the target event id.
#[implement(Client, params = "<'_>")]
async fn relations(&self, room_id: &RoomId, target: &EventId, path: &str) -> Result<Vec<String>> {
	let url = format!("{}/_matrix/client/v1/rooms/{room_id}/relations/{target}{path}", self.base);
	let page: Value = self
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

	page["chunk"]
		.as_array()
		.into_iter()
		.flatten()
		.map(|event| field(event, "event_id").map(ToOwned::to_owned))
		.collect()
}

/// Reply in the thread under `root`.
#[implement(Client, params = "<'_>")]
async fn reply(&self, room_id: &RoomId, body: &str, root: &EventId) -> Result<OwnedEventId> {
	let content = json!({
		"msgtype": "m.text",
		"body": body,
		"m.relates_to": { "rel_type": "m.thread", "event_id": root },
	});

	self.send(room_id, "m.room.message", body, &content)
		.await
}

/// React to `target` with a key named `key`.
#[implement(Client, params = "<'_>")]
async fn react(&self, room_id: &RoomId, key: &str, target: &EventId) -> Result<OwnedEventId> {
	let content = json!({
		"m.relates_to": { "rel_type": "m.annotation", "event_id": target, "key": key },
	});

	self.send(room_id, "m.reaction", key, &content)
		.await
}

/// Redact `event_id`.
#[implement(Client, params = "<'_>")]
async fn redact(&self, room_id: &RoomId, event_id: &EventId) -> Result {
	self.services
		.client
		.clients
		.default
		.put(self.url(&format!("rooms/{room_id}/redact/{event_id}/redact-{event_id}")))
		.bearer_auth(self.token)
		.json(&json!({}))
		.send()
		.await?
		.error_for_status()?;

	Ok(())
}

/// Send an event of `kind` and return its id.
#[implement(Client, params = "<'_>")]
async fn send(
	&self,
	room_id: &RoomId,
	kind: &str,
	txn: &str,
	content: &Value,
) -> Result<OwnedEventId> {
	let response: Value = self
		.services
		.client
		.clients
		.default
		.put(self.url(&format!("rooms/{room_id}/send/{kind}/{txn}")))
		.bearer_auth(self.token)
		.json(content)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(field(&response, "event_id")?.try_into()?)
}
