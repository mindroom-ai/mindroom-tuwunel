use futures::StreamExt;
use ruma::{api::Direction, user_id};
use serde_json::json;
use tuwunel_core::{Result, config::Figment, utils::u64_from_u8};
use tuwunel_database::Json;
use tuwunel_matrix::{
	PduCount,
	pdu::{PduId, RawPduId},
};

use super::{
	RequesterQuery,
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

const ROOM: u64 = 1;
const OTHER_ROOM: u64 = 2;

fn store_relation_event(services: &Services, count: u64, room: u64, parents: &[u64]) {
	let pdu_id: RawPduId = PduId {
		shortroomid: room,
		count: PduCount::Normal(count),
	}
	.into();
	let pdu = json!({
		"type": "m.room.message",
		"event_id": format!("$event-{count}:localhost"),
		"room_id": "!thread:localhost",
		"sender": "@alice:localhost",
		"origin_server_ts": 1,
		"depth": 1,
		"hashes": { "sha256": "hash" },
		"prev_events": [],
		"auth_events": [],
		"content": { "body": "event" },
	});

	services.db["pduid_pdu"].raw_put(pdu_id, Json(pdu));
	for &parent in parents {
		services
			.pdu_metadata
			.add_relation(PduCount::Normal(count), PduCount::Normal(parent));
	}
}

/// Relations are only indexed for normal positions, so a backfilled `from`
/// walks forward over all relations and backward over none without panicking.
#[tokio::test]
async fn get_relations_from_backfilled_cursor() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	store_relation_event(services, 1, ROOM, &[]);
	for child in 2..=4 {
		store_relation_event(services, child, ROOM, &[1]);
	}

	for from in [PduCount::Backfilled(-5), PduCount::Backfilled(0)] {
		let forward: Vec<u64> = services
			.pdu_metadata
			.get_relations(ROOM, PduCount::Normal(1), Some(from), Direction::Forward, None)
			.map(|(count, _)| count.into_unsigned())
			.collect()
			.await;
		let backward: Vec<u64> = services
			.pdu_metadata
			.get_relations(ROOM, PduCount::Normal(1), Some(from), Direction::Backward, None)
			.map(|(count, _)| count.into_unsigned())
			.collect()
			.await;

		assert_eq!(forward, vec![2, 3, 4], "from {from}: forward");
		assert_eq!(backward, Vec::<u64>::new(), "from {from}: backward");
	}

	Ok(())
}

/// `requester_relation_rows` counts deleted and cross-room index rows against
/// `limit` by yielding `None` for them while returning servable events.
#[tokio::test]
async fn requester_relation_rows_counts_unservable_rows() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let alice = user_id!("@alice:localhost");
	store_relation_event(services, 1, ROOM, &[]);
	store_relation_event(services, 2, ROOM, &[1]);
	services
		.pdu_metadata
		.add_relation(PduCount::Normal(3), PduCount::Normal(1));
	store_relation_event(services, 4, OTHER_ROOM, &[1]);
	store_relation_event(services, 5, ROOM, &[1]);

	let query = RequesterQuery {
		shortroomid: ROOM,
		target: PduCount::Normal(1),
		from: None,
		to: None,
		dir: Direction::Forward,
		user_id: alice,
		ignored: None,
	};

	let all: Vec<Option<u64>> = services
		.pdu_metadata
		.requester_relation_rows(query, 10)
		.map(|item| item.map(|(count, _)| count.into_unsigned()))
		.collect()
		.await;
	assert_eq!(all, vec![Some(2), None, None, Some(5)], "all raw rows accounted for");

	let capped: Vec<Option<u64>> = services
		.pdu_metadata
		.requester_relation_rows(query, 3)
		.map(|item| item.map(|(count, _)| count.into_unsigned()))
		.collect()
		.await;
	assert_eq!(capped, vec![Some(2), None, None], "row cap stops before 4th row");

	Ok(())
}
