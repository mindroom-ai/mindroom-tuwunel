#![cfg(test)]

use serde_json::{Value, json};
use tuwunel_core::{
	Result, err, implement,
	ruma::{EventId, OwnedEventId, RoomId, UserId},
};
use tuwunel_service::Services;

use self::{
	client::{Client, field, register},
	fixture::boot,
};

mod client;
mod fixture;

const READER_TOKEN: &str = "receipt-read-position-reader-token";
const SENDER_TOKEN: &str = "receipt-read-position-sender-token";

/// A receipt reads up to its event, and only in its own context.
///
/// The reader's counts after a receipt are those of the notified events after
/// the receipt's event: an unthreaded or main receipt on the fourth of six
/// unread messages leaves two, and a thread receipt on the first of three
/// replies leaves two in that thread. In `/notifications`, a main receipt
/// reads the main timeline up to its event but no thread reply, a thread
/// receipt reads its thread up to its event, and an unthreaded receipt reads
/// both up to its event; later events stay unread.
#[test]
fn notification_flags_follow_receipt_positions() -> Result {
	boot("notification-read-position", ["log_enable=false"], exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let reader_id = register(services, "positionreader", READER_TOKEN).await?;

	register(services, "positionsender", SENDER_TOKEN).await?;

	let reader = Client { services, base, token: READER_TOKEN };
	let sender = Client { services, base, token: SENDER_TOKEN };
	let room = sender
		.create_room(&json!({ "preset": "public_chat" }))
		.await?;

	reader
		.post(&format!("rooms/{room}/join"), &json!({}))
		.await?;

	let mut messages = Vec::new();
	for body in ["m1", "m2", "m3", "m4", "m5", "m6"] {
		messages.push(sender.message(&room, body, None).await?);
	}

	// Sends count their notifications before they answer.
	let counts = Counts { services, user: &reader_id, room: &room };
	let mut failures = Vec::new();
	let mut check = |step: &str, actual: Value, expected: Value| {
		if actual != expected {
			failures.push(format!("{step}: {actual} instead of {expected}"));
		}
	};

	check("six messages", counts.get().await, json!([6, {}]));

	reader.receipt(&room, &messages[3], None).await?;
	check("unthreaded receipt on m4", counts.get().await, json!([2, {}]));

	reader
		.receipt(&room, &messages[4], Some("main"))
		.await?;

	check("main receipt on m5", counts.get().await, json!([1, {}]));

	let root = sender.message(&room, "root", None).await?;
	let mut replies = Vec::new();
	for body in ["t1", "t2", "t3"] {
		replies.push(sender.message(&room, body, Some(&root)).await?);
	}

	check("three replies", counts.get().await, json!([2, { root.as_str(): 3 }]));

	reader
		.receipt(&room, &replies[0], Some(root.as_str()))
		.await?;

	check("thread receipt on t1", counts.get().await, json!([2, { root.as_str(): 2 }]));

	reader.receipt(&room, &root, Some("main")).await?;

	check("main receipt on the root", counts.get().await, json!([0, { root.as_str(): 2 }]));

	let all = [messages.as_slice(), std::slice::from_ref(&root), replies.as_slice()].concat();
	let read = reader.read_flags(&all).await?;
	check(
		"/notifications after the main receipt",
		read,
		json!([true, true, true, true, true, true, true, true, false, false]),
	);

	reader.receipt(&room, &replies[1], None).await?;
	check("unthreaded receipt on t2", counts.get().await, json!([0, { root.as_str(): 1 }]));

	let read = reader.read_flags(&replies).await?;
	check("/notifications after the unthreaded receipt", read, json!([true, true, false]));
	if failures.is_empty() {
		return Ok(());
	}

	Err(err!("{}", failures.join("\n")))
}

/// The reader's unread counts in one room.
struct Counts<'a> {
	services: &'a Services,
	user: &'a UserId,
	room: &'a RoomId,
}

impl Counts<'_> {
	/// `[main, {thread root: count}]`, threads read down to zero left out.
	async fn get(&self) -> Value {
		let main = self
			.services
			.pusher
			.notification_count(self.user, self.room)
			.await;

		let threads: serde_json::Map<_, _> = self
			.services
			.pusher
			.thread_notification_counts(self.user, self.room)
			.await
			.into_iter()
			.filter(|&(_, (count, _))| count > 0)
			.map(|(root, (count, _))| (root.to_string(), json!(count)))
			.collect();

		json!([main, threads])
	}
}

/// The `read` flag of the notification of each of `events`, in order.
#[implement(Client, params = "<'_>")]
async fn read_flags(&self, events: &[OwnedEventId]) -> Result<Value> {
	let response: Value = self
		.services
		.client
		.clients
		.default
		.get(self.url("notifications"))
		.bearer_auth(self.token)
		.query(&[("limit", "100")])
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	let notifications = response["notifications"]
		.as_array()
		.ok_or_else(|| err!("notifications response without a list"))?;

	events
		.iter()
		.map(|event_id| {
			notifications
				.iter()
				.find(|notification| notification["event"]["event_id"] == event_id.as_str())
				.map(|notification| notification["read"].clone())
				.ok_or_else(|| err!("no notification for {event_id}"))
		})
		.collect()
}

/// Send a message, in the thread under `root` when given.
#[implement(Client, params = "<'_>")]
async fn message(
	&self,
	room_id: &RoomId,
	body: &str,
	root: Option<&EventId>,
) -> Result<OwnedEventId> {
	let mut content = json!({ "msgtype": "m.text", "body": body });
	if let Some(root) = root {
		content["m.relates_to"] = json!({ "rel_type": "m.thread", "event_id": root });
	}

	let response: Value = self
		.services
		.client
		.clients
		.default
		.put(self.url(&format!("rooms/{room_id}/send/m.room.message/{body}")))
		.bearer_auth(self.token)
		.json(&content)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(field(&response, "event_id")?.try_into()?)
}

/// Post an `m.read` receipt, with `thread_id` when given.
#[implement(Client, params = "<'_>")]
async fn receipt(&self, room_id: &RoomId, event_id: &EventId, thread_id: Option<&str>) -> Result {
	let body = thread_id.map_or_else(|| json!({}), |thread| json!({ "thread_id": thread }));

	self.post(&format!("rooms/{room_id}/receipt/m.read/{event_id}"), &body)
		.await
		.map(drop)
}
