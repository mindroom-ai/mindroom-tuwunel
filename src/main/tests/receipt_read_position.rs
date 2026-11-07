#![cfg(test)]

use serde_json::{Value, json};
use tuwunel_core::{
	Result, err, implement,
	ruma::{EventId, OwnedEventId, RoomId, UserId},
};
use tuwunel_matrix::{
	PduId,
	pdu::{PduCount, RawPduId},
};
use tuwunel_service::{Services, pusher::Notified};

use self::{
	client::{Client, field, register},
	fixture::boot,
};

mod client;
mod fixture;

#[test]
fn read_markers_preserve_later_notifications() -> Result {
	boot("read-markers-position", ["log_enable=false"], async |services, base| {
		let user = register(services, "markerreader", READER_TOKEN).await?;
		register(services, "markersender", SENDER_TOKEN).await?;
		let reader = Client { services, base, token: READER_TOKEN };
		let sender = Client { services, base, token: SENDER_TOKEN };
		let mut failures = Vec::new();
		for case in 0..6 {
			let room = sender
				.create_room(&json!({"preset": "public_chat"}))
				.await?;
			reader
				.post(&format!("rooms/{room}/join"), &json!({}))
				.await?;
			let mut events = Vec::new();
			for i in 0..6 {
				events.push(
					sender
						.message(&room, &format!("marker-{i}"), None)
						.await?,
				);
			}
			let (body, expected) = match case {
				| 0 => (json!({"m.read": events[3]}), 2),
				| 1 => (json!({"m.read.private": events[3]}), 2),
				| 2 => (json!({"m.read": events[3], "m.read.private": events[4]}), 1),
				| 3 => (json!({"m.read": events[4], "m.read.private": events[3]}), 1),
				| 4 => (json!({"m.fully_read": events[3]}), 6),
				| _ => {
					reader
						.post(
							&format!("rooms/{room}/receipt/m.read.private/{}", events[4]),
							&json!({}),
						)
						.await?;
					(json!({"m.read": events[3], "m.read.private": events[3]}), 1)
				},
			};
			reader
				.post(&format!("rooms/{room}/read_markers"), &body)
				.await?;
			let count = services
				.pusher
				.notification_count(&user, &room)
				.await;
			if count != expected {
				failures.push(format!("marker case {case}: count {count}, expected {expected}"));
			}
		}
		if failures.is_empty() {
			Ok(())
		} else {
			Err(err!("{}", failures.join("; ")))
		}
	})
}

#[test]
fn sibling_backlog_does_not_block_sends() -> Result {
	boot("bounded-receipt-scan", ["log_enable=false"], async |services, base| {
		use std::time::{Duration, Instant};
		let user = register(services, "backlogreader", READER_TOKEN).await?;
		register(services, "backlogsender", SENDER_TOKEN).await?;
		let reader = Client { services, base, token: READER_TOKEN };
		let sender = Client { services, base, token: SENDER_TOKEN };
		let room = sender
			.create_room(&json!({"preset": "public_chat"}))
			.await?;
		reader
			.post(&format!("rooms/{room}/join"), &json!({}))
			.await?;
		let root = sender
			.message(&room, "backlog-root", None)
			.await?;
		let reply = sender
			.message(&room, "backlog-reply", Some(&root))
			.await?;
		let notification = first_notification(services, &user).await?;
		seed(services, &reply, 100_000, Some((&user, &notification))).await?;
		services.db["userroomid_notificationcount"].put((&user, &room, &root), 100_001_u64);
		let receipt = async {
			let start = Instant::now();
			reader.receipt(&room, &root, Some("main")).await?;
			Ok::<_, tuwunel_core::Error>(start.elapsed())
		};
		let send = async {
			tokio::time::sleep(Duration::from_millis(50)).await;
			let start = Instant::now();
			sender
				.message(&room, "during-recount", Some(&root))
				.await?;
			Ok::<_, tuwunel_core::Error>(start.elapsed())
		};
		let (receipt, send) = futures::join!(receipt, send);
		let (receipt, send) = (receipt?, send?);
		eprintln!("bounded recount: receipt={receipt:?}, concurrent send={send:?}");
		assert!(receipt < Duration::from_secs(2), "receipt scanned sibling backlog: {receipt:?}");
		assert!(send < Duration::from_secs(1), "send waited for recount: {send:?}");
		// The root is the newest main event: an overflowing main recount reads
		// the main timeline through the present and leaves the thread alone.
		let counts = Counts { services, user: &user, room: &room };
		assert_eq!(counts.get().await, json!([0, { root.as_str(): 100_002 }]));
		Ok(())
	})
}

