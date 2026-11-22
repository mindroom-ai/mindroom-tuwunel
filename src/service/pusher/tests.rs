#![cfg(test)]

use std::time::{Duration, Instant};

use ruma::{
	EventId, RoomId, UInt, UserId,
	events::{AnySyncTimelineEvent, TimelineEventType},
	owned_room_id, owned_user_id,
	push::{
		Action, ConditionalPushRuleInit, EventMatchConditionData, HighlightTweakValue,
		NewConditionalPushRule, NewPushRule, NewSimplePushRule, PredefinedOverrideRuleId,
		PushCondition, PushConditionRoomCtx, RuleKind, Ruleset, SoundTweakValue, Tweak,
	},
	serde::Raw,
	uint,
};
use serde_json::{Value as JsonValue, from_value, json};
use tuwunel_database::{
	Ignore, IgnoreAll, Interfix, SEP, deserialize_from_slice, serialize_to_vec,
};

use super::{
	ExtractRelatesTo, mindroom_push_suppressed, mindroom_terminal_push_content,
	mindroom_terminal_push_event,
	ring_rules::{
		self, AcceptedRings, DEFAULT_CALL_MEMBERSHIP_EXPIRE_MS, MAX_RING_ROOM_MEMBERS,
		RING_ENCRYPTED_RULE_ID, RING_RULE_ID, RING_SOUND_TWEAK, RTC_NOTIFICATION_EVENT_TYPE,
		is_live_call_membership, ring_permitted,
	},
};

const ROOM: &str = "!room:example.com";
const USER: &str = "@user:example.com";
const THREAD_ROOT_A: &str = "$thread_root_a";
const THREAD_ROOT_B: &str = "$thread_root_b";

fn room() -> &'static RoomId { ROOM.try_into().unwrap() }
fn user() -> &'static UserId { USER.try_into().unwrap() }
fn root_a() -> &'static EventId { THREAD_ROOT_A.try_into().unwrap() }
fn root_b() -> &'static EventId { THREAD_ROOT_B.try_into().unwrap() }

fn main_key() -> Vec<u8> { serialize_to_vec((user(), room())).expect("serialize main key") }
fn thread_key(root: &EventId) -> Vec<u8> {
	serialize_to_vec((user(), room(), root)).expect("serialize thread key")
}
fn interfix_prefix() -> Vec<u8> {
	serialize_to_vec((user(), room(), Interfix)).expect("serialize prefix")
}

fn stream_event(sender: &str, status: &str, msgtype: &str) -> Raw<AnySyncTimelineEvent> {
	from_value(json!({
		"content": {
			"body": "* final answer",
			"io.mindroom.stream_status": status,
			"m.new_content": {
				"body": "final answer",
				"io.mindroom.stream_status": status,
				"msgtype": msgtype,
			},
			"m.relates_to": {"event_id": "$original", "rel_type": "m.replace"},
			"msgtype": msgtype,
		},
		"event_id": "$edit",
		"origin_server_ts": 1,
		"sender": sender,
		"type": "m.room.message",
	}))
	.expect("valid timeline event")
}

fn encrypted_stream_event(sender: &str, status: &str, edit: bool) -> Raw<AnySyncTimelineEvent> {
	let mut content = serde_json::json!({
		"algorithm": "m.megolm.v1.aes-sha2",
		"ciphertext": "encrypted payload",
		"device_id": "DEVICE",
		"io.mindroom.stream_status": status,
		"sender_key": "sender key",
		"session_id": "session",
	});
	if edit {
		content
			.as_object_mut()
			.expect("content object")
			.insert(
				"m.relates_to".into(),
				serde_json::json!({
				"event_id": "$original",
				"rel_type": "m.replace",
				}),
			);
	}
	from_value(json!({
		"content": content,
		"event_id": "$encrypted_edit",
		"origin_server_ts": 1,
		"sender": sender,
		"type": "m.room.encrypted",
	}))
	.expect("valid encrypted timeline event")
}

/// Main `(user, room)` and thread `(user, room, root)` rows share a CF.
/// The `Interfix` prefix appends a trailing separator so a `starts_with`
/// scan matches only the longer 3-tuple shape.
#[test]
fn interfix_prefix_excludes_main_row() {
	let prefix = interfix_prefix();
	let main = main_key();

	assert!(!main.starts_with(&prefix), "Main 2-tuple row must not match thread prefix");
	assert_eq!(prefix.len(), main.len() + 1);
	assert_eq!(&prefix[..main.len()], &*main);
	assert_eq!(*prefix.last().unwrap(), SEP);
}

