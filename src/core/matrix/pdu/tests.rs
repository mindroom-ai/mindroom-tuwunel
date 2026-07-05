use ruma::{EventId, OwnedRoomId, OwnedUserId, UInt};
use serde_json::{Value as JsonValue, value::RawValue};

use super::{Count, EventHash, Pdu};

fn message_pdu(event_id: &str, sender: &str, ts: u64, content: &str) -> Pdu {
	Pdu {
		kind: ruma::events::TimelineEventType::RoomMessage,
		content: RawValue::from_string(content.to_owned())
			.expect("valid JSON content")
			.into(),
		event_id: EventId::parse(event_id).expect("valid event id"),
		room_id: OwnedRoomId::try_from("!room:example.com").expect("valid room id"),
		sender: OwnedUserId::try_from(sender).expect("valid sender"),
		state_key: None,
		redacts: None,
		prev_events: Default::default(),
		auth_events: Default::default(),
		origin_server_ts: UInt::try_from(ts).expect("valid timestamp"),
		depth: UInt::try_from(1_u64).expect("valid depth"),
		hashes: EventHash::default(),
		origin: None,
		unsigned: None,
		rejected: false,
	}
}

fn unsigned_value(pdu: &Pdu) -> JsonValue {
	serde_json::from_str(
		pdu.unsigned
			.as_ref()
			.expect("unsigned present")
			.json()
			.get(),
	)
	.expect("valid unsigned JSON")
}

/// The `m.replace` bundle must be a full client-format event: the MindRoom
/// Cinny client hydrates replacements from this object verbatim and ignores
/// bundles missing `origin_server_ts`.
#[test]
fn add_relation_bundles_full_client_format_event() {
	let mut original = message_pdu(
		"$original:example.com",
		"@alice:example.com",
		1_000,
		r#"{"body":"thinking"}"#,
	);
	let edit = message_pdu(
		"$edit:example.com",
		"@alice:example.com",
		2_000,
		r#"{"body":"* final","m.new_content":{"body":"final"},"m.relates_to":{"rel_type":"m.replace","event_id":"$original:example.com"}}"#,
	);

	original
		.add_relation("m.replace", &edit)
		.expect("add_relation succeeds");

	let unsigned = unsigned_value(&original);
	let bundle = &unsigned["m.relations"]["m.replace"];

	assert_eq!(bundle["event_id"], "$edit:example.com");
	assert_eq!(bundle["sender"], "@alice:example.com");
	assert_eq!(bundle["room_id"], "!room:example.com");
	assert_eq!(bundle["type"], "m.room.message");
	assert_eq!(bundle["origin_server_ts"], 2_000);
	assert_eq!(bundle["content"]["m.new_content"]["body"], "final");
	assert_eq!(
		bundle["content"]["m.relates_to"]["rel_type"], "m.replace",
		"bundle content must retain m.relates_to"
	);
	assert!(
		bundle.get("depth").is_none() && bundle.get("hashes").is_none(),
		"bundle must be client format, not a federation PDU: {bundle}"
	);
}

#[test]
fn add_relation_preserves_existing_unsigned_and_relations() {
	let mut original =
		message_pdu("$original2:example.com", "@alice:example.com", 1_000, r#"{"body":"root"}"#);
	original.unsigned = Some(
		RawValue::from_string(r#"{"age":5,"m.relations":{"m.thread":{"count":2}}}"#.to_owned())
			.expect("valid unsigned")
			.into(),
	);

	let edit = message_pdu(
		"$edit2:example.com",
		"@alice:example.com",
		2_000,
		r#"{"body":"* new","m.relates_to":{"rel_type":"m.replace","event_id":"$original2:example.com"}}"#,
	);

	original
		.add_relation("m.replace", &edit)
		.expect("add_relation succeeds");

	let unsigned = unsigned_value(&original);
	assert_eq!(unsigned["age"], 5, "unrelated unsigned keys must survive");
	assert_eq!(unsigned["m.relations"]["m.thread"]["count"], 2, "existing bundles must survive");
	assert_eq!(unsigned["m.relations"]["m.replace"]["event_id"], "$edit2:example.com");
}

#[test]
fn backfilled_parse() {
	let count: Count = "-987654".parse().expect("parse() failed");
	let backfilled = matches!(count, Count::Backfilled(_));

	assert!(backfilled, "not backfilled variant");
}

#[test]
fn normal_parse() {
	let count: Count = "987654".parse().expect("parse() failed");
	let backfilled = matches!(count, Count::Backfilled(_));

	assert!(!backfilled, "backfilled variant");
}
