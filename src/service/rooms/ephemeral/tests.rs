#![cfg(test)]

use ruma::{
	DeviceId, OwnedUserId, RoomId, TransactionId, UserId, api::client::filter::RoomEventFilter,
};
use serde_json::{json, value::to_raw_value};

use super::{Buffer, MAX_BYTES, MAX_EVENTS, New, RATE_LIMIT, SendOutcome, opts_into};

const ROOM: &str = "!room:example.com";
const OTHER_ROOM: &str = "!other:example.com";
const SENDER: &str = "@sender:example.com";
const DEVICE: &str = "DEVICE";
const OTHER_DEVICE: &str = "OTHER";
const EVENT_TYPE: &str = "io.mindroom.live";
const T0: u64 = 1_800_000_000_000;

fn room() -> &'static RoomId { ROOM.try_into().unwrap() }
fn other_room() -> &'static RoomId { OTHER_ROOM.try_into().unwrap() }
fn sender() -> &'static UserId { SENDER.try_into().unwrap() }
fn device() -> &'static DeviceId { DEVICE.into() }
fn other_device() -> &'static DeviceId { OTHER_DEVICE.into() }
fn txn(id: &str) -> &TransactionId { id.into() }

fn new<'a>(room_id: &'a RoomId, txn_id: &'a str, content_len: usize) -> New<'a> {
	New {
		room_id,
		sender: sender(),
		device_id: Some(device()),
		event_type: EVENT_TYPE,
		txn_id: txn(txn_id),
		content: to_raw_value(&json!({ "x": "a".repeat(content_len) })).unwrap(),
	}
}

/// Send at `now`, taking `count` if the buffer admits the event.
fn send(buffer: &mut Buffer, now: u64, count: u64, new: New<'_>) -> SendOutcome {
	buffer
		.send(now, new, || Box::new(count))
		.map_or_else(|outcome| outcome, |_| SendOutcome::Accepted)
}

fn counts(buffer: &Buffer, since: u64, until: u64, now: u64) -> Vec<u64> {
	buffer
		.between(room(), since, until, &filter(Some(&["*"]), &[]), now)
		.into_iter()
		.map(|event| event.count)
		.collect()
}

fn rate_limit() -> u64 { RATE_LIMIT.try_into().unwrap() }

#[test]
fn evicts_oldest_beyond_max_events() {
	let mut buffer = Buffer::default();
	let mut now = T0;
	let last = u64::try_from(MAX_EVENTS).unwrap() + 1;
	for count in 1..=last {
		let txn_id = count.to_string();
		let outcome = send(&mut buffer, now, count, new(room(), &txn_id, 1));

		assert_eq!(outcome, SendOutcome::Accepted, "event {count}");
		now += 2;
	}

	let counts = counts(&buffer, 0, u64::MAX, now);

	assert_eq!(counts.len(), MAX_EVENTS);
	assert_eq!(counts.first(), Some(&2));
	assert_eq!(counts.last(), Some(&last));

	let index = &buffer.rooms[room()].txns;

	assert_eq!(index.len(), MAX_EVENTS, "evicted events leave the transaction index");
}

#[test]
fn evicts_oldest_beyond_max_bytes() {
	let mut buffer = Buffer::default();

	send(&mut buffer, T0, 1, new(room(), "first", MAX_BYTES * 3 / 5));
	send(&mut buffer, T0, 2, new(room(), "second", MAX_BYTES * 3 / 5));

	assert_eq!(counts(&buffer, 0, u64::MAX, T0), [2]);
}

/// The transaction id is held with the event, so it counts toward the bound.
#[test]
fn long_txn_id_counts_toward_max_bytes() {
	let mut buffer = Buffer::default();
	let long_txn_id = "t".repeat(MAX_BYTES / 2);

	send(&mut buffer, T0, 1, new(room(), "first", MAX_BYTES * 3 / 5));
	send(&mut buffer, T0, 2, new(room(), &long_txn_id, 1));

	assert_eq!(counts(&buffer, 0, u64::MAX, T0), [2]);
}

#[test]
fn expires_after_thirty_seconds() {
	let mut buffer = Buffer::default();

	send(&mut buffer, T0, 1, new(room(), "only", 1));

	assert_eq!(counts(&buffer, 0, u64::MAX, T0 + 30_000), [1]);
	assert_eq!(counts(&buffer, 0, u64::MAX, T0 + 30_001), Vec::<u64>::new());
}

/// Expired events no longer answer for their transactions, so the same
/// transaction sent again is buffered anew.
#[test]
fn transaction_after_its_event_expired_is_buffered_again() {
	let mut buffer = Buffer::default();

	send(&mut buffer, T0, 1, new(room(), "first", 1));
	send(&mut buffer, T0, 2, new(room(), "again", 1));

	assert_eq!(
		send(&mut buffer, T0 + 30_001, 3, new(room(), "again", 1)),
		SendOutcome::Accepted
	);
	assert_eq!(counts(&buffer, 0, u64::MAX, T0 + 30_001), [3]);
}

