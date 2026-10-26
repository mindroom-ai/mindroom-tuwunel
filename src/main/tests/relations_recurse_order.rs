#![cfg(test)]

use std::collections::HashMap;

use serde_json::{Value, json};
use tuwunel_core::{
	Err, Result, err, implement,
	ruma::{EventId, OwnedEventId, RoomId},
};
use tuwunel_service::Services;

use self::{
	client::{Client, field, register},
	fixture::boot,
};

mod client;
mod fixture;

const ACCESS_TOKEN: &str = "relations-recurse-order-access-token";

const REPLIES: usize = 6;

/// Recursive `/relations` pages return each related event once, in order.
///
/// A thread's replies are all sent before their edits, so every edit is newer
/// than every reply and lands on a page away from its reply. A chain of
/// references below the first reply reaches past the recursion depth. Read
/// page by page at several limits and in both directions, the thread gives
/// every event down to the depth limit exactly once, in the order it was sent
/// (reversed for `dir=b`), and nothing past the depth limit. A `to` taken from
/// a page in the other direction ends the walk before the event it names.
/// Without recursion only the replies come back.
#[test]
fn relations_recurse_pages_each_event_once() -> Result {
	boot("relations-recurse-order", ["log_enable=false"], exercise)
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

	let mut names = HashMap::new();
	let mut replies = Vec::new();
	for i in 0..REPLIES {
		let name = format!("reply-{i}");
		let content = json!({
			"msgtype": "m.text",
			"body": name,
			"m.relates_to": { "rel_type": "m.thread", "event_id": root },
		});

		let event_id = client.send(&room_id, &name, &content).await?;
		names.insert(event_id.clone(), name);
		replies.push(event_id);
	}

	let mut edits = Vec::new();
	for (i, reply) in replies.iter().enumerate() {
		let name = format!("edit-{i}");
		let content = json!({
			"msgtype": "m.text",
			"body": format!("* {name}"),
			"m.new_content": { "msgtype": "m.text", "body": name },
			"m.relates_to": { "rel_type": "m.replace", "event_id": reply },
		});

		let event_id = client.send(&room_id, &name, &content).await?;
		names.insert(event_id.clone(), name);
		edits.push(event_id);
	}

	// The chain hangs below the first reply at depths 1 to 4. The walk reads the
	// root's children and three levels below them, so the last link is too deep.
	let mut chain: Vec<OwnedEventId> = Vec::new();
	for i in 0..4 {
		let name = format!("reference-{i}");
		let target = chain.last().unwrap_or(&replies[0]);
		let content = json!({
			"msgtype": "m.text",
			"body": name,
			"m.relates_to": { "rel_type": "m.reference", "event_id": target },
		});

		let event_id = client.send(&room_id, &name, &content).await?;
		names.insert(event_id.clone(), name);
		chain.push(event_id);
	}

	let name = |event_id: &OwnedEventId| {
		names
			.get(event_id)
			.cloned()
			.unwrap_or_else(|| event_id.to_string())
	};

	let within_depth = chain.iter().take(3);
	let tree: Vec<_> = replies
		.iter()
		.chain(&edits)
		.chain(within_depth)
		.map(name)
		.collect();

	let replies: Vec<_> = replies.iter().map(name).collect();

	for limit in [1, 2, 3, 5, 100] {
		for dir in ["b", "f"] {
			let in_order = |sent: &[String]| -> Vec<String> {
				match dir {
					| "b" => sent.iter().rev().cloned().collect(),
					| _ => sent.to_vec(),
				}
			};

			let case = format!("dir={dir} limit={limit}");
			let recursive = client
				.paginate(&room_id, &root, dir, "true", limit)
				.await?;

			let recursive: Vec<_> = recursive.iter().map(name).collect();
			assert_eq!(recursive, in_order(&tree), "{case} recurse=true");

			let direct = client
				.paginate(&room_id, &root, dir, "false", limit)
				.await?;

			let direct: Vec<_> = direct.iter().map(name).collect();
			assert_eq!(direct, in_order(&replies), "{case} recurse=false");
		}
	}

	// A page's next_batch names its last event, and walking the other way to
	// it stops just before it: forward to the fifth newest event (edit-4) gives
	// the ten oldest, backward to the seventh oldest (edit-0) the eight newest.
	for (dir, limit, back) in [("b", "5", "f"), ("f", "7", "b")] {
		let query = [("dir", dir), ("recurse", "true"), ("limit", limit)];
		let (_, to) = client.page(&room_id, &root, &query).await?;
		let to = to.ok_or_else(|| err!("a dir={dir} page of {limit} reached the end"))?;

		let query = [("dir", back), ("recurse", "true"), ("to", to.as_str())];
		let (until, _) = client.page(&room_id, &root, &query).await?;
		let until: Vec<_> = until.iter().map(name).collect();

		let expected: Vec<_> = match back {
			| "f" => tree.iter().take(10).cloned().collect(),
			| _ => tree.iter().skip(7).rev().cloned().collect(),
		};

		assert_eq!(until, expected, "dir={back} to the end of a dir={dir} page");
	}

	Ok(())
}

/// Read every page of the relations of `root` and return the event ids in the
/// order the pages gave them.
#[implement(Client, params = "<'_>")]
async fn paginate(
	&self,
	room_id: &RoomId,
	root: &EventId,
	dir: &str,
	recurse: &str,
	limit: usize,
) -> Result<Vec<OwnedEventId>> {
	let limit = limit.to_string();
	let mut events = Vec::new();
	let mut from = None;

	// Each page holds at least one event, so a walk needs fewer pages than
	// the thread has events; any more means a page repeats.
	for _ in 0..64 {
		let mut query = vec![("dir", dir), ("recurse", recurse), ("limit", limit.as_str())];
		query.extend(from.as_deref().map(|from| ("from", from)));

		let (chunk, next_batch) = self.page(room_id, root, &query).await?;

		events.extend(chunk);
		from = next_batch;
		if from.is_none() {
			return Ok(events);
		}
	}

	Err!("relations of {root} did not reach their end: {events:?}")
}

/// Read one page of the relations of `root`: its event ids and `next_batch`.
#[implement(Client, params = "<'_>")]
async fn page(
	&self,
	room_id: &RoomId,
	root: &EventId,
	query: &[(&str, &str)],
) -> Result<(Vec<OwnedEventId>, Option<String>)> {
	let url = format!("{}/_matrix/client/v1/rooms/{room_id}/relations/{root}", self.base);
	let page: Value = self
		.services
		.client
		.clients
		.default
		.get(url)
		.bearer_auth(self.token)
		.query(query)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	let chunk = page["chunk"]
		.as_array()
		.into_iter()
		.flatten()
		.map(|event| Ok(field(event, "event_id")?.try_into()?))
		.collect::<Result<_>>()?;

	let next_batch = page["next_batch"].as_str().map(ToOwned::to_owned);

	Ok((chunk, next_batch))
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
