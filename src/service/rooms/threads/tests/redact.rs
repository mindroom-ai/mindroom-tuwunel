//! Thread reply counts across redaction: redacting a reply takes it off its
//! root's bundled `m.thread.count`, and the recount repairs counts kept before
//! that.

use std::sync::Arc;

use futures::StreamExt;
use ruma::{
	EventId, OwnedEventId, RoomId, api::Direction, event_id, events::StateEventType, room_id,
};
use serde_json::{Value, json};
use tuwunel_core::{
	Result,
	config::Figment,
	err,
	matrix::pdu::{PduCount, PduEvent, PduId, RawPduId},
};
use tuwunel_database::Json;

use crate::{Services, rooms::short::ShortRoomId, test_utils::fixture};

const MESSAGE: &str = "m.room.message";
const REACTION: &str = "m.reaction";

/// A room with just its create event, enough for redaction to read the room
/// version.
struct Room<'a> {
	services: &'a Services,
	id: &'static RoomId,
	shortroomid: ShortRoomId,
}

#[tokio::test]
async fn redacting_a_thread_reply_decrements_its_root_once() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let room = Room::new(&fixture.services, room_id!("!thread:localhost")).await?;
	let root = event_id!("$root:localhost");
	let first = event_id!("$first:localhost");
	let second = event_id!("$second:localhost");

	room.append(1, root, MESSAGE, text("root"))
		.await?;
	room.append(2, first, MESSAGE, reply(root, "first"))
		.await?;
	room.append(3, second, MESSAGE, reply(root, "second"))
		.await?;

	assert_eq!(room.thread(root).await?["count"], 2);

	room.redact(first).await?;

	let thread = room.thread(root).await?;

	assert_eq!(thread["count"], 1);
	assert_eq!(thread["latest_event"]["event_id"], second.as_str());

	// The reply is already redacted, so it no longer names a thread.
	room.redact(first).await?;

	assert_eq!(room.thread(root).await?["count"], 1);

	// `/relations` still serves the redacted reply, so clients learn of the
	// redaction.
	assert_eq!(room.relations(root).await?, [first, second]);

	// A count that is already zero stays there.
	room.set_count(root, 0).await?;
	room.redact(second).await?;

	assert_eq!(room.thread(root).await?["count"], 0);

	Ok(())
}

#[tokio::test]
async fn redacting_other_events_keeps_the_count() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let room = Room::new(services, room_id!("!thread:localhost")).await?;
	let foreign = Room::new(services, room_id!("!foreign:localhost")).await?;
	let root = event_id!("$root:localhost");
	let answer = event_id!("$answer:localhost");
	let note = event_id!("$note:localhost");
	let answer_edit = event_id!("$answer-edit:localhost");
	let root_edit = event_id!("$root-edit:localhost");
	let reaction = event_id!("$reaction:localhost");
	let backfilled = event_id!("$backfilled:localhost");
	let elsewhere = event_id!("$elsewhere:localhost");

	room.append(1, root, MESSAGE, text("root"))
		.await?;
	room.append(2, answer, MESSAGE, reply(root, "answer"))
		.await?;
	room.append(3, note, MESSAGE, text("unthreaded"))
		.await?;
	room.append(4, answer_edit, MESSAGE, edit(answer))
		.await?;
	room.append(5, root_edit, MESSAGE, edit(root))
		.await?;
	room.append(6, reaction, REACTION, react(root))
		.await?;

	// Backfill neither indexes relations nor counts thread replies.
	room.store(PduCount::Backfilled(-1), backfilled, MESSAGE, reply(root, "backfilled"));

	// A reply naming a root in another room is not counted there.
	foreign
		.append(7, elsewhere, MESSAGE, reply(root, "elsewhere"))
		.await?;

	assert_eq!(room.thread(root).await?["count"], 1);

	for event in [note, answer_edit, root_edit, reaction, backfilled] {
		room.redact(event).await?;

		assert_eq!(room.thread(root).await?["count"], 1, "redacting {event}");
	}

	foreign.redact(elsewhere).await?;

	assert_eq!(room.thread(root).await?["count"], 1);

	Ok(())
}

#[tokio::test]
async fn redacted_root_gains_no_thread_bundle() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let room = Room::new(&fixture.services, room_id!("!thread:localhost")).await?;
	let root = event_id!("$root:localhost");
	let answer = event_id!("$answer:localhost");

	room.append(1, root, MESSAGE, text("root"))
		.await?;
	room.append(2, answer, MESSAGE, reply(root, "answer"))
		.await?;

	// Redaction replaces the root's `unsigned`, bundle included.
	room.redact(root).await?;

	assert!(room.thread(root).await?.is_null());

	room.redact(answer).await?;

	assert!(room.thread(root).await?.is_null());

	Ok(())
}

