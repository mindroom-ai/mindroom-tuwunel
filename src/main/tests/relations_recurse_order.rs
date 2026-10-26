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
/// references below the first reply reaches past the recursion depth, and one
/// event relates to two replies at once. Events in another room that relate to
/// the thread are not part of it. Read page by page at several limits and in
/// both directions, the thread gives every event down to the depth limit
/// exactly once, in the order it was sent (reversed for `dir=b`), nothing past
/// the depth limit, and the same `recursion_depth` on every page. A `to` ends
/// the walk before the position it names, whether it is one of the thread's
/// events or not. Without recursion only the replies come back.
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

	let other_room = client
		.create_room(&json!({ "preset": "private_chat" }))
		.await?;

	let mut names = HashMap::new();
	let mut send = async |room_id: &RoomId, name: &str, relates_to: Option<Value>| {
		let content = message(name, relates_to);
		let event_id = client.send(room_id, name, &content).await?;
		names.insert(event_id.clone(), name.to_owned());

		Ok::<_, tuwunel_core::Error>(event_id)
	};

	let root = send(&room_id, "root", None).await?;

	let mut replies = Vec::new();
	for i in 0..REPLIES {
		let relates_to = json!({ "rel_type": "m.thread", "event_id": root });

		replies.push(send(&room_id, &format!("reply-{i}"), Some(relates_to)).await?);
	}

	// An event outside the thread, with relations of its own, sent between the
	// replies and the edits.
	let outside = send(&room_id, "outside", None).await?;
	for i in 0..2 {
		let relates_to = json!({ "rel_type": "m.reference", "event_id": outside });

		send(&room_id, &format!("outside-reference-{i}"), Some(relates_to)).await?;
	}

	// Events in another room naming the root and a reply.
	for (i, target) in [&root, &replies[0]].into_iter().enumerate() {
		let relates_to = json!({ "rel_type": "m.reference", "event_id": target });

		send(&other_room, &format!("elsewhere-{i}"), Some(relates_to)).await?;
	}

	let mut edits = Vec::new();
	for (i, reply) in replies.iter().enumerate() {
		let relates_to = json!({ "rel_type": "m.replace", "event_id": reply });

		edits.push(send(&room_id, &format!("edit-{i}"), Some(relates_to)).await?);
	}

	// The chain hangs below the first reply at depths 1 to 4. The walk reads the
	// root's children and three levels below them, so the last link is too deep.
	let mut chain: Vec<OwnedEventId> = Vec::new();
	for i in 0..4 {
		let target = chain.last().unwrap_or(&replies[0]);
		let relates_to = json!({ "rel_type": "m.reference", "event_id": target });

		chain.push(send(&room_id, &format!("reference-{i}"), Some(relates_to)).await?);
	}

	// A relation with an event_id and a reply target gives two relation rows.
	let relates_to = json!({
		"event_id": replies[1],
		"m.in_reply_to": { "event_id": replies[2] },
	});

	let twice = send(&room_id, "twice", Some(relates_to)).await?;

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
		.chain([&twice])
		.map(name)
		.collect();

	let replies: Vec<_> = replies.iter().map(name).collect();
	let in_order = |dir: &str, sent: &[String]| -> Vec<String> {
		match dir {
			| "b" => sent.iter().rev().cloned().collect(),
			| _ => sent.to_vec(),
		}
	};

	for limit in [1, 2, 3, 5, 100] {
		for dir in ["b", "f"] {
			let case = format!("dir={dir} limit={limit}");
			let (recursive, depths) = client
				.paginate(&room_id, &root, dir, "true", limit)
				.await?;

			let recursive: Vec<_> = recursive.iter().map(name).collect();
			assert_eq!(recursive, in_order(dir, &tree), "{case} recurse=true");
			assert!(depths.iter().all(|&d| d == Some(3)), "{case} recurse=true: {depths:?}");

			let (direct, depths) = client
				.paginate(&room_id, &root, dir, "false", limit)
				.await?;

			let direct: Vec<_> = direct.iter().map(name).collect();
			assert_eq!(direct, in_order(dir, &replies), "{case} recurse=false");
			assert!(depths.iter().all(Option::is_none), "{case} recurse=false: {depths:?}");
		}
	}

	// A page's next_batch names its last event, and walking the other way to it
	// stops just before that event.
	for (dir, limit, back) in [("b", "5", "f"), ("f", "7", "b")] {
		let query = [("dir", dir), ("recurse", "true"), ("limit", limit)];
		let page = client.page(&room_id, &root, &query).await?;
		let to = page
			.next_batch
			.ok_or_else(|| err!("a dir={dir} page of {limit} reached the end"))?;

		let last = page.events.last().map(name).unwrap_or_default();
		let position = tree.iter().position(|event| *event == last);
		let (before, after) = tree.split_at(position.unwrap_or_default());
		let expected = match back {
			| "f" => before.to_vec(),
			| _ => in_order(back, after.get(1..).unwrap_or_default()),
		};

		let query = [("dir", back), ("recurse", "true"), ("to", to.as_str())];
		let until = client.page(&room_id, &root, &query).await?;
		let until: Vec<_> = until.events.iter().map(name).collect();

		assert_eq!(until, expected, "dir={back} to {last}, the end of a dir={dir} page");
	}

	// A position outside the thread, between its replies and its edits, ends a
	// walk there too.
	let query = [("dir", "b"), ("limit", "1")];
	let between = client
		.page(&room_id, &outside, &query)
		.await?
		.next_batch
		.ok_or_else(|| err!("the outside event has two relations"))?;

	for (dir, expected) in [("f", &tree[..REPLIES]), ("b", &tree[REPLIES..])] {
		let query = [("dir", dir), ("recurse", "true"), ("to", between.as_str())];
		let until = client.page(&room_id, &root, &query).await?;
		let until: Vec<_> = until.events.iter().map(name).collect();

		assert_eq!(until, in_order(dir, expected), "dir={dir} to a position outside the thread");
	}

	Ok(())
}

