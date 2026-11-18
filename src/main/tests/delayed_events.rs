#![cfg(test)]

//! Delayed events (MSC4140) end to end, on a booted server.
//!
//! The cases cover discovery, scheduling through every request form, firing
//! no earlier than the delay, management by path and by body, a tokenless
//! send, owner checks, finalised records and their replay, the delay ID shown
//! to the sender alone, limits, deactivation, and a scheduled event surviving
//! a restart.
//!
//! MindRoom: a client can delay only its own MatrixRTC call leave, a call
//! membership with empty content in the sender's slot. The cases about the
//! routes delay that leave instead of a message, a topic or a room membership,
//! and keep what they check; `every_form_schedules` expects the refusal of the
//! rest. The cases about what the service does with an event the routes now
//! refuse (`empty_key_query_fires`, `stored_send_stands` and the restart)
//! schedule upstream's event in the service, which is not held to that scope.

use std::{
	env::{current_exe, var},
	fs::remove_dir_all,
	path::{Path, PathBuf},
	process::Command,
	time::Duration,
};

use futures::{StreamExt, future::join3};
use reqwest::{Method, Response, StatusCode, header::RETRY_AFTER};
use serde_json::{Value, from_value, json};
use tokio::time::{sleep, timeout};
use tuwunel::Args;
use tuwunel_core::{
	Err, Result, err, implement,
	ruma::{
		OwnedRoomId, RoomAliasId, RoomId, UserId,
		events::{StateEventType, room::topic::RoomTopicEventContent},
	},
	utils::{BoolExt, stream::ReadyExt, time::now_millis},
};
use tuwunel_matrix::Event;
use tuwunel_service::{Services, users::DeactivationReason};

use self::{
	client::{Client, accepted, field, poll_until, register},
	fixture::{boot, boot_at},
	require::require,
};

mod client;
mod fixture;

#[path = "client/require.rs"]
mod require;

const ALICE: &str = "delayed-events-alice-token-000000";
const BOB: &str = "delayed-events-bob-token-00000000";
const QUIET: [&str; 2] = ["logging.enable=false", "database.pool.max_workers=64"];
const UNSTABLE: &str = "unstable/org.matrix.msc4140/";
const CALL_MEMBER: &str = "org.matrix.msc3401.call.member";
const DELAY_ID: &str = "org.matrix.msc4140.delay_id";
const FLAGS: [&str; 2] = ["org.matrix.msc4140", "org.matrix.msc4140.stable"];
const CAPABILITIES: [&str; 2] = ["m.delayed_events", "org.matrix.msc4140.delayed_events"];
const CHILD_DATABASE_ENV: &str = "DELAYED_EVENTS_TEST_DATABASE";
const CHILD_PHASE_ENV: &str = "DELAYED_EVENTS_TEST_PHASE";
const FIRE_DEADLINE: Duration = Duration::from_secs(10);

// Long enough that nothing fires while a case inspects it.
const LONG: u64 = 60_000;

/// Runs every case, each on a server of its own.
///
/// The cases boot in turn, and each restart phase boots in a child process,
/// since a database closed by this process stays locked to it.
#[test]
fn delayed_events() -> Result {
	if let Ok(phase) = var(CHILD_PHASE_ENV) {
		return restart_phase(&phase);
	}

	let limited = QUIET
		.into_iter()
		.chain(["rooms.delayed_events.max_delay_s=120", "rooms.delayed_events.max_scheduled=1"]);

	let switched_off = QUIET
		.into_iter()
		.chain(["rooms.delayed_events.max_scheduled=0"]);

	boot("delayed-events", QUIET, exercise)?;
	boot("delayed-events-limits", limited, limits)?;
	boot("delayed-events-disabled", switched_off, disabled)?;

	survive_a_restart()
}

fn restart_phase(phase: &str) -> Result {
	let database = var(CHILD_DATABASE_ENV).expect("delayed events child database is set");
	let database = PathBuf::from(database);

	match phase {
		| "schedule" => boot_at(&database, &["fresh"], QUIET, schedule_before_restart),
		| "fire" => {
			let started = now_millis();

			boot_at(&database, &[], QUIET, async |services: &Services, _: &str| {
				fired_after_restart(services, started).await
			})
		},
		| _ => Err!("unknown delayed events phase: {phase}"),
	}
}