#[tokio::test]
async fn recount_drops_redacted_replies_and_leaves_other_roots() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let room = Room::new(services, room_id!("!thread:localhost")).await?;

	// Counted before redaction decremented it: one reply is gone.
	let stale = event_id!("$stale:localhost");
	let stale_live = event_id!("$stale-live:localhost");
	let stale_gone = event_id!("$stale-gone:localhost");

	room.append(1, stale, MESSAGE, text("stale"))
		.await?;
	room.append(2, stale_live, MESSAGE, reply(stale, "live"))
		.await?;
	room.append(3, stale_gone, MESSAGE, reply(stale, "gone"))
		.await?;
	room.redact(stale_gone).await?;
	room.set_count(stale, 2).await?;

	// Correct, beside relations that are not thread replies.
	let kept = event_id!("$kept:localhost");

	room.append(4, kept, MESSAGE, text("kept"))
		.await?;
	room.append(5, event_id!("$kept-reply:localhost"), MESSAGE, reply(kept, "reply"))
		.await?;
	room.append(6, event_id!("$kept-edit:localhost"), MESSAGE, edit(kept))
		.await?;
	room.append(7, event_id!("$kept-reaction:localhost"), REACTION, react(kept))
		.await?;

	// Redacted: no bundle to correct.
	let redacted = event_id!("$redacted:localhost");

	room.append(8, redacted, MESSAGE, text("redacted"))
		.await?;
	room.append(9, event_id!("$redacted-reply:localhost"), MESSAGE, reply(redacted, "reply"))
		.await?;
	room.redact(redacted).await?;

	// Backfilled: its replies are not in the relation index.
	let backfilled = event_id!("$backfilled:localhost");

	room.store(PduCount::Backfilled(-1), backfilled, MESSAGE, text("backfilled"));
	room.append(
		10,
		event_id!("$backfilled-reply:localhost"),
		MESSAGE,
		reply(backfilled, "reply"),
	)
	.await?;

	let stale_thread = room.thread(stale).await?;
	let kept_thread = room.thread(kept).await?;
	let backfilled_thread = room.thread(backfilled).await?;

	assert_eq!(stale_thread["count"], 2);
	assert_eq!(kept_thread["count"], 1);
	assert_eq!(backfilled_thread["count"], 1);

	assert_eq!(services.threads.recount_thread_replies().await?, 1);

	let mut expected = stale_thread;

	expected["count"] = json!(1);

	assert_eq!(room.thread(stale).await?, expected);
	assert_eq!(room.thread(kept).await?, kept_thread);
	assert!(room.thread(redacted).await?.is_null());
	assert_eq!(room.thread(backfilled).await?, backfilled_thread);

	// Nothing left to correct, so nothing is written.
	let sequence = services.db.engine.current_sequence();

	assert_eq!(services.threads.recount_thread_replies().await?, 0);
	assert_eq!(services.db.engine.current_sequence(), sequence);

	Ok(())
}

impl<'a> Room<'a> {
	async fn new(services: &'a Services, id: &'static RoomId) -> Result<Self> {
		let create = create_id(id)?;

		services.db["eventid_outlierpdu"].raw_put(
			&create,
			Json(json!({
				"type": "m.room.create",
				"state_key": "",
				"event_id": create,
				"room_id": id,
				"sender": "@alice:localhost",
				"origin_server_ts": 1,
				"depth": 1,
				"hashes": { "sha256": "hash" },
				"prev_events": [],
				"auth_events": [],
				"content": { "creator": "@alice:localhost", "room_version": "10" },
			})),
		);

		let key = services
			.short
			.get_or_create_shortstatekey(&StateEventType::RoomCreate, "")
			.await;

		let compressed = services
			.state_compressor
			.compress_state_event(key, &create)
			.await;

		let state = services
			.state
			.set_event_state(&create, id, Arc::new([compressed].into()))
			.await?;

		let lock = services.state.mutex.lock(id).await;

		services.state.set_room_state(id, state, &lock);

		let shortroomid = services.short.get_or_create_shortroomid(id).await;

		Ok(Self { services, id, shortroomid })
	}

