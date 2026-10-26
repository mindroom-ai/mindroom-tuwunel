use futures::StreamExt;
use ruma::api::Direction;
use serde_json::json;
use tuwunel_core::{
	Result,
	config::Figment,
	matrix::{
		PduCount,
		pdu::{PduId, RawPduId},
	},
	utils::u64_from_u8,
};
use tuwunel_database::Json;

use super::{
	relations::Walk,
	typed_relations::{CHILD_COUNT_OFFSET, KEY_LEN, PREFIX_LEN, Tag, key, prefix},
};
use crate::{Services, test_utils::fixture};

#[test]
fn typed_relation_key_round_trips() {
	let shortroomid = 0x0102_0304_0506_0708_u64;
	let parent = PduCount::Normal(0x1112_1314_1516_1718);
	let child_ts = 0x2122_2324_2526_2728_u64;
	let child = PduCount::Normal(0x3132_3334_3536_3738);

	let key = key(shortroomid, parent, Tag::Replace, child_ts, child);

	assert_eq!(key.len(), KEY_LEN);
	assert_eq!(&key[..8], &shortroomid.to_be_bytes());
	assert_eq!(&key[8..16], &parent.to_be_bytes());
	assert_eq!(key[16], u8::from(Tag::Replace));
	assert_eq!(&key[17..25], &child_ts.to_be_bytes());

	let read_child = PduCount::from_unsigned(u64_from_u8(&key[CHILD_COUNT_OFFSET..KEY_LEN]));

	assert_eq!(read_child, child);

	let prefix = prefix(shortroomid, parent, Tag::Replace);

	assert_eq!(prefix.len(), PREFIX_LEN);
	assert!(key.starts_with(&prefix));
}

#[test]
fn rel_tag_wire_bytes() {
	assert_eq!(u8::from(Tag::Replace), 0x01);
	assert_eq!(u8::from(Tag::Reference), 0x02);
}

/// The room the walks below read, and another room on the same server.
const ROOM: u64 = 1;
const OTHER_ROOM: u64 = 2;

/// A thread of three replies, an edit of each, and a chain of a reaction and
/// two references below the first edit, at walk depths 0 to 4. A reply also
/// has a relation row left by a deleted edit, with a child of its own, and the
/// root has a relation from an event in another room, also with a child. The
/// rows that cannot be served sit at depths 0 and 1.
///
/// Returns the root and the events a full walk serves, in position order.
fn thread(services: &Services) -> (u64, Vec<u64>) {
	store(services, 1, ROOM, &[]);
	for (count, parent) in
		[(2, 1), (3, 1), (4, 1), (5, 2), (6, 3), (7, 4), (8, 5), (9, 8), (10, 9)]
	{
		store(services, count, ROOM, &[parent]);
	}

	// Deleted edit 11 of reply 2 keeps its row, and 12 relates to it.
	services
		.pdu_metadata
		.add_relation(PduCount::Normal(11), PduCount::Normal(2));
	store(services, 12, ROOM, &[11]);

	// 13 in another room relates to the root, and 14 relates to 13.
	store(services, 13, OTHER_ROOM, &[1]);
	store(services, 14, ROOM, &[13]);

	(1, (2..=9).collect())
}

/// Store an event at `count` in `room` with a relation row to each parent.
fn store(services: &Services, count: u64, room: u64, parents: &[u64]) {
	let pdu_id: RawPduId = PduId {
		shortroomid: room,
		count: PduCount::Normal(count),
	}
	.into();
	let pdu = json!({
		"type": "m.room.message", "event_id": format!("$event-{count}:localhost"),
		"room_id": "!thread:localhost", "sender": "@alice:localhost",
		"origin_server_ts": 1, "depth": 1, "hashes": { "sha256": "hash" },
		"prev_events": [], "auth_events": [], "content": { "body": "event" },
	});

	services.db["pduid_pdu"].raw_put(pdu_id, Json(pdu));
	for &parent in parents {
		services
			.pdu_metadata
			.add_relation(PduCount::Normal(count), PduCount::Normal(parent));
	}
}

