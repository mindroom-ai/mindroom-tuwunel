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

const ACCESS_TOKEN: &str = "relations-next-batch-access-token";

/// `/relations` gives a `next_batch` only while more relations follow.
///
/// A thread of three replies read three at a time is one page without a
/// `next_batch`, in both directions and with recursion. Read two at a time,
/// the first page has a `next_batch` and the page after it, which reaches the
/// end, has none.
#[test]
fn relations_next_batch_only_before_the_end() -> Result {
	boot("relations-next-batch", ["log_enable=false"], exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	register(services, "relations", ACCESS_TOKEN).await?;

	let client = Client { services, base, token: ACCESS_TOKEN };
	let room_id = client
		.create_room(&json!({ "preset": "private_chat" }))
		.await?;

	let root = client
		.send(&room_id, "root", &json!({ "msgtype": "m.text", "body": "root" }))
		.await?;

	for reply in ["one", "two", "three"] {
		let content = json!({
			"msgtype": "m.text",
			"body": reply,
			"m.relates_to": { "rel_type": "m.thread", "event_id": root },
		});

		client.send(&room_id, reply, &content).await?;
	}

	for (dir, recurse) in [("b", "false"), ("f", "false"), ("b", "true")] {
		let case = format!("dir={dir} recurse={recurse}");
		let page = async |limit: &str, from: Option<&str>| {
			client
				.relations(
					&room_id,
					&root,
					&[("dir", dir), ("recurse", recurse), ("limit", limit)],
					from,
				)
				.await
		};

		assert_eq!(page("3", None).await?, (3, None), "{case}: exact-size page");

		let (count, next_batch) = page("2", None).await?;

		assert_eq!(count, 2, "{case}: first page");
		assert!(next_batch.is_some(), "{case}: first page ends early");
		assert_eq!(page("2", next_batch.as_deref()).await?, (1, None), "{case}: last page");
	}

	Ok(())
}

/// Read one page of the thread under `root`: its size and its `next_batch`.
#[implement(Client, params = "<'_>")]
async fn relations(
	&self,
	room_id: &RoomId,
	root: &EventId,
	query: &[(&str, &str)],
	from: Option<&str>,
) -> Result<(usize, Option<String>)> {
	let url =
		format!("{}/_matrix/client/v1/rooms/{room_id}/relations/{root}/m.thread", self.base);
	let from = from.map(|from| ("from", from));
	let page: Value = self
		.services
		.client
		.clients
		.default
		.get(url)
		.bearer_auth(self.token)
		.query(query)
		.query(&from.as_slice())
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	let count = page["chunk"].as_array().map_or(0, Vec::len);
	let next_batch = page["next_batch"].as_str().map(ToOwned::to_owned);

	Ok((count, next_batch))
}

/// Send a message event and return its id.
#[implement(Client, params = "<'_>")]
async fn send(&self, room_id: &RoomId, txn: &str, content: &Value) -> Result<OwnedEventId> {
	let response: Value = self
		.services
		.client
		.clients
		.default
		.put(self.url(&format!("rooms/{room_id}/send/m.room.message/{txn}")))
		.bearer_auth(self.token)
		.json(content)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(field(&response, "event_id")?.try_into()?)
}