	/// Store an event and index it the way `append_pdu_effects` does.
	async fn append(&self, count: u64, event_id: &EventId, kind: &str, content: Value) -> Result {
		let relates_to = content.get("m.relates_to").cloned();
		let (pdu_id, pdu) = self.store(PduCount::Normal(count), event_id, kind, content);

		let Some(relates_to) = relates_to else {
			return Ok(());
		};

		let target: OwnedEventId = serde_json::from_value(relates_to["event_id"].clone())?;
		let target_count = self
			.services
			.timeline
			.get_pdu_count(&target)
			.await?;

		self.services
			.pdu_metadata
			.add_relation(pdu_id.pdu_count(), target_count);

		if relates_to["rel_type"] == "m.thread" {
			self.services
				.threads
				.add_to_thread(&target, pdu_id, &pdu)
				.await?;
		}

		Ok(())
	}

	fn store(
		&self,
		count: PduCount,
		event_id: &EventId,
		kind: &str,
		content: Value,
	) -> (RawPduId, PduEvent) {
		let pdu_id: RawPduId = PduId { shortroomid: self.shortroomid, count }.into();
		let pdu = self.event(event_id, kind, content);

		self.services.db["eventid_pduid"].insert(event_id.as_bytes(), pdu_id.as_bytes());
		self.services.db["pduid_pdu"].raw_put(pdu_id, Json(&pdu));

		(pdu_id, pdu)
	}

	async fn redact(&self, target: &EventId) -> Result {
		let redaction = self.event(
			event_id!("$redaction:localhost"),
			"m.room.redaction",
			json!({ "redacts": target }),
		);

		let lock = self.services.state.mutex.lock(self.id).await;

		self.services
			.timeline
			.redact_pdu(target, &redaction, self.shortroomid, &lock)
			.await
	}

	/// The root's stored `m.thread` bundle, or null without one.
	async fn thread(&self, root: &EventId) -> Result<Value> {
		let pdu = self.stored(root).await?;

		Ok(pdu["unsigned"]["m.relations"]["m.thread"].clone())
	}

	/// Overwrite the root's stored count, as an earlier server version left it.
	async fn set_count(&self, root: &EventId, count: u64) -> Result {
		let mut pdu = self.stored(root).await?;

		pdu["unsigned"]["m.relations"]["m.thread"]["count"] = json!(count);

		let pdu_id = self.services.timeline.get_pdu_id(root).await?;

		self.services.db["pduid_pdu"].raw_put(pdu_id, Json(&pdu));

		Ok(())
	}

	/// The events `/relations` serves for `target`, oldest first.
	async fn relations(&self, target: &EventId) -> Result<Vec<OwnedEventId>> {
		let count = self
			.services
			.timeline
			.get_pdu_count(target)
			.await?;

		Ok(self
			.services
			.pdu_metadata
			.get_relations(self.shortroomid, count, None, Direction::Forward, None)
			.map(|(_, pdu)| pdu.event_id)
			.collect()
			.await)
	}

	async fn stored(&self, event_id: &EventId) -> Result<Value> {
		let pdu_id = self
			.services
			.timeline
			.get_pdu_id(event_id)
			.await?;
		let pdu = self
			.services
			.timeline
			.get_pdu_json_from_id(&pdu_id)
			.await?;

		Ok(serde_json::to_value(pdu)?)
	}

	fn event(&self, event_id: &EventId, kind: &str, content: Value) -> PduEvent {
		let mut event = json!({
			"type": kind,
			"event_id": event_id,
			"room_id": self.id,
			"sender": "@alice:localhost",
			"origin_server_ts": 1,
			"depth": 1,
			"hashes": { "sha256": "hash" },
			"prev_events": [],
			"auth_events": [],
		});

		event["content"] = content;

		serde_json::from_value(event).expect("test PDU")
	}
}

fn create_id(room: &RoomId) -> Result<OwnedEventId> {
	OwnedEventId::try_from(format!("$create-{}", room.as_str().trim_start_matches('!')))
		.map_err(|e| err!("test create event ID: {e}"))
}

fn text(body: &str) -> Value { json!({ "msgtype": "m.text", "body": body }) }

fn reply(root: &EventId, body: &str) -> Value {
	json!({
		"msgtype": "m.text",
		"body": body,
		"m.relates_to": { "rel_type": "m.thread", "event_id": root },
	})
}

fn react(target: &EventId) -> Value {
	json!({
		"m.relates_to": { "rel_type": "m.annotation", "event_id": target, "key": "+1" },
	})
}

fn edit(target: &EventId) -> Value {
	json!({
		"msgtype": "m.text",
		"body": "* edited",
		"m.new_content": { "msgtype": "m.text", "body": "edited" },
		"m.relates_to": { "rel_type": "m.replace", "event_id": target },
	})
}