#[test]
fn interfix_prefix_includes_thread_row() {
	let prefix = interfix_prefix();
	let thread = thread_key(root_a());

	assert!(thread.starts_with(&prefix), "Thread 3-tuple row must match thread prefix");
}

#[test]
fn distinct_threads_have_distinct_keys() {
	assert_ne!(thread_key(root_a()), thread_key(root_b()));
}

/// The 3-tuple prefix that lists a room's thread rows leaves out its main row.
#[test]
fn thread_prefix_sweep_preserves_main() {
	let prefix = interfix_prefix();
	let main = main_key();
	let a = thread_key(root_a());
	let b = thread_key(root_b());

	assert!(a.starts_with(&prefix));
	assert!(b.starts_with(&prefix));
	assert!(!main.starts_with(&prefix));
}

#[test]
fn notification_key_room_survives_main_and_thread_tail() {
	for key in [main_key(), thread_key(root_a())] {
		let (_, room_id, _): (Ignore, &RoomId, IgnoreAll) =
			deserialize_from_slice(&key).expect("deserialize notification key");

		assert_eq!(room_id, room());
	}
}

/// A reaction relates to the event it annotates, which is the relation
/// MSC3664 follows to notify an author of reactions to their own message.
#[test]
fn reaction_relates_to_the_annotated_event() {
	let content = json!({
		"m.relates_to": {
			"rel_type": "m.annotation",
			"event_id": THREAD_ROOT_A,
			"key": "👍",
		},
	});

	let ExtractRelatesTo { relates_to } =
		from_value(content).expect("deserialize reaction content");

	assert_eq!(relates_to.rel_type.as_deref(), Some("m.annotation"));
	assert_eq!(relates_to.event_id.as_deref(), Some(root_a()));
	assert!(relates_to.in_reply_to.is_none());
}

/// A rich reply carries no `rel_type`, so it is only reachable through the
/// nested `m.in_reply_to`, which MSC3664 matches as its own relation type.
#[test]
fn reply_relates_only_through_in_reply_to() {
	let content = json!({
		"body": "Sounds good",
		"msgtype": "m.text",
		"m.relates_to": {
			"m.in_reply_to": { "event_id": THREAD_ROOT_A },
		},
	});

	let ExtractRelatesTo { relates_to } = from_value(content).expect("deserialize reply content");

	let in_reply_to = relates_to.in_reply_to.map(|reply| reply.event_id);

	assert!(relates_to.rel_type.is_none());
	assert!(relates_to.event_id.is_none());
	assert_eq!(in_reply_to.as_deref(), Some(root_a()));
}

/// A threaded message carries both relations, so both are resolved: the
/// thread root and the reply fallback target.
#[test]
fn threaded_message_relates_to_root_and_reply() {
	let content = json!({
		"body": "Sounds good",
		"msgtype": "m.text",
		"m.relates_to": {
			"rel_type": "m.thread",
			"event_id": THREAD_ROOT_A,
			"is_falling_back": true,
			"m.in_reply_to": { "event_id": THREAD_ROOT_B },
		},
	});

	let ExtractRelatesTo { relates_to } =
		from_value(content).expect("deserialize threaded content");

	let in_reply_to = relates_to.in_reply_to.map(|reply| reply.event_id);

	assert_eq!(relates_to.event_id.as_deref(), Some(root_a()));
	assert_eq!(in_reply_to.as_deref(), Some(root_b()));
}

/// Content without a relation must not resolve one, which is what keeps
/// the lookup off the path of every ordinary message.
#[test]
fn unrelated_message_resolves_no_relation() {
	let content = json!({ "body": "Dinner at 7?", "msgtype": "m.text" });

	assert!(from_value::<ExtractRelatesTo>(content).is_err(), "content carries no relation");
}

#[test]
fn terminal_mindroom_edit_is_normalized_for_push_rules() {
	let event = stream_event("@mindroom_helper:example.com", "completed", "m.text");
	let normalized = mindroom_terminal_push_event(&event).expect("terminal event");
	let value: JsonValue = serde_json::from_str(normalized.json().get()).expect("event json");
	let content = value["content"]
		.as_object()
		.expect("content object");

	assert!(!content.contains_key("m.relates_to"));
	assert!(!content.contains_key("m.new_content"));
	assert_eq!(content.get("body").and_then(JsonValue::as_str), Some("final answer"));
}

#[test]
fn nonterminal_stream_edit_is_not_normalized() {
	let streaming = stream_event("@mindroom_helper:example.com", "streaming", "m.notice");

	assert!(mindroom_terminal_push_event(&streaming).is_none());
}