/// Page through the recursive walk of `root`, `page` events at a time, with
/// the given limits. Returns the depth each page reported and the positions in
/// the order the pages served them.
async fn walk(
	services: &Services,
	root: u64,
	dir: Direction,
	(events, rows): (usize, usize),
	page: usize,
) -> (Vec<usize>, Vec<u64>) {
	let (mut depths, mut served, mut from) = (Vec::new(), Vec::new(), None);

	loop {
		let walk = Walk { depth: 3, events, rows };
		let (depth, relations) = services
			.pdu_metadata
			.walk_relations(ROOM, PduCount::Normal(root), from, dir, None, walk)
			.await;

		let chunk: Vec<_> = relations
			.take(page)
			.map(|(count, _)| count)
			.collect()
			.await;

		depths.push(depth);
		served.extend(chunk.iter().map(|count| count.into_unsigned()));
		from = chunk
			.last()
			.copied()
			.filter(|_| chunk.len() == page);
		if from.is_none() {
			return (depths, served);
		}
	}
}

/// Check every page size and direction of a walk with the given limits: each
/// page reports `depth`, and the pages serve `events` once each, in order.
async fn check_walk(
	services: &Services,
	root: u64,
	limits: (usize, usize),
	depth: usize,
	events: &[u64],
) {
	for page in 1..=events.len().saturating_add(1) {
		for dir in [Direction::Forward, Direction::Backward] {
			let (depths, served) = walk(services, root, dir, limits, page).await;
			let expected: Vec<_> = match dir {
				| Direction::Forward => events.to_vec(),
				| Direction::Backward => events.iter().rev().copied().collect(),
			};

			let case = format!("limits {limits:?}, {dir:?} by {page}");

			assert_eq!(served, expected, "{case}");
			assert!(depths.iter().all(|&d| d == depth), "{case}: depths {depths:?}");
		}
	}
}

/// Rows whose event was deleted or is in another room are not kept, so they
/// do not count against the event limit, and nothing below them is walked.
#[tokio::test]
async fn recursive_walk_keeps_only_events_in_the_room() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let (root, events) = thread(&fixture.services);

	check_walk(&fixture.services, root, (events.len(), usize::MAX), 3, &events).await;

	Ok(())
}

/// A level that does not fit the limits is dropped whole with the levels below
/// it, on every page alike, and the depth reached is reported. When the
/// target's own relations do not fit, they are served as without recursion.
#[tokio::test]
async fn recursive_walk_serves_whole_levels() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let (root, events) = thread(services);
	let levels = |depth: usize| match depth {
		| 0 => &events[..3],
		| 1 => &events[..6],
		| 2 => &events[..7],
		| _ => events.as_slice(),
	};

	// Kept events per level: 3, 3, 1, 1. The deleted edit, the event in the
	// other room and their children are never kept.
	for (event_limit, depth) in [(7, 2), (6, 1), (5, 0), (3, 0)] {
		check_walk(services, root, (event_limit, usize::MAX), depth, levels(depth)).await;
	}

	// Rows read per level, the unservable ones included: 4, 4, 1, 1.
	for (row_limit, depth) in [(9, 2), (8, 1), (7, 0), (4, 0)] {
		check_walk(services, root, (usize::MAX, row_limit), depth, levels(depth)).await;
	}

	for limits in [(2, usize::MAX), (usize::MAX, 3)] {
		check_walk(services, root, limits, 0, levels(0)).await;
	}

	Ok(())
}

/// An event relating to two events of the tree is served once.
#[tokio::test]
async fn recursive_walk_serves_an_event_reached_twice_once() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let (root, mut events) = thread(services);

	store(services, 15, ROOM, &[3, 4]);
	events.push(15);
	check_walk(services, root, (usize::MAX, usize::MAX), 3, &events).await;

	Ok(())
}
