#![cfg(test)]

use std::time::{Duration, Instant};

use ruma::{
	EventId, RoomId, UInt, UserId,
	events::{AnySyncTimelineEvent, TimelineEventType},
	owned_room_id, owned_user_id,
	push::{
		Action, EventMatchConditionData, NewConditionalPushRule, NewPushRule,
		PredefinedOverrideRuleId, PushCondition, PushConditionRoomCtx, RuleKind, Ruleset,
		SoundTweakValue, Tweak,
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
		self, AcceptedRings, MAX_RING_ROOM_MEMBERS, RING_ENCRYPTED_RULE_ID, RING_HINT_KEY,
		RING_RULE_ID, RING_SOUND_TWEAK, RTC_NOTIFICATION_EVENT_TYPE, RingKind,
		detect_ring_candidate, strip_ring_marker,
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
		"type": RTC_NOTIFICATION_EVENT_TYPE,
	}))
	.expect("valid clear ring event")
}

fn encrypted_ring_event(
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
			RING_HINT_KEY.into(),
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
		"type": "m.room.encrypted",
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
				{ "kind": "event_match", "key": "type", "pattern": "org.matrix.msc4075.rtc.notification" },
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
				{ "kind": "event_match", "key": "type", "pattern": "m.room.encrypted" },
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

		assert!(ruleset.get_actions(&group_notif, &ctx).await.is_empty());
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

	let merged = ring_rules::merged(&user, stored);
	let ctx = PushConditionRoomCtx::new(room_id, uint!(2), user, "Bob".into());

	let clear = clear_ring_event("@alice:example.com", "$ring_clear", "ring", Some("$mem"));
	let encrypted =
		encrypted_ring_event("@alice:example.com", "$ring_enc", Some("ring"), Some("$mem"));

	assert!(merged.get_actions(&clear, &ctx).await.is_empty());
	assert!(merged.get_actions(&encrypted, &ctx).await.is_empty());
}

#[test]
fn merged_adds_missing_ring_rules_and_preserves_user_overrides_and_state() {
	let user = owned_user_id!("@bob:example.com");
	let mut old = Ruleset::server_default(&user);
	old.set_enabled(RuleKind::Override, PredefinedOverrideRuleId::IsUserMention.as_str(), false)
		.expect("disable user mention");

	let mut merged = ring_rules::merged(&user, old);
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

	// Disable clear ring, then ensure a subsequent merge preserves that choice.
	merged
		.set_enabled(RuleKind::Override, RING_RULE_ID, false)
		.expect("disable clear ring");
	let custom_actions = vec![
		Action::Notify,
		Action::SetTweak(Tweak::Sound(SoundTweakValue::Default)),
	];
	merged
		.set_actions(RuleKind::Override, RING_ENCRYPTED_RULE_ID, custom_actions.into())
		.expect("customize encrypted ring actions");

	let remerged = ring_rules::merged(&user, merged);
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
	let tracker = AcceptedRings::default();
	let membership = EventId::parse("$membership_a").expect("event id");
	let other_membership = EventId::parse("$membership_b").expect("event id");
	let ring1 = EventId::parse("$ring_1").expect("event id");
	let ring2 = EventId::parse("$ring_2").expect("event id");

	let t0 = Instant::now();
	assert!(tracker.accept_at(&membership, &ring1, t0));
	assert!(
		tracker.accept_at(&membership, &ring1, t0.checked_add(Duration::from_secs(10)).unwrap()),
		"the same ring event passes again at dispatch"
	);
	assert!(
		!tracker.accept_at(&membership, &ring2, t0.checked_add(Duration::from_secs(10)).unwrap()),
		"a second ring event for the same membership is rejected"
	);
	assert!(
		tracker.accept_at(
			&other_membership,
			&ring2,
			t0.checked_add(Duration::from_secs(10)).unwrap(),
		),
		"another call membership is tracked independently"
	);
	assert!(
		tracker.accept_at(&membership, &ring2, t0.checked_add(Duration::from_secs(121)).unwrap()),
		"entries expire after two minutes"
	);
}

#[test]
fn ring_marker_detection_and_stripping_preserves_other_events() {
	let clear = clear_ring_event("@alice:example.com", "$ring_clear", "ring", None);
	let (kind, value) = detect_ring_candidate(&clear).expect("clear ring detected");
	assert_eq!(kind, RingKind::Clear);
	let stripped = strip_ring_marker(kind, value).expect("strip clear ring");
	assert!(detect_ring_candidate(&stripped).is_none());

	let encrypted = encrypted_ring_event("@alice:example.com", "$ring_enc", Some("ring"), None);
	let (kind, value) = detect_ring_candidate(&encrypted).expect("encrypted ring detected");
	assert_eq!(kind, RingKind::Encrypted);
	let stripped = strip_ring_marker(kind, value).expect("strip encrypted ring");
	assert!(detect_ring_candidate(&stripped).is_none());

	let group_notif = clear_ring_event("@alice:example.com", "$group", "notification", None);
	assert!(detect_ring_candidate(&group_notif).is_none());

	let plain_encrypted = encrypted_ring_event("@alice:example.com", "$enc", None, None);
	assert!(detect_ring_candidate(&plain_encrypted).is_none());
}