/// After the clock steps back, an event that expired behind a newer one no
/// longer answers for its transaction.
#[test]
fn transaction_expired_behind_a_newer_event_is_buffered_again() {
	let mut buffer = Buffer::default();

	send(&mut buffer, T0 + 1_000, 1, new(room(), "newer", 1));
	send(&mut buffer, T0, 2, new(room(), "older", 1));

	assert_eq!(
		send(&mut buffer, T0 + 30_001, 3, new(room(), "older", 1)),
		SendOutcome::Accepted
	);
	assert_eq!(counts(&buffer, 0, u64::MAX, T0 + 30_001), [1, 3]);

	let held = &buffer.rooms[room()];

	assert!(
		held.txns.values().all(|&events| events == 1),
		"the expired event left the transaction index: {:?}",
		held.txns
	);
	assert!(!held.unordered, "the buffer is back in timestamp order");
}

/// After the clock steps back, events that expired behind a newer one no
/// longer count toward the byte cap, so they cannot push the newer one out.
#[test]
fn events_expired_behind_a_newer_event_free_their_bytes() {
	let mut buffer = Buffer::default();

	let size = MAX_BYTES / 17;

	send(&mut buffer, T0 + 1_000, 1, new(room(), "newer", size));
	for count in 2..=16 {
		let txn_id = count.to_string();

		send(&mut buffer, T0, count, new(room(), &txn_id, size));
	}

	send(&mut buffer, T0 + 30_001, 17, new(room(), "last", 2 * size));

	assert_eq!(counts(&buffer, 0, u64::MAX, T0 + 30_001), [1, 17]);
}

#[test]
fn window_is_since_exclusive_until_inclusive() {
	let mut buffer = Buffer::default();

	send(&mut buffer, T0, 5, new(room(), "five", 1));
	send(&mut buffer, T0, 6, new(room(), "six", 1));
	send(&mut buffer, T0, 7, new(room(), "seven", 1));

	assert_eq!(counts(&buffer, 5, 6, T0), [6]);
}

#[test]
fn duplicate_txn_is_not_buffered_twice() {
	let mut buffer = Buffer::default();

	let first = send(&mut buffer, T0, 1, new(room(), "same", 1));
	let second = buffer
		.send(T0, new(room(), "same", 1), || -> Box<u64> {
			unreachable!("a duplicate takes no count")
		})
		.map(|_| ());

	assert_eq!(first, SendOutcome::Accepted);
	assert_eq!(second, Err(SendOutcome::Duplicate));
	assert_eq!(counts(&buffer, 0, u64::MAX, T0), [1]);

	let other = New {
		device_id: Some(other_device()),
		..new(room(), "same", 1)
	};

	assert_eq!(send(&mut buffer, T0, 2, other), SendOutcome::Accepted);
	assert_eq!(counts(&buffer, 0, u64::MAX, T0), [1, 2]);
}

#[test]
fn rate_limits_event_past_the_limit_in_one_second() {
	let mut buffer = Buffer::default();
	let limit = rate_limit();
	for count in 1..=limit {
		let txn_id = count.to_string();

		assert_eq!(
			send(&mut buffer, T0, count, new(room(), &txn_id, 1)),
			SendOutcome::Accepted,
			"event {count}"
		);
	}

	let SendOutcome::RateLimited { retry_after_ms } =
		send(&mut buffer, T0 + 500, limit + 1, new(room(), "over", 1))
	else {
		panic!("the event past the limit in one second must be rate limited");
	};

	assert_eq!(retry_after_ms, 500);
	assert_eq!(
		send(&mut buffer, T0 + 1000, limit + 2, new(room(), "next", 1)),
		SendOutcome::Accepted
	);
}

#[test]
fn retried_transaction_is_answered_in_a_full_window() {
	let mut buffer = Buffer::default();
	let limit = rate_limit();
	for count in 1..=limit {
		let txn_id = count.to_string();

		send(&mut buffer, T0, count, new(room(), &txn_id, 1));
	}

	assert_eq!(
		send(&mut buffer, T0 + 100, limit + 1, new(room(), "7", 1)),
		SendOutcome::Duplicate
	);
	assert!(matches!(
		send(&mut buffer, T0 + 100, limit + 2, new(room(), "over", 1)),
		SendOutcome::RateLimited { .. }
	));
}

/// What a sync saw of a stretch of streaming: the frames its senders sent, the
/// ones the buffer refused, the ones the sync got, and whether those were the
/// accepted ones, each once and in order.
#[derive(Debug, Eq, PartialEq)]
struct Streamed {
	sent: u64,
	refused: u64,
	delivered: u64,
	exact: bool,
}