async fn exercise(services: &Services, base: &str) -> Result {
	let alice_id = register(services, "alice", ALICE).await?;
	let bob_id = register(services, "bob", BOB).await?;

	let alice = Client { services, base, token: ALICE };
	let bob = Client { services, base, token: BOB };
	let room = alice.create_room(&json!({})).await?;

	discovery_advertised(&alice).await?;
	empty_key_query_fires(services, &alice, &room).await?;
	every_form_schedules(&alice, &room).await?;
	bare_delay_refused(&alice, &room).await?;
	restart_and_cancel(services, &alice, &room).await?;
	tokenless_send_on_unstable_only(services, &alice, &room, &alice_id).await?;
	owner_mismatch_is_not_found(&alice, &bob, &room).await?;
	transaction_retry_returns_one_delay(services, &alice, &room, &alice_id).await?;
	failed_sends_are_kept(services, &alice, &room, &alice_id).await?;
	stored_send_stands(&alice, &room).await?;
	refused_before_storage(&alice, &bob).await?;
	room_auth_checked_at_schedule(&bob, &room).await?;
	delay_id_reaches_only_its_sender(services, &alice, &bob).await?;
	deactivation_removes_scheduled(services, &bob, &bob_id).await
}

async fn discovery_advertised(alice: &Client<'_>) -> Result {
	let versions = alice
		.get_json(&alice.client_url("", "versions"))
		.await?;

	for flag in FLAGS {
		assert_eq!(versions["unstable_features"][flag], json!(true), "{flag}: {versions}");
	}

	let capabilities = alice.get_json(&alice.url("capabilities")).await?;

	for name in CAPABILITIES {
		let capability = &capabilities["capabilities"][name];

		assert_eq!(capability["max_delay_ms"], json!(86_400_000), "{name}: {capabilities}");
		assert_eq!(capability["max_scheduled"], json!(100), "{name}: {capabilities}");
	}

	Ok(())
}

/// MindRoom: the query is refused, and the service schedules the same topic.
async fn empty_key_query_fires(services: &Services, alice: &Client<'_>, room: &RoomId) -> Result {
	let delay = 300;
	let path = format!("rooms/{room}/state/m.room.topic?org.matrix.msc4140.delay={delay}");
	let scheduled_at = now_millis();
	let content = json!({ "topic": "later" });
	let response = alice
		.request(Method::PUT, &alice.url(&path), &content)
		.await?;

	refused(response, StatusCode::FORBIDDEN, "M_FORBIDDEN").await?;
	alice
		.schedule_in_service(room, "m.room.topic", Some(""), &content, delay)
		.await?;

	let fired = poll_until(FIRE_DEADLINE, async || {
		topic(services, room).await.as_deref() == Some("later")
	})
	.await;

	require(fired, "the delayed topic was never sent")?;
	require(
		topic_sent_at(services, room)
			.await?
			.ge(&scheduled_at.saturating_add(delay)),
		"the delayed topic was sent before its delay",
	)
}

async fn every_form_schedules(alice: &Client<'_>, room: &RoomId) -> Result {
	let delay = format!("org.matrix.msc4140.delay={LONG}");
	let slot = alice.slot("form").await?;
	let message = alice.url(&format!("rooms/{room}/send/m.room.message/query?{delay}"));
	let empty_key = alice.url(&format!("rooms/{room}/state/{CALL_MEMBER}/?{delay}"));
	let state = alice.url(&format!("rooms/{room}/state/{CALL_MEMBER}/{slot}?{delay}"));
	let stable_message = alice.url(&format!("rooms/{room}/delayed_event/m.room.message/stable"));
	let stable = alice.url(&format!("rooms/{room}/delayed_event/{CALL_MEMBER}/stable"));
	let dedicated = json!({ "delay_ms": LONG, "content": { "body": "x" } });
	let dedicated_state = json!({ "delay_ms": LONG, "state_key": slot, "content": {} });
	let join = json!({ "delay_ms": LONG, "state_key": slot, "content": { "body": "x" } });

	// MindRoom: a message, a call membership outside the sender's slot and one
	// with content are refused.
	let refusals = [
		(&message, json!({ "body": "x" })),
		(&stable_message, dedicated),
		(&empty_key, json!({})),
		(&state, json!({ "body": "x" })),
		(&stable, join),
	];

	for (url, body) in &refusals {
		let response = alice.request(Method::PUT, url, body).await?;

		refused(response, StatusCode::FORBIDDEN, "M_FORBIDDEN").await?;
	}

	let state = alice.put_json(&state, &json!({})).await?;
	let stable = alice.put_json(&stable, &dedicated_state).await?;
	let key = Some(slot.as_str());
	let forms = [(state, CALL_MEMBER, key), (stable, CALL_MEMBER, key)];

	for (reply, event_type, state_key) in forms {
		let delay_id = field(&reply, "delay_id")?;
		let event = alice.delayed(delay_id).await?;

		assert_eq!(event["type"], event_type, "{event}");
		assert_eq!(event.get("state_key").and_then(Value::as_str), state_key, "{event}");

		let event_url = alice.client_url(UNSTABLE, &format!("delayed_events/{delay_id}"));

		alice
			.post_json(&event_url, &json!({ "action": "cancel" }))
			.await?;

		require(cancelled(&alice.delayed(delay_id).await?), "a cancelled event was not kept")?;
	}

	Ok(())
}