#[test]
fn stream_protocol_is_sender_agnostic() {
	let remote = stream_event("@mindroom_helper:remote.example", "completed", "m.text");
	let ordinary_user = stream_event("@alice:example.com", "completed", "m.text");

	assert!(mindroom_terminal_push_event(&remote).is_some());
	assert!(mindroom_terminal_push_event(&ordinary_user).is_some());
}

#[test]
fn terminal_encrypted_edit_removes_only_the_exposed_relation() {
	let event = encrypted_stream_event("@mindroom_helper:example.com", "completed", true);
	let normalized = mindroom_terminal_push_event(&event).expect("terminal encrypted event");
	let value: JsonValue = serde_json::from_str(normalized.json().get()).expect("event json");
	let content = value["content"]
		.as_object()
		.expect("content object");

	assert!(!content.contains_key("m.relates_to"));
	assert_eq!(
		content
			.get("ciphertext")
			.and_then(JsonValue::as_str),
		Some("encrypted payload")
	);
	assert_eq!(
		content
			.get("io.mindroom.stream_status")
			.and_then(JsonValue::as_str),
		Some("completed")
	);
}

#[test]
fn encrypted_gateway_content_matches_push_rule_normalization() {
	let event = encrypted_stream_event("@mindroom_helper:example.com", "completed", true);
	let value: JsonValue = serde_json::from_str(event.json().get()).expect("event json");
	let content =
		mindroom_terminal_push_content(&TimelineEventType::RoomEncrypted, &value["content"])
			.expect("terminal encrypted content");

	assert!(content.get("m.relates_to").is_none());
	assert_eq!(
		content
			.get("ciphertext")
			.and_then(JsonValue::as_str),
		Some("encrypted payload")
	);
}

#[test]
fn nonterminal_stream_events_are_suppressed_for_any_sender() {
	let encrypted_pending =
		encrypted_stream_event("@mindroom_helper:example.com", "pending", false);
	let encrypted_streaming =
		encrypted_stream_event("@mindroom_helper:example.com", "streaming", true);
	let remote_pending =
		encrypted_stream_event("@mindroom_helper:remote.example", "pending", false);
	let future_intermediate =
		encrypted_stream_event("@mindroom_helper:example.com", "paused", true);

	assert!(mindroom_push_suppressed(&encrypted_pending));
	assert!(mindroom_push_suppressed(&encrypted_streaming));
	assert!(mindroom_push_suppressed(&remote_pending));
	assert!(mindroom_push_suppressed(&future_intermediate));
}

#[tokio::test]
async fn terminal_mindroom_edit_uses_normal_message_push_rule() {
	let user = owned_user_id!("@user:example.com");
	let ruleset = Ruleset::server_default(&user);
	let context = PushConditionRoomCtx::new(
		owned_room_id!("!room:example.com"),
		uint!(3),
		user,
		"User".into(),
	);
	let event = stream_event("@mindroom_helper:example.com", "completed", "m.text");

	assert!(
		ruleset
			.get_actions(&event, &context)
			.await
			.is_empty()
	);
	let normalized = mindroom_terminal_push_event(&event).expect("terminal event");
	assert!(
		ruleset
			.get_actions(&normalized, &context)
			.await
			.iter()
			.any(|action| matches!(action, Action::Notify))
	);
}

#[tokio::test]
async fn terminal_encrypted_edit_uses_encrypted_message_push_rule() {
	let user = owned_user_id!("@user:example.com");
	let ruleset = Ruleset::server_default(&user);
	let context = PushConditionRoomCtx::new(
		owned_room_id!("!room:example.com"),
		uint!(3),
		user,
		"User".into(),
	);
	let event = encrypted_stream_event("@mindroom_helper:example.com", "completed", true);

	assert!(
		ruleset
			.get_actions(&event, &context)
			.await
			.is_empty()
	);
	let normalized = mindroom_terminal_push_event(&event).expect("terminal encrypted event");
	assert!(
		ruleset
			.get_actions(&normalized, &context)
			.await
			.iter()
			.any(|action| matches!(action, Action::Notify))
	);
}

fn clear_ring_event(
	sender: &str,
	event_id: &str,
	notification_type: &str,
	membership_id: Option<&str>,
) -> Raw<AnySyncTimelineEvent> {
	clear_ring_event_with_type(
		RTC_NOTIFICATION_EVENT_TYPE,
		sender,
		event_id,
		notification_type,
		membership_id,
	)
}

