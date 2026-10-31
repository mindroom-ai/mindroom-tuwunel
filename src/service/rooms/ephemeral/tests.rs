#![cfg(test)]

use ruma::{DeviceId, RoomId, TransactionId, UserId, api::client::filter::RoomEventFilter};
use serde_json::{json, value::to_raw_value};

use super::{Buffer, New, SendOutcome, opts_into};

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
		.between(room(), since, until, now)
		.into_iter()
		.map(|event| event.count)
		.collect()
}

#[test]
fn evicts_oldest_beyond_256_events() {
	let mut buffer = Buffer::default();
	let mut now = T0;
	for count in 1..=257 {
		let txn_id = count.to_string();
		let outcome = send(&mut buffer, now, count, new(room(), &txn_id, 1));

		assert_eq!(outcome, SendOutcome::Accepted, "event {count}");
		now += 40;
	}

	let counts = counts(&buffer, 0, u64::MAX, now);

	assert_eq!(counts.len(), 256);
	assert_eq!(counts.first(), Some(&2));
	assert_eq!(counts.last(), Some(&257));
}

#[test]
fn evicts_oldest_beyond_one_mib() {
	let mut buffer = Buffer::default();

	send(&mut buffer, T0, 1, new(room(), "first", 600 * 1024));
	send(&mut buffer, T0, 2, new(room(), "second", 600 * 1024));

	assert_eq!(counts(&buffer, 0, u64::MAX, T0), [2]);
}

#[test]
fn expires_after_thirty_seconds() {
	let mut buffer = Buffer::default();

	send(&mut buffer, T0, 1, new(room(), "only", 1));

	assert_eq!(counts(&buffer, 0, u64::MAX, T0 + 30_000), [1]);
	assert!(counts(&buffer, 0, u64::MAX, T0 + 30_001).is_empty());
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
fn rate_limits_31st_event_in_one_second() {
	let mut buffer = Buffer::default();
	for count in 1..=30 {
		let txn_id = count.to_string();

		assert_eq!(
			send(&mut buffer, T0, count, new(room(), &txn_id, 1)),
			SendOutcome::Accepted,
			"event {count}"
		);
	}

	let SendOutcome::RateLimited { retry_after_ms } =
		send(&mut buffer, T0 + 500, 31, new(room(), "31", 1))
	else {
		panic!("the 31st event in one second must be rate limited");
	};

	assert!(retry_after_ms > 0 && retry_after_ms <= 1000, "retry_after_ms {retry_after_ms}");
	assert_eq!(retry_after_ms, 500);
	assert_eq!(send(&mut buffer, T0 + 1000, 32, new(room(), "32", 1)), SendOutcome::Accepted);
}

#[test]
fn retried_transaction_is_answered_in_a_full_window() {
	let mut buffer = Buffer::default();
	for count in 1..=30 {
		let txn_id = count.to_string();

		send(&mut buffer, T0, count, new(room(), &txn_id, 1));
	}

	assert_eq!(send(&mut buffer, T0 + 100, 31, new(room(), "7", 1)), SendOutcome::Duplicate);
	assert!(matches!(
		send(&mut buffer, T0 + 100, 32, new(room(), "31", 1)),
		SendOutcome::RateLimited { .. }
	));
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