/// Whether a finalised event reads as cancelled.
///
/// A cancelled outcome carries its timestamp and neither an event ID nor an
/// error.
fn cancelled(event: &Value) -> bool {
	let finalised = &event["finalised"];

	finalised["finalised_ts"].is_u64()
		&& finalised.get("event_id").is_none()
		&& finalised.get("error").is_none()
}

async fn bare_delay_refused(alice: &Client<'_>, room: &RoomId) -> Result {
	let path = format!("rooms/{room}/state/m.room.topic/?org.matrix.msc4140.delay");
	let response = alice
		.request(Method::PUT, &alice.url(&path), &json!({ "topic": "now" }))
		.await?;

	refused(response, StatusCode::BAD_REQUEST, "M_INVALID_PARAM").await
}

async fn restart_and_cancel(services: &Services, alice: &Client<'_>, room: &RoomId) -> Result {
	let delay_id = alice
		.schedule_dedicated(room, CALL_MEMBER, "restart", &json!({}))
		.await?;

	let event = alice.delayed(&delay_id).await?;

	assert_eq!(event["delay_ms"], json!(LONG), "{event}");
	assert_eq!(event["type"], CALL_MEMBER, "{event}");

	let scheduled_at = event["delayed_since_ts"].as_u64();

	sleep(Duration::from_millis(20)).await;
	alice.act(&delay_id, "restart").await?;

	let restarted = alice.delayed(&delay_id).await?;

	require(
		restarted["delayed_since_ts"].as_u64() > scheduled_at,
		"a restart left delayed_since_ts in place",
	)?;

	let listed = alice
		.get_json(&alice.client_url(UNSTABLE, "delayed_events"))
		.await?;

	assert_eq!(listed["delayed_events"][0]["delay_id"], delay_id.as_str(), "{listed}");
	assert_eq!(listed["delayed_events"][0]["delay"], json!(LONG), "{listed}");

	let cancelled_at = now_millis();

	alice.act(&delay_id, "cancel").await?;

	let event = alice.delayed(&delay_id).await?;

	require(cancelled(&event), "a cancelled event was not kept")?;
	require(
		event["finalised"]["finalised_ts"]
			.as_u64()
			.ge(&Some(cancelled_at)),
		"finalised_ts predates the cancel",
	)?;

	alice.act(&delay_id, "cancel").await?;
	conflicting(alice, &delay_id, ["send", "restart"]).await?;

	let rescheduled = scheduled_rows(services, &delay_id).await;

	require(rescheduled.is_false(), "a conflicting action rescheduled a finalised event")?;

	let listed = alice
		.get_json(&alice.client_url(UNSTABLE, "delayed_events"))
		.await?;

	require(
		listed.to_string().contains(&delay_id).is_false(),
		"a finalised event is listed as scheduled",
	)
}

async fn conflicting<const N: usize>(
	client: &Client<'_>,
	delay_id: &str,
	actions: [&str; N],
) -> Result {
	for action in actions {
		let response = client.attempt(delay_id, action).await?;

		// Fork: the unstable path, used here, answers such a conflict as missing.
		refused(response, StatusCode::NOT_FOUND, "M_NOT_FOUND").await?;
	}

	Ok(())
}

/// Whether an event still has a scheduled record or due-time entry.
///
/// Listing reads the owner index alone, so these two rows are read directly.
async fn scheduled_rows(services: &Services, delay_id: &str) -> bool {
	let record = services.db["delayid_event"]
		.exists(delay_id)
		.await
		.is_ok();

	let due = services.db["dueat_delayid"]
		.keys()
		.ready_any(|key: Result<(u64, &str)>| key.is_ok_and(|(_, id)| id == delay_id))
		.await;

	record || due
}