/// Streams `streams` replies from each of `senders` into one room for
/// `seconds`, each sending one delta every 100 ms and a snapshot every 5 s
/// instead, as the MindRoom runtime does, while a sync reads the room every
/// `read_every_ms`. A delta's body is `delta_len` characters of text in 8
/// bytes of JSON, and a snapshot's `snapshot_len`.
#[expect(clippy::arithmetic_side_effects)]
fn stream(
	senders: usize,
	streams: usize,
	seconds: u64,
	read_every_ms: u64,
	(delta_len, snapshot_len): (usize, usize),
) -> Streamed {
	let senders: Vec<OwnedUserId> = (0..senders)
		.map(|i| {
			format!("@agent{i}:example.com")
				.try_into()
				.unwrap()
		})
		.collect();

	let mut buffer = Buffer::default();
	let (mut accepted, mut got) = (Vec::new(), Vec::new());
	let (mut count, mut since) = (0, 0);
	for tick in 0..seconds * 10 {
		let now = T0 + tick * 100;
		for (agent, sender) in senders.iter().enumerate() {
			for reply in 0..streams {
				let txn_id = format!("{agent}.{reply}.{tick}");
				let content_len = if tick % 50 == 0 { snapshot_len } else { delta_len };
				let event = New {
					sender,
					..new(room(), &txn_id, content_len)
				};

				count += 1;
				if send(&mut buffer, now, count, event) == SendOutcome::Accepted {
					accepted.push(count);
				}
			}
		}

		if ((tick + 1) * 100).is_multiple_of(read_every_ms) {
			got.extend(counts(&buffer, since, count, now));
			since = count;
		}
	}

	Streamed {
		sent: count,
		refused: count - u64::try_from(accepted.len()).unwrap(),
		delivered: u64::try_from(got.len()).unwrap(),
		exact: got == accepted,
	}
}

/// One sender streaming 50 replies at the runtime's 10 frames a second, as one
/// agent answering in 50 threads of a room does, is never refused, and a sync
/// that reads the room every 100 ms gets every frame.
#[test]
fn one_sender_streams_fifty_replies() {
	let streamed = stream(1, 50, 3, 100, (300, 3_000));

	assert_eq!(streamed, Streamed {
		sent: 1_500,
		refused: 0,
		delivered: 1_500,
		exact: true,
	});
}

/// A sync that falls 5 s behind, the runtime's time between snapshots, still
/// gets every frame of 51 streaming replies from 17 senders.
#[test]
fn sync_five_seconds_behind_gets_every_frame() {
	let streamed = stream(17, 3, 10, 5_000, (300, 3_000));

	assert_eq!(streamed, Streamed {
		sent: 5_100,
		refused: 0,
		delivered: 5_100,
		exact: true,
	});
}

/// One sender's 50 replies, each sending a snapshot of the largest body the
/// route accepts (64 KiB) every 5 s and a 300-byte delta every 100 ms
/// otherwise, fit in the buffer for 5 s, so a sync that falls 5 s behind still
/// gets every frame, each reply's latest snapshot included.
#[test]
fn sync_five_seconds_behind_gets_the_largest_snapshots() {
	let streamed = stream(1, 50, 10, 5_000, (292, 65_528));

	assert_eq!(streamed, Streamed {
		sent: 5_000,
		refused: 0,
		delivered: 5_000,
		exact: true,
	});
}

#[test]
fn sweep_removes_expired_rooms() {
	let mut buffer = Buffer::default();

	send(&mut buffer, T0, 1, new(room(), "old", 1));

	assert!(buffer.rooms.contains_key(room()));

	send(&mut buffer, T0 + 61_000, 2, new(other_room(), "new", 1));

	assert!(!buffer.rooms.contains_key(room()));
	assert!(
		!buffer
			.rates
			.contains_key(&(room().to_owned(), sender().to_owned()))
	);
	assert!(buffer.rooms.contains_key(other_room()));
}

fn filter(types: Option<&[&str]>, not_types: &[&str]) -> RoomEventFilter {
	serde_json::from_value(json!({ "types": types, "not_types": not_types })).unwrap()
}

#[test]
fn opt_in_needs_the_type_listed() {
	assert!(!opts_into(&filter(None, &[]), EVENT_TYPE));
	assert!(!opts_into(&filter(Some(&[]), &[]), EVENT_TYPE));
	assert!(!opts_into(&filter(Some(&["m.typing", "m.receipt"]), &[]), EVENT_TYPE));
	assert!(opts_into(&filter(Some(&["m.typing", EVENT_TYPE]), &[]), EVENT_TYPE));
}

#[test]
fn opt_in_matches_wildcards() {
	for pattern in ["*", "io.mindroom.*", "io.*.live", "*.live", "io.mindroom.live*"] {
		assert!(opts_into(&filter(Some(&[pattern]), &[]), EVENT_TYPE), "{pattern}");
	}

	for pattern in ["m.*", "io.mindroom", "*.lives", "io.*.mindroom.*"] {
		assert!(!opts_into(&filter(Some(&[pattern]), &[]), EVENT_TYPE), "{pattern}");
	}
}

#[test]
fn not_types_override_types() {
	assert!(!opts_into(&filter(Some(&["*"]), &[EVENT_TYPE]), EVENT_TYPE));
	assert!(!opts_into(&filter(Some(&[EVENT_TYPE]), &["io.mindroom.*"]), EVENT_TYPE));
	assert!(opts_into(&filter(Some(&["*"]), &["m.*"]), EVENT_TYPE));
}
