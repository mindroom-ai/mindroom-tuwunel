//! Default push rules and validation for MatrixRTC incoming-call rings.
//!
//! Two MindRoom server-default override rules turn clear and hint-carrying
//! encrypted ring events in small rooms into `sound: "ring"` pushes. During
//! push-rule evaluation in [`Service::get_actions`], when one of these two
//! rules matches, the server checks room-kind mute and validates that the
//! event references the sender's live, unexpired
//! `org.matrix.msc3401.call.member` state event in the same room while the
//! sender is still joined, allowing at most one audible ring per call
//! membership. If validation fails or the room is muted, those two rules are
//! skipped and evaluation continues against the original unmutated event.

use std::{
	collections::HashMap,
	sync::{Mutex, MutexGuard, PoisonError},
	time::{Duration, Instant},
};

use ruma::{
	EventId, OwnedEventId, OwnedUserId, RoomId, UInt, UserId,
	events::{AnySyncTimelineEvent, StateEventType},
	push::{
		Action, AnyPushRuleRef, ConditionalPushRule, ConditionalPushRuleInit,
		EventPropertyIsConditionData, PredefinedOverrideRuleId, PushCondition,
		RoomMemberCountConditionData, RoomMemberCountIs, RuleKind, Ruleset, ScalarJsonValue,
		SoundTweakValue, Tweak,
	},
	serde::Raw,
};
use serde::Deserialize;
use serde_json::{Map as JsonMap, Value as JsonValue};
use tuwunel_core::{implement, utils::time::now_millis};
use tuwunel_matrix::Event;

/// Maximum joined room member count eligible to trigger an incoming-call ring.
pub const MAX_RING_ROOM_MEMBERS: u32 = 3;

/// MSC4075 MatrixRTC notification event type.
pub const RTC_NOTIFICATION_EVENT_TYPE: &str = "org.matrix.msc4075.rtc.notification";

/// MSC3401 MatrixRTC call membership state event type.
pub const CALL_MEMBER_EVENT_TYPE: &str = "org.matrix.msc3401.call.member";

/// Cleartext hint field carried on encrypted ring events.
pub const RING_HINT_KEY: &str = "io.mindroom.rtc.notification";

/// Server-default override rule ID for unencrypted incoming-call rings.
pub const RING_RULE_ID: &str = ".io.mindroom.rule.rtc.ring";

/// Server-default override rule ID for encrypted incoming-call rings carrying the hint.
pub const RING_ENCRYPTED_RULE_ID: &str = ".io.mindroom.rule.rtc.ring_encrypted";

/// Push sound tweak value emitted for validated incoming-call rings.
pub const RING_SOUND_TWEAK: &str = "ring";

/// Default call membership lifetime when `expires` is omitted (4 hours, matching
/// matrix-js-sdk `DEFAULT_EXPIRE_DURATION`).
pub const DEFAULT_CALL_MEMBERSHIP_EXPIRE_MS: u64 = 4 * 60 * 60 * 1_000;

const RING_NOTIFICATION_TYPE: &str = "ring";
const ENCRYPTED_EVENT_TYPE: &str = "m.room.encrypted";
const ENCRYPTED_RING_PROPERTY_KEY: &str =
	r"content.io\.mindroom\.rtc\.notification.notification_type";
const RING_DEDUP_TTL: Duration = Duration::from_mins(2);

type AcceptedRingMap = HashMap<OwnedEventId, (OwnedEventId, Instant)>;

#[derive(Deserialize)]
struct RingClaimEvent {
	event_id: OwnedEventId,
	sender: OwnedUserId,
	content: RingClaimContent,
}

#[derive(Deserialize)]
struct RingClaimContent {
	#[serde(rename = "m.relates_to")]
	relates_to: Option<RingRelatesTo>,
}

#[derive(Deserialize)]
struct RingRelatesTo {
	rel_type: Option<String>,
	event_id: Option<OwnedEventId>,
}

/// In-memory map of accepted call membership event IDs to their single ring
/// event ID, bounded by a 2-minute TTL.
#[derive(Default)]
pub(super) struct AcceptedRings {
	inner: Mutex<AcceptedRingMap>,
}

impl AcceptedRings {
	fn lock(&self) -> MutexGuard<'_, AcceptedRingMap> {
		self.inner
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
	}

	pub(super) fn accept(&self, membership_id: &EventId, ring_event_id: &EventId) -> bool {
		self.accept_at(membership_id, ring_event_id, Instant::now())
	}

	pub(super) fn accept_at(
		&self,
		membership_id: &EventId,
		ring_event_id: &EventId,
		now: Instant,
	) -> bool {
		let mut map = self.lock();
		map.retain(|_, (_, accepted_at)| {
			now.saturating_duration_since(*accepted_at) < RING_DEDUP_TTL
		});

		if let Some((accepted_ring_id, _)) = map.get(membership_id) {
			return accepted_ring_id == ring_event_id;
		}

		map.insert(membership_id.to_owned(), (ring_event_id.to_owned(), now));
		true
	}
}