async fn tokenless_send_on_unstable_only(
	services: &Services,
	alice: &Client<'_>,
	room: &RoomId,
	alice_id: &UserId,
) -> Result {
	let state_key = format!("_{alice_id}_DEVICE_m.call");
	let path =
		format!("rooms/{room}/state/{CALL_MEMBER}/{state_key}?org.matrix.msc4140.delay={LONG}");

	let reply = alice
		.put_json(&alice.url(&path), &json!({}))
		.await?;

	let delay_id = field(&reply, "delay_id")?;

	let action = format!("delayed_events/{delay_id}/send");
	let stable = tokenless(services, &alice.client_url("v1/", &action)).await?;

	refused(stable, StatusCode::UNAUTHORIZED, "M_MISSING_TOKEN").await?;
	require(!call_member_sent(services, room, &state_key).await, "stable tokenless sent")?;

	let unstable = alice.client_url(UNSTABLE, &action);

	accepted(tokenless(services, &unstable).await?).await?;

	let sent = call_member_id(services, room, &state_key).await?;

	accepted(tokenless(services, &unstable).await?).await?;

	let cancel = alice.client_url(UNSTABLE, &format!("delayed_events/{delay_id}/cancel"));

	refused(tokenless(services, &cancel).await?, StatusCode::NOT_FOUND, "M_NOT_FOUND").await?;
	let resent = call_member_id(services, room, &state_key).await?;

	require(resent.eq(&sent), "a repeated send sent again")?;

	let event = alice.delayed(delay_id).await?;

	assert_eq!(event["finalised"]["event_id"], sent.as_str(), "{event}");

	Ok(())
}

async fn call_member_id(services: &Services, room: &RoomId, state_key: &str) -> Result<String> {
	let event = services
		.state_accessor
		.room_state_get(room, &CALL_MEMBER.into(), state_key)
		.await?;

	Ok(event.event_id.to_string())
}

async fn owner_mismatch_is_not_found(
	alice: &Client<'_>,
	bob: &Client<'_>,
	room: &RoomId,
) -> Result {
	let delay_id = alice
		.schedule_dedicated(room, CALL_MEMBER, "owner", &json!({}))
		.await?;

	let url = alice.client_url(UNSTABLE, &format!("delayed_events/{delay_id}"));

	refused(bob.fetch(&url).await?, StatusCode::NOT_FOUND, "M_NOT_FOUND").await?;

	let cancel = bob
		.post_url(&format!("{url}/cancel"), &json!({}))
		.await?;

	refused(cancel, StatusCode::NOT_FOUND, "M_NOT_FOUND").await?;
	alice.act(&delay_id, "cancel").await?;
	refused(bob.fetch(&url).await?, StatusCode::NOT_FOUND, "M_NOT_FOUND").await?;

	for action in ["cancel", "send"] {
		let response = bob.attempt(&delay_id, action).await?;

		refused(response, StatusCode::NOT_FOUND, "M_NOT_FOUND").await?;
	}

	Ok(())
}

async fn transaction_retry_returns_one_delay(
	services: &Services,
	alice: &Client<'_>,
	room: &RoomId,
	alice_id: &UserId,
) -> Result {
	let content = json!({});
	let first = alice
		.schedule_dedicated(room, CALL_MEMBER, "retry", &content)
		.await?;

	// Suspension fails the send policy, which a retry must not reach.
	services
		.users
		.set_suspended(alice_id, &services.globals.server_user);

	let retry = alice
		.schedule_dedicated(room, CALL_MEMBER, "retry", &content)
		.await;

	services.users.clear_suspended(alice_id);

	assert_eq!(retry?, first, "a retried transaction scheduled twice");

	alice.act(&first, "cancel").await
}

/// Keeps each failed send as a finalised record with its error (MSC4140).
///
/// One event is refused when it falls due and one on an explicit send, both
/// for a suspended sender; neither may be sent or restarted afterwards.
async fn failed_sends_are_kept(
	services: &Services,
	alice: &Client<'_>,
	room: &RoomId,
	alice_id: &UserId,
) -> Result {
	let slot = alice.slot("fails").await?;
	let path = format!("rooms/{room}/state/{CALL_MEMBER}/{slot}?org.matrix.msc4140.delay=300");
	let reply = alice
		.put_json(&alice.url(&path), &json!({}))
		.await?;

	let due = field(&reply, "delay_id")?;
	let explicit = alice
		.schedule_dedicated(room, CALL_MEMBER, "explicit", &json!({}))
		.await?;

	services
		.users
		.set_suspended(alice_id, &services.globals.server_user);

	let fired = poll_until(FIRE_DEADLINE, async || {
		alice
			.delayed(due)
			.await
			.is_ok_and(|event| event.get("finalised").is_some())
	})
	.await;

	let refusal = alice.attempt(&explicit, "send").await;

	services.users.clear_suspended(alice_id);
	require(fired, "the suspended sender's event was never finalised")?;
	refused(refusal?, StatusCode::FORBIDDEN, "M_USER_SUSPENDED").await?;

	for delay_id in [due, explicit.as_str()] {
		let event = alice.delayed(delay_id).await?;
		let finalised = &event["finalised"];

		assert_eq!(finalised["error"]["errcode"], "M_USER_SUSPENDED", "{event}");
		assert_eq!(finalised.get("event_id"), None, "{event}");

		conflicting(alice, delay_id, ["send", "restart"]).await?;
		alice.act(delay_id, "cancel").await?;
	}

	Ok(())
}

