//! Push rules for MatrixRTC incoming-call rings.
//!
//! Two MindRoom server-default override rules give ring events in small rooms
//! the `sound: "ring"` tweak. The tweak is not authorization, so
//! [`Service::get_actions`](super::Service::get_actions) enforces this
//! invariant. A push gets `sound: "ring"` only if
//!
//! - (a) one of the two ring rules is the winning rule for this recipient,
//!   enabled, with `notify` and `sound: "ring"` actions;
//! - (b) the room's joined count is known and at most
//!   [`MAX_RING_ROOM_MEMBERS`];
//! - (c) no enabled room-kind or override mute for this room wins;
//! - (d) the ring `m.reference`s the sender's current, live MSC4143
//!   `call.member` state in that room and the sender is joined;
//! - (e) no other ring event already claimed that membership.
//!
//! Otherwise the ring rules are skipped and the event is evaluated exactly as
//! without them. A user turns rings off by disabling the rules or muting the
//! room.

use std::{
	collections::HashMap,
	sync::{Mutex, PoisonError},
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
use serde::{Deserialize, de::IgnoredAny};
use serde_json::Value as JsonValue;
use tuwunel_core::{implement, utils::time::now_millis};
use tuwunel_matrix::Event;

/// Maximum joined member count of a room in which a call rings.
pub const MAX_RING_ROOM_MEMBERS: u32 = 3;

/// MSC4075 MatrixRTC notification event type.
pub const RTC_NOTIFICATION_EVENT_TYPE: &str = "org.matrix.msc4075.rtc.notification";

/// MSC4143 MatrixRTC call membership state event type.
pub const CALL_MEMBER_EVENT_TYPE: &str = "org.matrix.msc3401.call.member";

/// Rule ID for unencrypted rings.
pub const RING_RULE_ID: &str = ".io.mindroom.rule.rtc.ring";

/// Rule ID for encrypted rings carrying the cleartext hint.
pub const RING_ENCRYPTED_RULE_ID: &str = ".io.mindroom.rule.rtc.ring_encrypted";

/// Sound tweak value of a ring push.
pub const RING_SOUND_TWEAK: &str = "ring";

/// Call membership lifetime when `expires` is omitted (matrix-js-sdk
/// `DEFAULT_EXPIRE_DURATION`).
pub const DEFAULT_CALL_MEMBERSHIP_EXPIRE_MS: u64 = 4 * 60 * 60 * 1_000;

const RING_CLAIM_TTL: Duration = Duration::from_mins(2);

#[derive(Deserialize)]
struct RingEvent {
	event_id: OwnedEventId,
	sender: OwnedUserId,
	content: RingContent,
}

#[derive(Deserialize)]
struct RingContent {
	#[serde(rename = "m.relates_to")]
	relates_to: RingRelation,
}

#[derive(Deserialize)]
struct RingRelation {
	rel_type: String,
	event_id: OwnedEventId,
}

/// The per-device MSC4143 `call.member` content matrix-js-sdk writes, with
/// the fields it requires. The older `memberships` array form is not
/// supported.
#[derive(Deserialize)]
struct CallMembership {
	application: String,
	device_id: String,
	#[serde(rename = "call_id")]
	_call_id: String,
	#[serde(rename = "focus_active")]
	_focus_active: FocusActive,
	created_ts: Option<u64>,
	expires: Option<u64>,
	leave_reason: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct FocusActive {
	#[serde(rename = "type")]
	_kind: String,
}

/// Call membership event ID to the one ring event that claimed it, kept for
/// two minutes.
#[derive(Default)]
pub(super) struct AcceptedRings(Mutex<HashMap<OwnedEventId, (OwnedEventId, Instant)>>);

impl AcceptedRings {
	/// Whether `ring_id` holds the claim on `membership_id`, claiming it if
	/// it is free. The same ring passes again for other recipients and at
	/// dispatch.
	pub(super) fn claim(&self, membership_id: &EventId, ring_id: &EventId, now: Instant) -> bool {
		let mut claims = self
			.0
			.lock()
			.unwrap_or_else(PoisonError::into_inner);

		claims.retain(|_, (_, at)| now.saturating_duration_since(*at) < RING_CLAIM_TTL);
		claims
			.entry(membership_id.to_owned())
			.or_insert_with(|| (ring_id.to_owned(), now))
			.0
			== ring_id
	}
}

/// Ruma's server-default ruleset plus the two ring rules.
#[must_use]
pub fn server_default(user_id: &UserId) -> Ruleset { merged(Ruleset::server_default(user_id)) }

/// Add the ring rules a stored ruleset lacks, right before
/// `.m.rule.is_user_mention` (or last in `override` without it). Nothing
/// else in the ruleset changes.
#[must_use]
pub fn merged(mut ruleset: Ruleset) -> Ruleset {
	for rule in ring_rules() {
		if ruleset.override_.contains(rule.rule_id.as_str()) {
			continue;
		}

		let before = ruleset
			.override_
			.get_index_of(PredefinedOverrideRuleId::IsUserMention.as_str());
		let (pos, _) = ruleset.override_.insert_full(rule);
		if let Some(before) = before {
			ruleset.override_.move_index(pos, before);
		}
	}

	ruleset
}

/// [`Ruleset::update_with_server_default`] for the legacy repair of the full
/// `GET /pushrules` routes.
///
/// Ruma's defaults alone would drop a stored ring rule, and [`merged`] would
/// then bring it back enabled. This keeps a stored ring rule with its
/// `enabled` and `actions`, and does not add a missing one.
pub fn update_with_server_default(ruleset: &mut Ruleset, user_id: &UserId) {
	let mut defaults = server_default(user_id);
	defaults.override_.retain(|rule| {
		!is_ring_rule_id(&rule.rule_id) || ruleset.override_.contains(rule.rule_id.as_str())
	});

	ruleset.update_with_server_default(defaults);
}

fn is_ring_rule_id(rule_id: &str) -> bool {
	rule_id == RING_RULE_ID || rule_id == RING_ENCRYPTED_RULE_ID
}

pub(super) fn is_ring_rule(rule: AnyPushRuleRef<'_>) -> bool {
	matches!(rule, AnyPushRuleRef::Override(rule) if is_ring_rule_id(&rule.rule_id))
}

fn ring_rules() -> [ConditionalPushRule; 2] {
	[
		ring_rule(RING_RULE_ID, RTC_NOTIFICATION_EVENT_TYPE, "content.notification_type"),
		ring_rule(
			RING_ENCRYPTED_RULE_ID,
			"m.room.encrypted",
			r"content.io\.mindroom\.rtc\.notification.notification_type",
		),
	]
}

fn ring_rule(
	rule_id: &str,
	event_type: &str,
	notification_type_key: &str,
) -> ConditionalPushRule {
	let property_is = |key: &str, value: &str| {
		PushCondition::EventPropertyIs(EventPropertyIsConditionData::new(
			key.into(),
			ScalarJsonValue::String(value.into()),
		))
	};

	ConditionalPushRuleInit {
		actions: [
			Action::Notify,
			Action::SetTweak(Tweak::Sound(SoundTweakValue::from(RING_SOUND_TWEAK))),
		]
		.into_iter()
		.collect(),
		default: true,
		enabled: true,
		rule_id: rule_id.into(),
		conditions: vec![
			property_is("type", event_type),
			property_is(notification_type_key, "ring"),
			PushCondition::RoomMemberCount(RoomMemberCountConditionData::new(
				RoomMemberCountIs::from(..=UInt::from(MAX_RING_ROOM_MEMBERS)),
			)),
		]
		.into(),
	}
	.into()
}

/// Parts (a) to (c) of the invariant for a ring rule that matched: its
/// actions still notify with the ring sound, the joined count is known and
/// small, and no enabled room-kind rule mutes the room. An override mute
/// needs no check here: user override rules precede the ring rules.
pub(super) fn ring_permitted(
	actions: &[Action],
	ruleset: &Ruleset,
	room_id: &RoomId,
	joined_count: Option<u64>,
) -> bool {
	// Exactly one sound tweak, so that no other sound can override the ring.
	let mut sounds = actions.iter().filter_map(|action| match action {
		| Action::SetTweak(Tweak::Sound(sound)) => Some(sound.as_str()),
		| _ => None,
	});
	let rings = actions.iter().any(Action::should_notify)
		&& sounds.next() == Some(RING_SOUND_TWEAK)
		&& sounds.next().is_none();

	let muted = ruleset
		.get(RuleKind::Room, room_id.as_str())
		.is_some_and(|rule| rule.enabled() && !rule.triggers_notification());

	rings && !muted && joined_count.is_some_and(|count| count <= MAX_RING_ROOM_MEMBERS.into())
}

/// Whether `content` is a per-device MSC4143 `m.call` membership that has
/// not left or expired at `now_ms`.
pub(super) fn is_live_call_membership(
	content: &JsonValue,
	origin_server_ts_ms: u64,
	now_ms: u64,
) -> bool {
	let Ok(member) = CallMembership::deserialize(content) else {
		return false;
	};

	let expires_at = member
		.created_ts
		.unwrap_or(origin_server_ts_ms)
		.saturating_add(
			member
				.expires
				.unwrap_or(DEFAULT_CALL_MEMBERSHIP_EXPIRE_MS),
		);

	member.application == "m.call"
		&& !member.device_id.is_empty()
		&& member.leave_reason.is_none()
		&& expires_at > now_ms
}

/// Whether the ring rule `rule`, which matched `pdu`, may ring. See the
/// module documentation. The claim (e) is taken last, so a ring that fails
/// any other part leaves the membership free.
#[implement(super::Service)]
pub(super) async fn ring_allowed(
	&self,
	rule: AnyPushRuleRef<'_>,
	ruleset: &Ruleset,
	room_id: &RoomId,
	pdu: &Raw<AnySyncTimelineEvent>,
) -> bool {
	// The evaluation context turns a failed count lookup into 0, which the
	// rule's own `<=3` condition accepts, so the count is read again here.
	let joined_count = self
		.services
		.state_cache
		.room_joined_count(room_id)
		.await
		.ok();

	if !ring_permitted(rule.actions(), ruleset, room_id, joined_count) {
		return false;
	}

	self.live_ring_membership(room_id, pdu)
		.await
		.is_some_and(|(membership_id, ring_id)| {
			self.accepted_rings
				.claim(&membership_id, &ring_id, Instant::now())
		})
}

/// Part (d) of the invariant: the sender's current, live call membership
/// that the ring `pdu` references, with the ring's own event ID.
#[implement(super::Service)]
async fn live_ring_membership(
	&self,
	room_id: &RoomId,
	pdu: &Raw<AnySyncTimelineEvent>,
) -> Option<(OwnedEventId, OwnedEventId)> {
	let ring: RingEvent = serde_json::from_str(pdu.json().get()).ok()?;
	let RingRelation { rel_type, event_id: membership_id } = ring.content.relates_to;
	if rel_type != "m.reference"
		|| !self
			.services
			.state_cache
			.is_joined(&ring.sender, room_id)
			.await
	{
		return None;
	}

	let member = self
		.services
		.timeline
		.get_pdu(&membership_id)
		.await
		.ok()?;
	let state_key = member.state_key()?;
	if member.room_id() != room_id
		|| member.kind().to_cow_str() != CALL_MEMBER_EVENT_TYPE
		|| member.sender() != ring.sender
		|| !is_live_call_membership(
			&member.get_content_as_value(),
			member.origin_server_ts().get().into(),
			now_millis(),
		) {
		return None;
	}

	let current: OwnedEventId = self
		.services
		.state_accessor
		.room_state_get_id(room_id, &StateEventType::from(CALL_MEMBER_EVENT_TYPE), state_key)
		.await
		.ok()?;

	(current == membership_id).then_some((membership_id, ring.event_id))
}