/// Ruma's server-default push ruleset with MindRoom's two call-ring override
/// rules inserted immediately before `.m.rule.is_user_mention`.
#[must_use]
pub fn server_default(user_id: &UserId) -> Ruleset { merged(Ruleset::server_default(user_id)) }

/// Insert missing MindRoom call-ring override rules into a stored ruleset.
///
/// Missing ring rules are inserted right before `.m.rule.is_user_mention` (or
/// at the end of `override` if absent), leaving every other stored rule,
/// enabled flag, action, and relative order untouched.
#[must_use]
pub fn merged(mut ruleset: Ruleset) -> Ruleset {
	insert_override_rule_before(&mut ruleset, ring_rule(), &[
		RING_ENCRYPTED_RULE_ID,
		PredefinedOverrideRuleId::IsUserMention.as_str(),
	]);
	insert_override_rule_before(&mut ruleset, ring_encrypted_rule(), &[
		PredefinedOverrideRuleId::IsUserMention.as_str(),
	]);
	ruleset
}

fn insert_override_rule_before(
	ruleset: &mut Ruleset,
	rule: ConditionalPushRule,
	before_rule_ids: &[&str],
) {
	if ruleset.override_.contains(rule.rule_id.as_str()) {
		return;
	}

	let target_idx = before_rule_ids
		.iter()
		.find_map(|id| ruleset.override_.get_index_of(*id));

	let (pos, _) = ruleset.override_.insert_full(rule);
	if let Some(idx) = target_idx {
		ruleset.override_.move_index(pos, idx);
	}
}

/// Whether `rule` is one of MindRoom's two call-ring override rules.
pub(super) fn is_ring_rule(rule: AnyPushRuleRef<'_>) -> bool {
	matches!(
		rule,
		AnyPushRuleRef::Override(rule)
			if rule.rule_id == RING_RULE_ID || rule.rule_id == RING_ENCRYPTED_RULE_ID
	)
}

/// Whether `ruleset` has an enabled room-kind mute rule for `room_id` (an
/// enabled `RuleKind::Room` rule whose actions do not notify).
pub(super) fn is_room_muted(ruleset: &Ruleset, room_id: &RoomId) -> bool {
	ruleset
		.get(RuleKind::Room, room_id.as_str())
		.is_some_and(|rule| rule.enabled() && !rule.triggers_notification())
}

fn ring_actions() -> ruma::push::Actions {
	[
		Action::Notify,
		Action::SetTweak(Tweak::Sound(SoundTweakValue::from(RING_SOUND_TWEAK))),
	]
	.into_iter()
	.collect()
}

fn ring_member_count_condition() -> PushCondition {
	PushCondition::RoomMemberCount(RoomMemberCountConditionData::new(RoomMemberCountIs::from(
		..=UInt::from(MAX_RING_ROOM_MEMBERS),
	)))
}

fn ring_rule() -> ConditionalPushRule {
	ConditionalPushRuleInit {
		actions: ring_actions(),
		default: true,
		enabled: true,
		rule_id: RING_RULE_ID.into(),
		conditions: vec![
			PushCondition::EventPropertyIs(EventPropertyIsConditionData::new(
				"type".into(),
				ScalarJsonValue::String(RTC_NOTIFICATION_EVENT_TYPE.into()),
			)),
			PushCondition::EventPropertyIs(EventPropertyIsConditionData::new(
				"content.notification_type".into(),
				ScalarJsonValue::String(RING_NOTIFICATION_TYPE.into()),
			)),
			ring_member_count_condition(),
		]
		.into(),
	}
	.into()
}

fn ring_encrypted_rule() -> ConditionalPushRule {
	ConditionalPushRuleInit {
		actions: ring_actions(),
		default: true,
		enabled: true,
		rule_id: RING_ENCRYPTED_RULE_ID.into(),
		conditions: vec![
			PushCondition::EventPropertyIs(EventPropertyIsConditionData::new(
				"type".into(),
				ScalarJsonValue::String(ENCRYPTED_EVENT_TYPE.into()),
			)),
			PushCondition::EventPropertyIs(EventPropertyIsConditionData::new(
				ENCRYPTED_RING_PROPERTY_KEY.into(),
				ScalarJsonValue::String(RING_NOTIFICATION_TYPE.into()),
			)),
			ring_member_count_condition(),
		]
		.into(),
	}
	.into()
}

fn extract_ring_claim(
	pdu: &Raw<AnySyncTimelineEvent>,
) -> Option<(OwnedEventId, OwnedUserId, OwnedEventId)> {
	let claim: RingClaimEvent = serde_json::from_str(pdu.json().get()).ok()?;
	let relates_to = claim.content.relates_to?;
	if relates_to.rel_type.as_deref() != Some("m.reference") {
		return None;
	}
	let membership_event_id = relates_to.event_id?;
	Some((claim.event_id, claim.sender, membership_event_id))
}

