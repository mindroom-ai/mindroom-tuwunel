//! Push rules for MatrixRTC incoming-call rings.
//!
//! Two MindRoom server-default override rules give ring events in small rooms
//! the `sound: "ring"` tweak. [`Service::get_actions`](super::Service::get_actions)
//! lets a ring rule win only if
//!
//! - (a) it is the first rule that matches for this recipient, enabled, with
//!   `notify` and `"ring"` as its only sound tweak;
//! - (b) the room's joined count is known and at most
//!   [`MAX_RING_ROOM_MEMBERS`];
//! - (c) no enabled room-kind or override mute for this room wins.
//!
//! Otherwise the ring rules are skipped and the event is evaluated exactly as
//! without them. A user turns rings off by disabling the rules or muting the
//! room.
//!
//! The rules do not check that a call exists: a member who can send the
//! event can ring the others in the room.

use ruma::{
	RoomId, UInt, UserId,
	push::{
		Action, AnyPushRuleRef, ConditionalPushRule, ConditionalPushRuleInit,
		EventPropertyIsConditionData, PredefinedOverrideRuleId, PushCondition,
		RoomMemberCountConditionData, RoomMemberCountIs, RuleKind, Ruleset, ScalarJsonValue,
		SoundTweakValue, Tweak,
	},
};
use tuwunel_core::implement;

/// Maximum joined member count of a room in which a call rings.
pub const MAX_RING_ROOM_MEMBERS: u32 = 3;

/// MSC4075 MatrixRTC notification event type.
pub const RTC_NOTIFICATION_EVENT_TYPE: &str = "org.matrix.msc4075.rtc.notification";

/// Rule ID for unencrypted rings.
pub const RING_RULE_ID: &str = ".io.mindroom.rule.rtc.ring";

/// Rule ID for encrypted rings carrying the cleartext hint.
pub const RING_ENCRYPTED_RULE_ID: &str = ".io.mindroom.rule.rtc.ring_encrypted";

/// Sound tweak value of a ring push.
pub const RING_SOUND_TWEAK: &str = "ring";

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

/// The invariant for a ring rule that matched: its actions still notify with
/// the ring sound, the joined count is known and small, and no enabled
/// room-kind rule mutes the room. An override mute needs no check here: user
/// override rules precede the ring rules.
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

/// Whether the ring rule `rule`, which matched an event in `room_id`, may
/// ring. See the module documentation.
#[implement(super::Service)]
pub(super) async fn ring_allowed(
	&self,
	rule: AnyPushRuleRef<'_>,
	ruleset: &Ruleset,
	room_id: &RoomId,
) -> bool {
	// The evaluation context turns a failed count lookup into 0, which the
	// rule's own `<=3` condition accepts, so the count is read again here.
	let joined_count = self
		.services
		.state_cache
		.room_joined_count(room_id)
		.await
		.ok();

	ring_permitted(rule.actions(), ruleset, room_id, joined_count)
}