/// A receipt on the newest event of its context reads it entirely, however
/// many later room events (here 4,097 without notifications, such as edits,
/// reactions and other threads) a recount would have to walk.
#[test]
fn newest_event_clears_after_busy_room() -> Result {
	boot("newest-after-busy-room", ["log_enable=false"], async |services, base| {
		let user = register(services, "newestreader", READER_TOKEN).await?;
		register(services, "newestsender", SENDER_TOKEN).await?;
		let reader = Client { services, base, token: READER_TOKEN };
		let sender = Client { services, base, token: SENDER_TOKEN };
		let room = sender
			.create_room(&json!({"preset": "public_chat"}))
			.await?;
		reader
			.post(&format!("rooms/{room}/join"), &json!({}))
			.await?;
		let old_root = sender.message(&room, "old-root", None).await?;
		let old_reply = sender
			.message(&room, "old-reply", Some(&old_root))
			.await?;
		let busy_root = sender.message(&room, "busy-root", None).await?;
		let busy_reply = sender
			.message(&room, "busy-reply", Some(&busy_root))
			.await?;
		seed(services, &busy_reply, 4097, None).await?;
		let counts = Counts { services, user: &user, room: &room };
		reader
			.receipt(&room, &busy_root, Some("main"))
			.await?;
		reader
			.receipt(&room, &old_reply, Some(old_root.as_str()))
			.await?;
		assert_eq!(counts.get().await, json!([0, { busy_root.as_str(): 1 }]));
		Ok(())
	})
}

/// Reading a thread removes its count row instead of leaving a zero, so rows
/// exist only for threads with unread notifications; rows an older server left
/// at zero go with the next unthreaded receipt.
#[test]
fn read_threads_leave_no_rows() -> Result {
	boot("read-threads-leave-no-rows", ["log_enable=false"], async |services, base| {
		let user = register(services, "rowsreader", READER_TOKEN).await?;
		register(services, "rowssender", SENDER_TOKEN).await?;
		let reader = Client { services, base, token: READER_TOKEN };
		let sender = Client { services, base, token: SENDER_TOKEN };
		let room = sender
			.create_room(&json!({"preset": "public_chat"}))
			.await?;
		reader
			.post(&format!("rooms/{room}/join"), &json!({}))
			.await?;
		let read = sender.message(&room, "read-root", None).await?;
		let reply = sender
			.message(&room, "read-reply", Some(&read))
			.await?;
		reader
			.receipt(&room, &reply, Some(read.as_str()))
			.await?;
		let answered = sender
			.message(&room, "answered-root", None)
			.await?;
		sender
			.message(&room, "answered-reply", Some(&answered))
			.await?;
		reader
			.message(&room, "own-reply", Some(&answered))
			.await?;
		let rows = services
			.pusher
			.thread_notification_counts(&user, &room)
			.await;
		assert!(rows.is_empty(), "read threads left rows: {rows:?}");

		for i in 0..4097 {
			let root: OwnedEventId = format!("$zero{i}:localhost").try_into()?;
			services.db["userroomid_notificationcount"].put((&user, &room, &root), 0_u64);
		}
		let newest = sender.message(&room, "newest", None).await?;
		reader.receipt(&room, &newest, None).await?;
		let rows = services
			.pusher
			.thread_notification_counts(&user, &room)
			.await;
		let main = services
			.pusher
			.notification_count(&user, &room)
			.await;
		assert_eq!((main, rows.len()), (0, 0), "unthreaded receipt on the newest event");
		Ok(())
	})
}

const READER_TOKEN: &str = "receipt-read-position-reader-token";
const SENDER_TOKEN: &str = "receipt-read-position-sender-token";