fn clear_ring_event_with_type(
	event_type: &str,
	sender: &str,
	event_id: &str,
	notification_type: &str,
	membership_id: Option<&str>,
) -> Raw<AnySyncTimelineEvent> {
	let mut content = json!({
		"lifetime": 30_000,
		"m.call.intent": "audio",
		"notification_type": notification_type,
		"sender_ts": 1_795_300_000_000_i64,
	});
	if let Some(membership_id) = membership_id {
		content
			.as_object_mut()
			.expect("content object")
			.insert(
				"m.relates_to".into(),
				json!({ "rel_type": "m.reference", "event_id": membership_id }),
			);
	}
	from_value(json!({
		"content": content,
		"event_id": event_id,
		"origin_server_ts": 1_795_300_000_000_i64,
		"sender": sender,
		"type": event_type,
	}))
	.expect("valid clear ring event")
}

fn encrypted_ring_event(
	sender: &str,
	event_id: &str,
	hint_notification_type: Option<&str>,
	membership_id: Option<&str>,
) -> Raw<AnySyncTimelineEvent> {
	encrypted_ring_event_with_type(
		"m.room.encrypted",
		sender,
		event_id,
		hint_notification_type,
		membership_id,
	)
}

fn encrypted_ring_event_with_type(
	event_type: &str,
	sender: &str,
	event_id: &str,
	hint_notification_type: Option<&str>,
	membership_id: Option<&str>,
) -> Raw<AnySyncTimelineEvent> {
	let mut content = json!({
		"algorithm": "m.megolm.v1.aes-sha2",
		"ciphertext": "encrypted ring payload",
		"device_id": "DEVICE",
		"sender_key": "sender key",
		"session_id": "session",
	});
	let obj = content.as_object_mut().expect("content object");
	if let Some(notification_type) = hint_notification_type {
		obj.insert(
			"io.mindroom.rtc.notification".into(),
			json!({
				"lifetime": 30_000,
				"m.call.intent": "audio",
				"notification_type": notification_type,
				"sender_ts": 1_795_300_000_000_i64,
			}),
		);
	}
	if let Some(membership_id) = membership_id {
		obj.insert(
			"m.relates_to".into(),
			json!({ "rel_type": "m.reference", "event_id": membership_id }),
		);
	}
	from_value(json!({
		"content": content,
		"event_id": event_id,
		"origin_server_ts": 1_795_300_000_000_i64,
		"sender": sender,
		"type": event_type,
	}))
	.expect("valid encrypted ring event")
}

fn has_ring_sound(actions: &[Action]) -> bool {
	actions.iter().any(|action| {
		matches!(action, Action::SetTweak(Tweak::Sound(sound)) if sound.as_str() == RING_SOUND_TWEAK)
	})
}

fn has_highlight(actions: &[Action]) -> bool {
	actions
		.iter()
		.any(|action| matches!(action, Action::SetTweak(Tweak::Highlight(_))))
}

#[test]
fn ring_rules_match_spec_json_and_position_before_user_mention() {
	let user = owned_user_id!("@bob:example.com");
	let ruleset = ring_rules::server_default(&user);

	let clear_idx = ruleset
		.override_
		.get_index_of(RING_RULE_ID)
		.expect("clear ring rule present");
	let enc_idx = ruleset
		.override_
		.get_index_of(RING_ENCRYPTED_RULE_ID)
		.expect("encrypted ring rule present");
	let mention_idx = ruleset
		.override_
		.get_index_of(PredefinedOverrideRuleId::IsUserMention.as_str())
		.expect("user mention rule present");

	assert_eq!(clear_idx.saturating_add(1), enc_idx);
	assert_eq!(enc_idx.saturating_add(1), mention_idx);

	let clear_json = serde_json::to_value(
		ruleset
			.override_
			.get(RING_RULE_ID)
			.expect("clear ring rule"),
	)
	.expect("serialize clear rule");
	assert_eq!(
		clear_json,
		json!({
			"rule_id": ".io.mindroom.rule.rtc.ring",
			"default": true,
			"enabled": true,
			"conditions": [
				{ "kind": "event_property_is", "key": "type", "value": "org.matrix.msc4075.rtc.notification" },
				{ "kind": "event_property_is", "key": "content.notification_type", "value": "ring" },
				{ "kind": "room_member_count", "is": format!("<={MAX_RING_ROOM_MEMBERS}") },
			],
			"actions": ["notify", { "set_tweak": "sound", "value": "ring" }],
		})
	);

	let enc_json = serde_json::to_value(
		ruleset
			.override_
			.get(RING_ENCRYPTED_RULE_ID)
			.expect("encrypted ring rule"),
	)
	.expect("serialize encrypted rule");
	assert_eq!(
		enc_json,
		json!({
			"rule_id": ".io.mindroom.rule.rtc.ring_encrypted",
			"default": true,
			"enabled": true,
			"conditions": [
				{ "kind": "event_property_is", "key": "type", "value": "m.room.encrypted" },
				{
					"kind": "event_property_is",
					"key": r"content.io\.mindroom\.rtc\.notification.notification_type",
					"value": "ring",
				},
				{ "kind": "room_member_count", "is": format!("<={MAX_RING_ROOM_MEMBERS}") },
			],
			"actions": ["notify", { "set_tweak": "sound", "value": "ring" }],
		})
	);
}