/// Check whether an `org.matrix.msc3401.call.member` content object describes a
/// live, unexpired call membership at `now_ms`.
///
/// Accepts both the MSC4143 per-device session shape written by matrix-js-sdk
/// 45.1.0 and the legacy `memberships` array shape. Rejects empty objects,
/// leave events (`leave_reason`), malformed entries, and expired entries.
pub(super) fn is_live_call_membership(
	content: &JsonValue,
	event_origin_server_ts_ms: u64,
	now_ms: u64,
) -> bool {
	let Some(obj) = content.as_object() else {
		return false;
	};
	if obj.contains_key("leave_reason") {
		return false;
	}

	if let Some(memberships) = obj.get("memberships") {
		return memberships.as_array().is_some_and(|list| {
			list.iter().any(|entry| {
				entry.as_object().is_some_and(|entry| {
					is_live_session_entry(entry, event_origin_server_ts_ms, now_ms)
				})
			})
		});
	}

	is_live_session_entry(obj, event_origin_server_ts_ms, now_ms)
}

fn is_live_session_entry(
	entry: &JsonMap<String, JsonValue>,
	event_origin_server_ts_ms: u64,
	now_ms: u64,
) -> bool {
	if !has_valid_session_fields(entry) {
		return false;
	}

	let base_ts = match entry.get("created_ts") {
		| None => event_origin_server_ts_ms,
		| Some(val) => {
			let Some(ts) = val.as_u64() else {
				return false;
			};
			ts
		},
	};

	let abs_expiry = match entry.get("expires") {
		| Some(val) => {
			let Some(expires_ms) = val.as_u64().filter(|&ms| ms > 0) else {
				return false;
			};
			base_ts.saturating_add(expires_ms)
		},
		| None => match entry.get("expires_ts") {
			| Some(val) => {
				let Some(ts) = val.as_u64().filter(|&ms| ms > 0) else {
					return false;
				};
				ts
			},
			| None => base_ts.saturating_add(DEFAULT_CALL_MEMBERSHIP_EXPIRE_MS),
		},
	};

	abs_expiry > now_ms
}

fn has_valid_session_fields(entry: &JsonMap<String, JsonValue>) -> bool {
	let non_empty_str = |key: &str| {
		entry
			.get(key)
			.and_then(JsonValue::as_str)
			.is_some_and(|s| !s.is_empty())
	};

	if !non_empty_str("device_id")
		|| !non_empty_str("application")
		|| entry
			.get("call_id")
			.and_then(JsonValue::as_str)
			.is_none()
	{
		return false;
	}

	let focus_active_ok = entry
		.get("focus_active")
		.and_then(JsonValue::as_object)
		.and_then(|focus| focus.get("type"))
		.and_then(JsonValue::as_str)
		.is_some_and(|s| !s.is_empty());
	if !focus_active_ok {
		return false;
	}

	if let Some(foci) = entry.get("foci_preferred") {
		let valid = foci.as_array().is_some_and(|list| {
			list.iter().all(|item| {
				item.as_object()
					.and_then(|obj| obj.get("type"))
					.and_then(JsonValue::as_str)
					.is_some_and(|s| !s.is_empty())
			})
		});
		if !valid {
			return false;
		}
	}

	if entry
		.get("scope")
		.is_some_and(|val| !val.is_string())
		|| entry
			.get("m.call.intent")
			.is_some_and(|val| !val.is_string())
	{
		return false;
	}

	if let Some(caps) = entry.get("capabilities") {
		let valid = caps
			.as_array()
			.is_some_and(|list| list.iter().all(JsonValue::is_string));
		if !valid {
			return false;
		}
	}

	true
}

/// Validate that a matched ring event points at the sender's live, unexpired
/// call membership in `room_id` while the sender is still joined, and claim
/// the single audible-ring slot for that membership.
#[implement(super::Service)]
pub(super) async fn ring_push_is_valid(
	&self,
	room_id: &RoomId,
	pdu: &Raw<AnySyncTimelineEvent>,
) -> bool {
	let Some((ring_event_id, sender, membership_event_id)) = extract_ring_claim(pdu) else {
		return false;
	};

	if !self
		.services
		.state_cache
		.is_joined(&sender, room_id)
		.await
	{
		return false;
	}

	let Ok(target) = self
		.services
		.timeline
		.get_pdu(&membership_event_id)
		.await
	else {
		return false;
	};

	if target.room_id() != room_id
		|| target.kind().to_cow_str() != CALL_MEMBER_EVENT_TYPE
		|| target.sender() != sender
		|| target.is_redacted()
	{
		return false;
	}

	let Some(state_key) = target.state_key() else {
		return false;
	};

	let event_ts_ms = u64::from(target.origin_server_ts().get());
	if !is_live_call_membership(&target.get_content_as_value(), event_ts_ms, now_millis()) {
		return false;
	}

	let state_event_type = StateEventType::from(CALL_MEMBER_EVENT_TYPE);
	let Ok(current_id) = self
		.services
		.state_accessor
		.room_state_get_id(room_id, &state_event_type, state_key)
		.await
	else {
		return false;
	};

	if current_id != membership_event_id {
		return false;
	}

	self.accepted_rings
		.accept(&membership_event_id, &ring_event_id)
}