/// A send stored before a later step fails still stands as sent.
///
/// The reply names a thread root this server lacks, so thread indexing fails
/// once the event is already in the room.
///
/// MindRoom: the service schedules the reply, which the routes refuse.
async fn stored_send_stands(alice: &Client<'_>, room: &RoomId) -> Result {
	let content = json!({
		"body": "x",
		"m.relates_to": { "rel_type": "m.thread", "event_id": "$absent:localhost" },
	});

	let delay_id = alice
		.schedule_in_service(room, "m.room.message", None, &content, LONG)
		.await?;

	alice.act(&delay_id, "send").await?;

	let event = alice.delayed(&delay_id).await?;
	let finalised = &event["finalised"];
	let event_id = field(finalised, "event_id")?;

	assert_eq!(finalised.get("error"), None, "{event}");

	alice
		.get_json(&alice.url(&format!("rooms/{room}/event/{event_id}")))
		.await?;

	alice.act(&delay_id, "send").await?;
	conflicting(alice, &delay_id, ["cancel", "restart"]).await
}

/// A send refused before the event is stored stays a failed send.
///
/// MindRoom: the room's power levels let Bob schedule his call leave. Alice
/// then raises the level it takes, so room auth refuses it at send time,
/// before anything is stored.
async fn refused_before_storage(alice: &Client<'_>, bob: &Client<'_>) -> Result {
	let open = json!({ "events": { CALL_MEMBER: 0 } });
	let room = alice
		.create_room(&json!({ "preset": "public_chat", "power_level_content_override": open }))
		.await?;

	bob.post_json(&bob.url(&format!("rooms/{room}/join")), &json!({}))
		.await?;

	let delay_id = bob
		.schedule_dedicated(&room, CALL_MEMBER, "refused", &json!({}))
		.await?;

	let levels = alice.url(&format!("rooms/{room}/state/m.room.power_levels"));
	let mut closed = alice.get_json(&levels).await?;

	closed["events"][CALL_MEMBER] = json!(50);
	alice.put_json(&levels, &closed).await?;

	let refusal = bob.attempt(&delay_id, "send").await?;

	refused(refusal, StatusCode::FORBIDDEN, "M_FORBIDDEN").await?;

	let event = bob.delayed(&delay_id).await?;
	let finalised = &event["finalised"];

	assert_eq!(finalised["error"]["errcode"], "M_FORBIDDEN", "{event}");
	assert_eq!(finalised.get("event_id"), None, "{event}");

	Ok(())
}

/// Creates a public room of Alice's that Bob has joined.
///
/// Bob holds the default power level there, below what redaction or state
/// changes on others' behalf require.
async fn shared_room(alice: &Client<'_>, bob: &Client<'_>) -> Result<OwnedRoomId> {
	let room = alice
		.create_room(&json!({ "preset": "public_chat" }))
		.await?;

	bob.post_json(&bob.url(&format!("rooms/{room}/join")), &json!({}))
		.await?;

	Ok(room)
}

async fn room_auth_checked_at_schedule(bob: &Client<'_>, room: &RoomId) -> Result {
	let slot = bob.slot("intruder").await?;
	let path = format!("rooms/{room}/state/{CALL_MEMBER}/{slot}?org.matrix.msc4140.delay={LONG}");

	let response = bob
		.request(Method::PUT, &bob.url(&path), &json!({}))
		.await?;

	refused(response, StatusCode::FORBIDDEN, "M_FORBIDDEN").await
}