#[tokio::test]
async fn ring_rules_require_exact_case_sensitive_type_and_notification_type() {
	let user = owned_user_id!("@bob:example.com");
	let ruleset = ring_rules::server_default(&user);
	let ctx = PushConditionRoomCtx::new(
		owned_room_id!("!room:example.com"),
		uint!(2),
		user,
		"Bob".into(),
	);

	let upper_clear = clear_ring_event_with_type(
		"ORG.MATRIX.MSC4075.RTC.NOTIFICATION",
		"@alice:example.com",
		"$upper_clear",
		"ring",
		Some("$mem"),
	);
	assert!(
		!has_ring_sound(ruleset.get_actions(&upper_clear, &ctx).await),
		"case variant of RTC notification type must not match ring rule"
	);

	let upper_notif =
		clear_ring_event("@alice:example.com", "$upper_notif", "RING", Some("$mem"));
	assert!(
		!has_ring_sound(ruleset.get_actions(&upper_notif, &ctx).await),
		"case variant of notification_type must not match ring rule"
	);

	let upper_enc = encrypted_ring_event_with_type(
		"M.ROOM.ENCRYPTED",
		"@alice:example.com",
		"$upper_enc",
		Some("ring"),
		Some("$mem"),
	);
	assert!(
		!has_ring_sound(ruleset.get_actions(&upper_enc, &ctx).await),
		"case variant of m.room.encrypted must not match encrypted ring rule"
	);

	let upper_enc_notif = encrypted_ring_event(
		"@alice:example.com",
		"$upper_enc_notif",
		Some("RING"),
		Some("$mem"),
	);
	assert!(
		!has_ring_sound(ruleset.get_actions(&upper_enc_notif, &ctx).await),
		"case variant of encrypted hint notification_type must not match ring rule"
	);
}

#[tokio::test]
async fn ring_rules_match_small_rooms_and_ignore_four_members_and_group_notifications() {
	let user = owned_user_id!("@bob:example.com");
	let ruleset = ring_rules::server_default(&user);
	let clear = clear_ring_event("@alice:example.com", "$ring_clear", "ring", Some("$mem"));
	let encrypted =
		encrypted_ring_event("@alice:example.com", "$ring_enc", Some("ring"), Some("$mem"));
	let group_notif =
		clear_ring_event("@alice:example.com", "$group_notif", "notification", Some("$mem"));

	for count in [2_u32, MAX_RING_ROOM_MEMBERS] {
		let ctx = PushConditionRoomCtx::new(
			owned_room_id!("!room:example.com"),
			UInt::from(count),
			user.clone(),
			"Bob".into(),
		);

		let clear_actions = ruleset.get_actions(&clear, &ctx).await;
		assert!(clear_actions.iter().any(Action::should_notify));
		assert!(has_ring_sound(clear_actions));
		assert!(!has_highlight(clear_actions));

		let enc_actions = ruleset.get_actions(&encrypted, &ctx).await;
		assert!(enc_actions.iter().any(Action::should_notify));
		assert!(has_ring_sound(enc_actions));
		assert!(!has_highlight(enc_actions));

		assert!(
			ruleset
				.get_actions(&group_notif, &ctx)
				.await
				.is_empty()
		);
	}

	let four_members_ctx = PushConditionRoomCtx::new(
		owned_room_id!("!room:example.com"),
		UInt::from(MAX_RING_ROOM_MEMBERS.saturating_add(1)),
		user,
		"Bob".into(),
	);
	assert!(
		ruleset
			.get_actions(&clear, &four_members_ctx)
			.await
			.is_empty()
	);
	let enc_four = ruleset
		.get_actions(&encrypted, &four_members_ctx)
		.await;
	assert!(enc_four.iter().any(Action::should_notify));
	assert!(!has_ring_sound(enc_four));
}

