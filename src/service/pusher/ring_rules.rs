//! Default push rules and validation for MatrixRTC incoming-call rings.
//!
//! Two MindRoom server-default override rules turn clear and hint-carrying
//! encrypted ring events in small rooms into `sound: "ring"` pushes. Before
//! push evaluation, [`Service::validate_ring_push`] checks that a ring points
//! via `m.reference` at the sender's live `org.matrix.msc3401.call.member`
//! state event in the same room and allows at most one audible ring per call
//! membership; failing candidates have their ring marker stripped so they
//! evaluate as they did before these rules existed.

use std::{
	collections::HashMap,
	sync::{Mutex, MutexGuard, PoisonError},
	time::{Duration, Instant},
};

use ruma::{
	EventId, OwnedEventId, OwnedUserId, RoomId, UInt, UserId,
	events::{AnySyncTimelineEvent, StateEventType},
	push::{
		Action, ConditionalPushRule, ConditionalPushRuleInit, EventMatchConditionData,
		EventPropertyIsConditionData, PredefinedOverrideRuleId, PushCondition,
		RoomMemberCountConditionData, RoomMemberCountIs, RuleKind, Ruleset, ScalarJsonValue,
		SoundTweakValue, Tweak,
	},
	serde::Raw,
};
use serde_json::{Value as JsonValue, value::to_raw_value};
use tuwunel_core::implement;
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

const RING_NOTIFICATION_TYPE: &str = "ring";
const ENCRYPTED_EVENT_TYPE: &str = "m.room.encrypted";
const ENCRYPTED_RING_PROPERTY_KEY: &str =
	r"content.io\.mindroom\.rtc\.notification.notification_type";
const RING_DEDUP_TTL: Duration = Duration::from_secs(120);

type AcceptedRingMap = HashMap<OwnedEventId, (OwnedEventId, Instant)>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RingKind {
	Clear,
	Encrypted,
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
pub fn server_default(user_id: &UserId) -> Ruleset {
	let mut ruleset = Ruleset::server_default(user_id);

	if let Some(mention_idx) = ruleset
		.override_
		.get_index_of(PredefinedOverrideRuleId::IsUserMention.as_str())
	{
		let (clear_pos, _) = ruleset.override_.insert_full(ring_rule());
		ruleset.override_.move_index(clear_pos, mention_idx);

		let (enc_pos, _) = ruleset
			.override_
			.insert_full(ring_encrypted_rule());
		ruleset
			.override_
			.move_index(enc_pos, mention_idx.saturating_add(1));
	} else {
		ruleset.override_.insert(ring_rule());
		ruleset.override_.insert(ring_encrypted_rule());
	}

	ruleset
}

/// Merge MindRoom's server-default call-ring rules into a stored ruleset when
/// either ring rule is missing, preserving user rules, enabled flags, and
/// actions.
#[must_use]
pub fn merged(user_id: &UserId, mut ruleset: Ruleset) -> Ruleset {
	if ruleset.get(RuleKind::Override, RING_RULE_ID).is_none()
		|| ruleset
			.get(RuleKind::Override, RING_ENCRYPTED_RULE_ID)
			.is_none()
	{
		ruleset.update_with_server_default(server_default(user_id));
	}

	ruleset
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
			PushCondition::EventMatch(EventMatchConditionData::new(
				"type".into(),
				RTC_NOTIFICATION_EVENT_TYPE.into(),
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
			PushCondition::EventMatch(EventMatchConditionData::new(
				"type".into(),
				ENCRYPTED_EVENT_TYPE.into(),
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

pub(super) fn detect_ring_candidate(
	pdu: &Raw<AnySyncTimelineEvent>,
) -> Option<(RingKind, JsonValue)> {
	let event = serde_json::to_value(pdu).ok()?;
	let event_type = event.get("type").and_then(JsonValue::as_str)?;
	let content = event.get("content")?.as_object()?;

	let kind = match event_type {
		| RTC_NOTIFICATION_EVENT_TYPE
			if content
				.get("notification_type")
				.and_then(JsonValue::as_str)
				== Some(RING_NOTIFICATION_TYPE) =>
			RingKind::Clear,
		| ENCRYPTED_EVENT_TYPE
			if content
				.get(RING_HINT_KEY)
				.and_then(JsonValue::as_object)
				.and_then(|hint| hint.get("notification_type"))
				.and_then(JsonValue::as_str)
				== Some(RING_NOTIFICATION_TYPE) =>
			RingKind::Encrypted,
		| _ => return None,
	};

	Some((kind, event))
}

pub(super) fn strip_ring_marker(
	kind: RingKind,
	mut event: JsonValue,
) -> Option<Raw<AnySyncTimelineEvent>> {
	let content = event.get_mut("content")?.as_object_mut()?;
	match kind {
		| RingKind::Clear => {
			content.remove("notification_type");
		},
		| RingKind::Encrypted => {
			content.remove(RING_HINT_KEY);
		},
	}

	to_raw_value(&event).ok().map(Raw::from_json)
}

fn extract_ring_claim(event: &JsonValue) -> Option<(OwnedEventId, OwnedUserId, OwnedEventId)> {
	let ring_event_id = event
		.get("event_id")
		.and_then(JsonValue::as_str)
		.and_then(|id| EventId::parse(id).ok())?;
	let sender = event
		.get("sender")
		.and_then(JsonValue::as_str)
		.and_then(|id| UserId::parse(id).ok())?;
	let relates_to = event
		.get("content")
		.and_then(JsonValue::as_object)
		.and_then(|content| content.get("m.relates_to"))
		.and_then(JsonValue::as_object)?;

	if relates_to.get("rel_type").and_then(JsonValue::as_str) != Some("m.reference") {
		return None;
	}

	let membership_event_id = relates_to
		.get("event_id")
		.and_then(JsonValue::as_str)
		.and_then(|id| EventId::parse(id).ok())?;

	Some((ring_event_id, sender, membership_event_id))
}

fn is_active_call_membership_content(content: &JsonValue) -> bool {
	let Some(obj) = content.as_object() else {
		return false;
	};
	if obj.is_empty() {
		return false;
	}
	if let Some(memberships) = obj.get("memberships") {
		return memberships
			.as_array()
			.is_some_and(|list| !list.is_empty());
	}
	true
}

/// Validate a ring candidate before push rule evaluation.
///
/// Returns `None` when the event is not a ring candidate or when it passes all
/// validation checks. Returns `Some(stripped)` with the ring marker removed
/// when a ring candidate fails validation so it evaluates with today's rules.
#[implement(super::Service)]
pub(super) async fn validate_ring_push(
	&self,
	room_id: &RoomId,
	pdu: &Raw<AnySyncTimelineEvent>,
) -> Option<Raw<AnySyncTimelineEvent>> {
	let (kind, event) = detect_ring_candidate(pdu)?;

	if self
		.ring_candidate_is_valid(room_id, &event)
		.await
	{
		return None;
	}

	strip_ring_marker(kind, event)
}

#[implement(super::Service)]
async fn ring_candidate_is_valid(&self, room_id: &RoomId, event: &JsonValue) -> bool {
	let Some((ring_event_id, sender, membership_event_id)) = extract_ring_claim(event) else {
		return false;
	};

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

	if !is_active_call_membership_content(&target.get_content_as_value()) {
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