#[tracing::instrument(level = "debug", skip_all)]
async fn delay_id_reaches_only_its_sender(
	services: &Services,
	alice: &Client<'_>,
	bob: &Client<'_>,
) -> Result {
	let room = shared_room(alice, bob).await?;

	let slot = alice.slot("later").await?;
	let path = format!("rooms/{room}/state/{CALL_MEMBER}/{slot}?org.matrix.msc4140.delay=300");

	let reply = alice
		.put_json(&alice.url(&path), &json!({}))
		.await?;

	let delay_id = field(&reply, "delay_id")?;
	let fired =
		poll_until(FIRE_DEADLINE, async || call_member_sent(services, &room, &slot).await).await;

	require(fired, "the delayed call leave was never sent")?;

	let event_id = services
		.state_accessor
		.room_state_get(&room, &CALL_MEMBER.into(), &slot)
		.await?
		.event_id;

	// MindRoom: `/members` lists room memberships only, so it is not read.
	let named = format!(r#""{DELAY_ID}":"{delay_id}""#);
	let paths = [
		format!("rooms/{room}/event/{event_id}"),
		format!("rooms/{room}/state/{CALL_MEMBER}/{slot}?format=event"),
		format!("rooms/{room}/state"),
		format!("rooms/{room}/messages?dir=b"),
	];

	for path in &paths {
		let sender = alice
			.get_json(&alice.url(path))
			.await?
			.to_string();

		let member = bob.get_json(&bob.url(path)).await?.to_string();

		assert!(sender.contains(event_id.as_str()), "{path}: {sender}");
		assert!(member.contains(event_id.as_str()), "{path}: {member}");
		assert!(sender.contains(&named), "{path}: {sender}");
		assert!(!member.contains(DELAY_ID), "{path}: {member}");
	}

	Ok(())
}

async fn deactivation_removes_scheduled(
	services: &Services,
	bob: &Client<'_>,
	bob_id: &UserId,
) -> Result {
	let room = bob.create_room(&json!({})).await?;

	let room_lock = services.state.mutex.lock(&room).await;
	let url =
		bob.client_url(UNSTABLE, &format!("rooms/{room}/delayed_event/{CALL_MEMBER}/deactivate"));

	let slot = bob.slot("x").await?;
	let body = json!({ "delay_ms": LONG, "state_key": slot, "content": {} });
	let (reply, deactivated, released) = join3(
		bob.request(Method::PUT, &url, &body),
		async {
			let held =
				poll_until(FIRE_DEADLINE, async || admission_held(services, bob_id).await).await;

			require(held, "schedule did not hold admission before deactivation")?;
			services
				.users
				.deactivate_account(bob_id, DeactivationReason::Admin)
				.await
		},
		async {
			let held =
				poll_until(FIRE_DEADLINE, async || admission_held(services, bob_id).await).await;

			let removed = poll_until(FIRE_DEADLINE, async || {
				services
					.users
					.all_device_ids(bob_id)
					.count()
					.await
					.eq(&0)
			})
			.await;

			sleep(Duration::from_millis(250)).await;
			drop(room_lock);
			require(held && removed, "devices were not removed under admission")
		},
	)
	.await;

	let reply = accepted(reply?).await?;

	deactivated?;
	released?;

	let scheduled = services
		.delayed_events
		.scheduled(bob_id)
		.count()
		.await;

	assert_eq!(scheduled, 0, "deactivation left scheduled events behind");

	let delay_id = field(&reply, "delay_id")?;
	let kept = services
		.delayed_events
		.get(delay_id, None)
		.await
		.is_ok();

	require(kept.is_false(), "deactivation kept a finalised record")
}

async fn admission_held(services: &Services, user_id: &UserId) -> bool {
	timeout(Duration::from_millis(50), services.delayed_events.lock_user(user_id))
		.await
		.is_err()
}

async fn limits(services: &Services, base: &str) -> Result {
	let alice_id = register(services, "alice", ALICE).await?;

	let alice = Client { services, base, token: ALICE };
	let room = alice.create_room(&json!({})).await?;
	let too_long = 120_001;

	let slot = alice.slot("x").await?;
	let path =
		format!("rooms/{room}/state/{CALL_MEMBER}/{slot}?org.matrix.msc4140.delay={too_long}");

	let query = alice
		.request(Method::PUT, &alice.url(&path), &json!({}))
		.await?;

	refused(query, StatusCode::BAD_REQUEST, "ORG.MATRIX.MSC4140_DELAY_TOO_LARGE").await?;

	let stable = alice
		.request(
			Method::PUT,
			&alice.url(&format!("rooms/{room}/delayed_event/{CALL_MEMBER}/long")),
			&json!({ "delay_ms": too_long, "state_key": slot, "content": {} }),
		)
		.await?;

	refused(stable, StatusCode::BAD_REQUEST, "M_DELAY_TOO_LARGE").await?;

	let other_room = alice.create_room(&json!({})).await?;
	let room_lock = services.state.mutex.lock(&room).await;
	let other_lock = services.state.mutex.lock(&other_room).await;
	let first =
		alice.client_url(UNSTABLE, &format!("rooms/{room}/delayed_event/{CALL_MEMBER}/first"));

	let rival = alice
		.client_url(UNSTABLE, &format!("rooms/{other_room}/delayed_event/{CALL_MEMBER}/rival"));

	let body = json!({ "delay_ms": LONG, "state_key": slot, "content": {} });
	let (first, rival, released) = join3(
		alice.request(Method::PUT, &first, &body),
		alice.request(Method::PUT, &rival, &body),
		async {
			let held =
				poll_until(FIRE_DEADLINE, async || admission_held(services, &alice_id).await)
					.await;

			sleep(Duration::from_millis(250)).await;
			drop((room_lock, other_lock));
			require(held, "schedule did not hold admission before room release")
		},
	)
	.await;

	released?;

	let (first, rival) = (first?, rival?);

	assert_ne!(first.status(), rival.status(), "concurrent schedules bypassed the cap");

	let (winner, loser) = match first.status() {
		| StatusCode::OK => (first, rival),
		| _ => (rival, first),
	};

	assert_eq!(winner.status(), StatusCode::OK);
	refused(loser, StatusCode::TOO_MANY_REQUESTS, "M_LIMIT_EXCEEDED").await?;

	let scheduled = services
		.delayed_events
		.scheduled(&alice_id)
		.count()
		.await;

	assert_eq!(scheduled, 1);

	let second =
		alice.client_url(UNSTABLE, &format!("rooms/{room}/delayed_event/{CALL_MEMBER}/second"));

	let over = alice.request(Method::PUT, &second, &body).await?;

	let retry_after = over
		.headers()
		.get(RETRY_AFTER)
		.and_then(|value| value.to_str().ok())
		.and_then(|value| value.parse().ok());

	require(
		retry_after.is_some_and(|secs: u64| (1..=60).contains(&secs)),
		"the cap answered without a Retry-After until the next event",
	)?;

	refused(over, StatusCode::TOO_MANY_REQUESTS, "M_LIMIT_EXCEEDED").await
}

async fn disabled(services: &Services, base: &str) -> Result {
	register(services, "alice", ALICE).await?;

	let alice = Client { services, base, token: ALICE };
	let room = alice.create_room(&json!({})).await?;
	let versions = alice
		.get_json(&alice.client_url("", "versions"))
		.await?;

	let capabilities = alice.get_json(&alice.url("capabilities")).await?;

	for flag in FLAGS {
		assert_eq!(versions["unstable_features"].get(flag), None, "{flag}: {versions}");
	}

	for name in CAPABILITIES {
		assert_eq!(capabilities["capabilities"].get(name), None, "{name}: {capabilities}");
	}

	let slot = alice.slot("off").await?;
	let path = format!("rooms/{room}/state/{CALL_MEMBER}/{slot}?org.matrix.msc4140.delay=1000");
	let response = alice
		.request(Method::PUT, &alice.url(&path), &json!({}))
		.await?;

	refused(response, StatusCode::FORBIDDEN, "M_FORBIDDEN").await
}

fn survive_a_restart() -> Result {
	let database = Args::test_database_path("delayed-events-restart");
	let outcome = run_child(&database, "schedule").and_then(|()| run_child(&database, "fire"));

	remove_dir_all(database).ok();

	outcome
}

fn run_child(database: &Path, phase: &str) -> Result {
	let output = Command::new(current_exe()?)
		.env(CHILD_DATABASE_ENV, database)
		.env(CHILD_PHASE_ENV, phase)
		.output()?;

	if !output.status.success() {
		let stdout = String::from_utf8_lossy(&output.stdout);
		let stderr = String::from_utf8_lossy(&output.stderr);

		return Err!(
			"delayed events {phase} child failed with {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
			output.status,
		);
	}

	Ok(())
}

async fn schedule_before_restart(services: &Services, base: &str) -> Result {
	register(services, "alice", ALICE).await?;

	let alice = Client { services, base, token: ALICE };
	let room = alice
		.create_room(&json!({ "room_alias_name": "restart" }))
		.await?;

	// MindRoom: the service schedules the topic, which the routes refuse.
	let content = json!({ "topic": "after restart" });
	let delay_id = alice
		.schedule_in_service(&room, "m.room.topic", Some(""), &content, 3000)
		.await?;

	alice.delayed(&delay_id).await.map(|_| ())
}

async fn fired_after_restart(services: &Services, started: u64) -> Result {
	let alias = format!("#restart:{}", services.globals.server_name());
	let alias: &RoomAliasId = alias.as_str().try_into()?;
	let room = services.alias.resolve_local_alias(alias).await?;

	let fired = poll_until(FIRE_DEADLINE, async || {
		topic(services, &room)
			.await
			.as_deref()
			.eq(&Some("after restart"))
	})
	.await;

	require(fired, "the scheduled topic did not survive the restart")?;
	require(
		topic_sent_at(services, &room).await?.ge(&started),
		"the scheduled topic was sent before the restart",
	)
}

async fn topic(services: &Services, room: &RoomId) -> Option<String> {
	services
		.state_accessor
		.room_state_get_content(room, &StateEventType::RoomTopic, "")
		.await
		.ok()
		.map(|content: RoomTopicEventContent| content.topic)
}

async fn topic_sent_at(services: &Services, room: &RoomId) -> Result<u64> {
	let topic = services
		.state_accessor
		.room_state_get(room, &StateEventType::RoomTopic, "")
		.await?;

	let sent_at = topic.origin_server_ts().get().into();

	Ok(sent_at)
}

async fn call_member_sent(services: &Services, room: &RoomId, state_key: &str) -> bool {
	services
		.state_accessor
		.room_state_get(room, &CALL_MEMBER.into(), state_key)
		.await
		.is_ok()
}

async fn tokenless(services: &Services, url: &str) -> Result<Response> {
	let response = services
		.client
		.clients
		.default
		.post(url)
		.json(&json!({}))
		.send()
		.await?;

	Ok(response)
}

async fn refused(response: Response, status: StatusCode, errcode: &str) -> Result {
	let actual = response.status();
	let body: Value = response.json().await?;

	assert_eq!(actual, status, "{body}");
	assert_eq!(body["errcode"], errcode, "{body}");

	Ok(())
}

#[implement(Client, params = "<'_>")]
async fn get_json(&self, url: &str) -> Result<Value> { accepted(self.fetch(url).await?).await }

#[implement(Client, params = "<'_>")]
async fn fetch(&self, url: &str) -> Result<Response> {
	self.request(Method::GET, url, &json!({})).await
}

#[implement(Client, params = "<'_>")]
async fn put_json(&self, url: &str, body: &Value) -> Result<Value> {
	accepted(self.request(Method::PUT, url, body).await?).await
}

#[implement(Client, params = "<'_>")]
async fn schedule_dedicated(
	&self,
	room: &RoomId,
	event_type: &str,
	txn_id: &str,
	content: &Value,
) -> Result<String> {
	// MindRoom: the state key makes it a state event in the sender's call slot.
	let path = format!("rooms/{room}/delayed_event/{event_type}/{txn_id}");
	let slot = self.slot(txn_id).await?;
	let body = json!({ "delay_ms": LONG, "state_key": slot, "content": content });
	let reply = self
		.put_json(&self.client_url(UNSTABLE, &path), &body)
		.await?;

	field(&reply, "delay_id").map(ToOwned::to_owned)
}

/// MindRoom: the state key of a call slot of this user's, as matrix-js-sdk
/// forms it.
#[implement(Client, params = "<'_>")]
async fn slot(&self, name: &str) -> Result<String> {
	let whoami = self.get_json(&self.url("account/whoami")).await?;

	Ok(format!("_{}_{name}", field(&whoami, "user_id")?))
}

/// MindRoom: schedules an event in the service, which is not held to the scope
/// of the client routes, and returns its `delay_id`.
#[implement(Client, params = "<'_>")]
async fn schedule_in_service(
	&self,
	room: &RoomId,
	event_type: &str,
	state_key: Option<&str>,
	content: &Value,
	delay_ms: u64,
) -> Result<String> {
	let whoami = self.get_json(&self.url("account/whoami")).await?;
	let event = json!({
		"user_id": field(&whoami, "user_id")?,
		"room_id": room,
		"type": event_type,
		"state_key": state_key,
		"content": content,
		"delay_ms": delay_ms,
		"due_ms": now_millis().saturating_add(delay_ms),
	});

	let delay_id = self
		.services
		.delayed_events
		.schedule(&from_value(event)?, None);

	Ok(delay_id.to_string())
}

#[implement(Client, params = "<'_>")]
async fn delayed(&self, delay_id: &str) -> Result<Value> {
	self.get_json(&self.client_url(UNSTABLE, &format!("delayed_events/{delay_id}")))
		.await
}

#[implement(Client, params = "<'_>")]
async fn act(&self, delay_id: &str, action: &str) -> Result {
	accepted(self.attempt(delay_id, action).await?)
		.await
		.map(|_| ())
		.map_err(|e| err!("{action} {delay_id}: {e}"))
}

#[implement(Client, params = "<'_>")]
async fn attempt(&self, delay_id: &str, action: &str) -> Result<Response> {
	let url = self.client_url(UNSTABLE, &format!("delayed_events/{delay_id}/{action}"));

	self.post_url(&url, &json!({})).await
}