#[tokio::test]
async fn room_mute_override_wins_before_and_after_ring_rule_merge() {
	let user = owned_user_id!("@bob:example.com");
	let room_id = owned_room_id!("!muted:example.com");

	let mut stored = Ruleset::server_default(&user);
	stored
		.insert(
			NewPushRule::Override(NewConditionalPushRule::new(
				room_id.as_str().into(),
				vec![PushCondition::EventMatch(EventMatchConditionData::new(
					"room_id".into(),
					room_id.to_string(),
				))]
				.into(),
				Vec::new().into(),
			)),
			None,
			None,
		)
		.expect("insert mute override");

	let merged = ring_rules::merged(stored);
	let ctx = PushConditionRoomCtx::new(room_id, uint!(2), user, "Bob".into());

	let clear = clear_ring_event("@alice:example.com", "$ring_clear", "ring", Some("$mem"));
	let encrypted =
		encrypted_ring_event("@alice:example.com", "$ring_enc", Some("ring"), Some("$mem"));

	assert!(merged.get_actions(&clear, &ctx).await.is_empty());
	assert!(
		merged
			.get_actions(&encrypted, &ctx)
			.await
			.is_empty()
	);
}

#[test]
fn ring_permitted_needs_ring_actions_a_known_small_count_and_no_room_mute() {
	let user = owned_user_id!("@bob:example.com");
	let room = owned_room_id!("!room:example.com");
	let muted_room = owned_room_id!("!muted:example.com");
	let disabled_mute_room = owned_room_id!("!disabled:example.com");

	let mut ruleset = ring_rules::server_default(&user);
	for room_id in [&muted_room, &disabled_mute_room] {
		ruleset
			.insert(
				NewPushRule::Room(NewSimplePushRule::new(room_id.clone(), Vec::new().into())),
				None,
				None,
			)
			.expect("insert room mute");
	}
	ruleset
		.set_enabled(RuleKind::Room, disabled_mute_room.as_str(), false)
		.expect("disable room mute");

	let ring = ruleset
		.get(RuleKind::Override, RING_RULE_ID)
		.expect("ring rule")
		.actions();
	let max = u64::from(MAX_RING_ROOM_MEMBERS);

	assert!(ring_permitted(ring, &ruleset, &room, Some(2)));
	assert!(ring_permitted(ring, &ruleset, &room, Some(max)));
	assert!(ring_permitted(ring, &ruleset, &disabled_mute_room, Some(2)));

	assert!(!ring_permitted(ring, &ruleset, &room, None), "unknown joined count");
	assert!(!ring_permitted(ring, &ruleset, &room, Some(max.saturating_add(1))));
	assert!(!ring_permitted(ring, &ruleset, &muted_room, Some(2)), "room-kind mute");

	let default_sound = Action::SetTweak(Tweak::Sound(SoundTweakValue::Default));
	let ring_sound = Action::SetTweak(Tweak::Sound(RING_SOUND_TWEAK.into()));
	for actions in [vec![], vec![Action::Notify], vec![Action::Notify, default_sound], vec![
		ring_sound,
	]] {
		assert!(!ring_permitted(&actions, &ruleset, &room, Some(2)), "{actions:?}");
	}
}

#[test]
fn legacy_repair_keeps_stored_ring_rules_and_adds_no_missing_one() {
	let user = owned_user_id!("@bob:example.com");

	// Stored: the plain ring rule disabled, the encrypted one with custom actions.
	let mut stored = ring_rules::server_default(&user);
	stored
		.set_enabled(RuleKind::Override, RING_RULE_ID, false)
		.expect("disable ring rule");
	stored
		.set_actions(RuleKind::Override, RING_ENCRYPTED_RULE_ID, vec![Action::Notify].into())
		.expect("customize encrypted ring rule");

	ring_rules::update_with_server_default(&mut stored, &user);
	let repaired = ring_rules::merged(stored);
	assert!(
		!repaired
			.get(RuleKind::Override, RING_RULE_ID)
			.expect("ring rule kept")
			.enabled()
	);
	assert!(matches!(
		repaired
			.get(RuleKind::Override, RING_ENCRYPTED_RULE_ID)
			.expect("encrypted ring rule kept")
			.actions(),
		[Action::Notify]
	));
	let ids: Vec<_> = repaired
		.override_
		.iter()
		.map(|rule| rule.rule_id.as_str())
		.collect();
	let ring_at = ids
		.iter()
		.position(|id| *id == RING_RULE_ID)
		.expect("ring rule position");
	assert_eq!(ids[ring_at..ring_at.saturating_add(3)], [
		RING_RULE_ID,
		RING_ENCRYPTED_RULE_ID,
		PredefinedOverrideRuleId::IsUserMention.as_str()
	]);

	// Stored without ring rules: the repair persists none.
	let mut old = Ruleset::server_default(&user);
	ring_rules::update_with_server_default(&mut old, &user);
	assert!(
		old.get(RuleKind::Override, RING_RULE_ID)
			.is_none()
	);
	assert!(
		old.get(RuleKind::Override, RING_ENCRYPTED_RULE_ID)
			.is_none()
	);
}