/// A text message named `name`, relating as `relates_to` says. An edit
/// carries its new content.
fn message(name: &str, relates_to: Option<Value>) -> Value {
	let mut content = json!({ "msgtype": "m.text", "body": name });
	if let Some(relates_to) = relates_to {
		if relates_to["rel_type"] == "m.replace" {
			content["m.new_content"] = json!({ "msgtype": "m.text", "body": name });
		}

		content["m.relates_to"] = relates_to;
	}

	content
}

/// One page of relations.
struct Page {
	events: Vec<OwnedEventId>,
	next_batch: Option<String>,
	recursion_depth: Option<u64>,
}

/// Read every page of the relations of `root` and return the event ids in the
/// order the pages gave them, with the `recursion_depth` of each page.
#[implement(Client, params = "<'_>")]
async fn paginate(
	&self,
	room_id: &RoomId,
	root: &EventId,
	dir: &str,
	recurse: &str,
	limit: usize,
) -> Result<(Vec<OwnedEventId>, Vec<Option<u64>>)> {
	let limit = limit.to_string();
	let (mut events, mut depths, mut from) = (Vec::new(), Vec::new(), None);

	// Each page holds at least one event, so a walk needs fewer pages than
	// the thread has events; any more means a page repeats.
	for _ in 0..64 {
		let mut query = vec![("dir", dir), ("recurse", recurse), ("limit", limit.as_str())];
		query.extend(from.as_deref().map(|from| ("from", from)));

		let page = self.page(room_id, root, &query).await?;

		events.extend(page.events);
		depths.push(page.recursion_depth);
		from = page.next_batch;
		if from.is_none() {
			return Ok((events, depths));
		}
	}

	Err!("relations of {root} did not reach their end: {events:?}")
}

/// Read one page of the relations of `root`.
#[implement(Client, params = "<'_>")]
async fn page(&self, room_id: &RoomId, root: &EventId, query: &[(&str, &str)]) -> Result<Page> {
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

	let events = page["chunk"]
		.as_array()
		.into_iter()
		.flatten()
		.map(|event| Ok(field(event, "event_id")?.try_into()?))
		.collect::<Result<_>>()?;

	Ok(Page {
		events,
		next_batch: page["next_batch"].as_str().map(ToOwned::to_owned),
		recursion_depth: page["recursion_depth"].as_u64(),
	})
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