/// A receipt reads up to its event, and only in its own context.
///
/// The reader's counts after a receipt are those of the notified events after
/// the receipt's event: an unthreaded or main receipt on the fourth of six
/// unread messages leaves two, and a thread receipt on the first of three
/// replies leaves two in that thread. A recount that exceeds its work limit
/// reads its context through the present.
#[test]
fn receipt_reads_up_to_its_event() -> Result {
	boot("receipt-read-position", ["log_enable=false"], exercise)
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

	reader.receipt(&room, &replies[1], None).await?;
	check("unthreaded receipt on t2", counts.get().await, json!([0, { root.as_str(): 1 }]));

	let first = sender.message(&room, "main-first", None).await?;
	sender.message(&room, "main-second", None).await?;

	// More than one recount budget of notifications in another room must not
	// prevent this room's receipt from preserving its later unread message.
	let other = sender
		.create_room(&json!({ "preset": "public_chat" }))
		.await?;
	let mut notification = first_notification(services, &reader_id).await?;
	notification.sroomid = services.short.get_shortroomid(&other).await?;
	for _ in 0..4097 {
		let count = services.globals.next_count();
		services.db["useridcount_notification"]
			.put((&reader_id, *count), tuwunel_database::Json(&notification));
	}

	sender
		.message(&room, "later", Some(&root))
		.await?;
	reader
		.receipt(&room, &replies[2], Some(root.as_str()))
		.await?;
	check(
		"thread receipt after busy other room",
		counts.get().await,
		json!([2, { root.as_str(): 1 }]),
	);

	reader
		.receipt(&room, &first, Some("main"))
		.await?;
	check(
		"main receipt after busy other room",
		counts.get().await,
		json!([1, { root.as_str(): 1 }]),
	);
	reader.receipt(&room, &first, None).await?;
	check(
		"unthreaded receipt after busy other room",
		counts.get().await,
		json!([1, { root.as_str(): 1 }]),
	);

	// A busy thread's traffic must not consume a main receipt's budget, and a
	// recount past its limit reads its context through the present, as an
	// unbounded clear would.
	let busy_root = sender.message(&room, "busy-root", None).await?;
	let busy_reply = sender
		.message(&room, "busy-reply", Some(&busy_root))
		.await?;
	notification.sroomid = services.short.get_shortroomid(&room).await?;
	seed(services, &busy_reply, 4097, Some((&reader_id, &notification))).await?;
	services.db["userroomid_notificationcount"].put((&reader_id, &room, &busy_root), 4098_u64);
	reader
		.receipt(&room, &busy_root, Some("main"))
		.await?;
	check(
		"main receipt after busy other thread",
		counts.get().await,
		json!([0, { root.as_str(): 1, busy_root.as_str(): 4098 }]),
	);
	reader
		.receipt(&room, &busy_reply, Some(busy_root.as_str()))
		.await?;
	check(
		"overflowing thread recount reads the thread",
		counts.get().await,
		json!([0, { root.as_str(): 1 }]),
	);
	reader.receipt(&room, &busy_reply, None).await?;
	check(
		"overflowing unthreaded recount reads the room",
		counts.get().await,
		json!([0, {}]),
	);

	if failures.is_empty() {
		return Ok(());
	}

	Err(err!("{}", failures.join("\n")))
}

/// The user's oldest stored notification, to copy into seeded rows.
async fn first_notification(services: &Services, user: &UserId) -> Result<Notified> {
	let source = services.pusher.get_notifications(user, None);
	futures::pin_mut!(source);
	futures::StreamExt::next(&mut source)
		.await
		.map(|(_, notification)| notification)
		.ok_or_else(|| err!("missing source notification"))
}

/// Copy `template`'s stored PDU to `n` later positions of its room, each with
/// `notified`'s notification row when given: stands in for thousands of sends.
async fn seed(
	services: &Services,
	template: &EventId,
	n: usize,
	notified: Option<(&UserId, &Notified)>,
) -> Result {
	let original = services.timeline.get_pdu_id(template).await?;
	let stored = services.db["pduid_pdu"].get(&original).await?;
	let shortroomid = PduId::from(original).shortroomid;
	for _ in 0..n {
		let count = services.globals.next_count();
		let id: RawPduId = PduId {
			shortroomid,
			count: PduCount::Normal(*count),
		}
		.into();
		services.db["pduid_pdu"].insert(&id, stored.as_ref());
		if let Some((user, notification)) = notified {
			services.db["useridcount_notification"]
				.put((user, *count), tuwunel_database::Json(notification));
		}
	}
	services.clear_cache().await;
	Ok(())
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