#[tokio::test]
async fn merged_preserves_genuinely_legacy_ruleset_and_appends_when_mention_absent() {
	let user = owned_user_id!("@bob:example.com");
	let mut legacy = Ruleset::server_default(&user);
	legacy
		.override_
		.shift_remove(PredefinedOverrideRuleId::IsUserMention.as_str());
	#[expect(deprecated)]
	legacy.override_.insert(
		ConditionalPushRuleInit {
			actions: vec![
				Action::Notify,
				Action::SetTweak(Tweak::Sound(SoundTweakValue::Default)),
				Action::SetTweak(Tweak::Highlight(HighlightTweakValue::Yes)),
			]
			.into(),
			default: true,
			enabled: true,
			rule_id: PredefinedOverrideRuleId::ContainsDisplayName
				.as_str()
				.into(),
			conditions: vec![PushCondition::ContainsDisplayName].into(),
		}
		.into(),
	);
	legacy
		.set_enabled(
			RuleKind::Override,
			PredefinedOverrideRuleId::SuppressNotices.as_str(),
			false,
		)
		.expect("disable suppress_notices");
	let custom_message_actions =
		vec![Action::Notify, Action::SetTweak(Tweak::Sound(SoundTweakValue::Default))];
	legacy
		.set_actions(RuleKind::Underride, ".m.rule.message", custom_message_actions.into())
		.expect("customize message rule");

	let before_ids: Vec<String> = legacy
		.override_
		.iter()
		.map(|r| r.rule_id.to_string())
		.collect();

	let merged = ring_rules::merged(legacy);

	// All pre-existing override rules stay in their exact original order,
	// with the two ring rules appended at the end since .m.rule.is_user_mention is absent.
	let after_ids: Vec<String> = merged
		.override_
		.iter()
		.map(|r| r.rule_id.to_string())
		.collect();
	let mut expected_ids = before_ids;
	expected_ids.push(RING_RULE_ID.to_owned());
	expected_ids.push(RING_ENCRYPTED_RULE_ID.to_owned());
	assert_eq!(after_ids, expected_ids);

	// Deprecated rule and custom enabled/action states are preserved untouched.
	#[expect(deprecated)]
	let contains_dn = PredefinedOverrideRuleId::ContainsDisplayName.as_str();
	assert!(
		merged
			.get(RuleKind::Override, contains_dn)
			.is_some()
	);
	assert!(
		!merged
			.get(RuleKind::Override, PredefinedOverrideRuleId::SuppressNotices.as_str())
			.expect("suppress_notices preserved")
			.enabled()
	);

	// Ordinary message matching .m.rule.contains_display_name evaluates unchanged.
	let ctx = PushConditionRoomCtx::new(
		owned_room_id!("!room:example.com"),
		uint!(2),
		user,
		"Bob".into(),
	);
	let msg: Raw<AnySyncTimelineEvent> = from_value(json!({
		"content": { "msgtype": "m.text", "body": "hey Bob" },
		"event_id": "$msg1",
		"origin_server_ts": 1,
		"sender": "@alice:example.com",
		"type": "m.room.message",
	}))
	.expect("valid message");
	let actions = merged.get_actions(&msg, &ctx).await;
	assert!(actions.iter().any(Action::should_notify));
	assert!(has_highlight(actions));

	// Ring rules are present and match.
	let clear = clear_ring_event("@alice:example.com", "$ring_legacy", "ring", Some("$mem"));
	assert!(has_ring_sound(merged.get_actions(&clear, &ctx).await));
}

#[test]
fn merged_adds_missing_ring_rules_and_preserves_user_overrides_and_state() {
	let user = owned_user_id!("@bob:example.com");
	let mut old = Ruleset::server_default(&user);
	old.set_enabled(RuleKind::Override, PredefinedOverrideRuleId::IsUserMention.as_str(), false)
		.expect("disable user mention");

	let mut merged = ring_rules::merged(old);
	assert!(
		merged
			.get(RuleKind::Override, RING_RULE_ID)
			.is_some_and(ruma::push::AnyPushRuleRef::enabled)
	);
	assert!(
		merged
			.get(RuleKind::Override, RING_ENCRYPTED_RULE_ID)
			.is_some_and(ruma::push::AnyPushRuleRef::enabled)
	);
	assert!(
		!merged
			.get(RuleKind::Override, PredefinedOverrideRuleId::IsUserMention.as_str())
			.expect("user mention rule preserved")
			.enabled()
	);

	// Disable clear ring and customize encrypted ring, then re-merge.
	merged
		.set_enabled(RuleKind::Override, RING_RULE_ID, false)
		.expect("disable clear ring");
	let custom_actions =
		vec![Action::Notify, Action::SetTweak(Tweak::Sound(SoundTweakValue::Default))];
	merged
		.set_actions(RuleKind::Override, RING_ENCRYPTED_RULE_ID, custom_actions.into())
		.expect("customize encrypted ring actions");

	let remerged = ring_rules::merged(merged);
	assert!(
		!remerged
			.get(RuleKind::Override, RING_RULE_ID)
			.expect("clear ring present")
			.enabled()
	);
	let enc_actions = remerged
		.get(RuleKind::Override, RING_ENCRYPTED_RULE_ID)
		.expect("encrypted ring present")
		.actions();
	assert!(enc_actions.iter().any(|action| {
		matches!(action, Action::SetTweak(Tweak::Sound(SoundTweakValue::Default)))
	}));
}

#[test]
fn accepted_rings_allows_same_ring_rejects_second_and_expires_after_two_minutes() {
	let claims = AcceptedRings::default();
	let membership = EventId::parse("$membership_a").expect("event id");
	let other_membership = EventId::parse("$membership_b").expect("event id");
	let ring1 = EventId::parse("$ring_1").expect("event id");
	let ring2 = EventId::parse("$ring_2").expect("event id");

	let t0 = Instant::now();
	let later = t0
		.checked_add(Duration::from_secs(10))
		.expect("instant");
	let expired = t0
		.checked_add(Duration::from_secs(121))
		.expect("instant");
	assert!(claims.claim(&membership, &ring1, t0));
	assert!(
		claims.claim(&membership, &ring1, later),
		"the same ring passes again at dispatch"
	);
	assert!(!claims.claim(&membership, &ring2, later), "a second ring is rejected");
	assert!(claims.claim(&other_membership, &ring2, later), "memberships are independent");
	assert!(claims.claim(&membership, &ring2, expired), "claims expire after two minutes");
}

#[test]
fn call_membership_liveness_checks_shape_leave_and_expiry() {
	let origin_ts: u64 = 1_795_300_000_000;
	let now_ms = origin_ts.saturating_add(10_000);
	let live =
		|content: JsonValue, now_ms: u64| is_live_call_membership(&content, origin_ts, now_ms);
	let member = |extra: JsonValue| {
		let mut content = json!({
			"application": "m.call",
			"call_id": "",
			"device_id": "DEV1",
			"focus_active": { "type": "livekit", "focus_selection": "oldest_membership" },
			"foci_preferred": [],
			"scope": "m.room",
		});
		content
			.as_object_mut()
			.expect("content object")
			.extend(extra.as_object().expect("extra object").clone());
		content
	};

	// The per-device MSC4143 form, with and without an explicit lifetime.
	assert!(live(member(json!({ "expires": 60_000 })), now_ms));
	assert!(live(member(json!({})), now_ms));
	assert!(
		!live(member(json!({})), origin_ts.saturating_add(DEFAULT_CALL_MEMBERSHIP_EXPIRE_MS)),
		"the default lifetime counts from origin_server_ts"
	);

	// Expired, counted from `created_ts` when present and `origin_server_ts` otherwise.
	let created_ts = origin_ts.saturating_sub(120_000);
	assert!(!live(member(json!({ "created_ts": created_ts, "expires": 60_000 })), now_ms));
	assert!(!live(member(json!({ "expires": 5_000 })), now_ms));
	assert!(!live(member(json!({ "expires": 0 })), now_ms));

	// Left, another application, or not the per-device form.
	assert!(!live(json!({}), now_ms));
	assert!(!live(json!({ "leave_reason": "user_left" }), now_ms));
	assert!(!live(member(json!({ "leave_reason": "user_left" })), now_ms));
	assert!(!live(member(json!({ "application": "m.other" })), now_ms));
	assert!(!live(member(json!({ "application": 42 })), now_ms));
	assert!(!live(member(json!({ "device_id": "" })), now_ms));
	assert!(!live(member(json!({ "expires": "soon" })), now_ms));
	assert!(!live(json!({ "application": "m.call", "call_id": "" }), now_ms));
	assert!(
		!live(json!({ "memberships": [member(json!({ "expires": 60_000 }))] }), now_ms),
		"the legacy memberships array is not supported"
	);
}
