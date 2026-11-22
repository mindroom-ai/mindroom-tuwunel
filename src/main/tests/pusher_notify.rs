#![cfg(test)]

use std::{
	fs::remove_dir_all, net::TcpListener as StdTcpListener, str::from_utf8, time::Duration,
};

use futures::StreamExt;
use serde_json::{Value, json};
use tokio::{
	io::{AsyncReadExt, AsyncWriteExt},
	net::{TcpListener, TcpStream},
	spawn,
	sync::mpsc::{UnboundedReceiver, UnboundedSender, error::TryRecvError, unbounded_channel},
	task::JoinHandle,
	time::{sleep, timeout},
};
use tuwunel::{Args, Runtime, Server, async_run, async_stop};
use tuwunel_core::{
	Err, Result, err,
	ruma::{
		DeviceId, EventId, OwnedEventId, OwnedRoomId, RoomId, UInt, UserId,
		api::{
			client::push::{
				EmailPusherData, Pusher, PusherIds, PusherInit, PusherKind,
				set_pusher::v3::{PusherAction, Request as SetPusherRequest},
			},
			error::ErrorKind,
		},
		device_id,
		events::{
			GlobalAccountDataEventType,
			push_rules::{PushRulesEvent, PushRulesEventContent},
			receipt::ReceiptThread,
		},
		presence::PresenceState,
		push::{
			Action, ConditionalPushRule, EventPropertyIsConditionData, HighlightTweakValue,
			HttpPusherData, NewConditionalPushRule, NewPushRule, PredefinedOverrideRuleId,
			PushCondition, PushFormat, RuleKind, Ruleset, ScalarJsonValue, Tweak,
		},
	},
	utils::stream::ReadyExt,
};
use tuwunel_database::Json;
use tuwunel_matrix::{Event, Pdu, PduCount, PduId, RawPduId};
use tuwunel_service::{
	Services,
	presence::Ping,
	pusher::{
		Evaluate,
		ring_rules::{
			CALL_MEMBER_EVENT_TYPE, RING_ENCRYPTED_RULE_ID, RING_HINT_KEY, RING_RULE_ID,
			RING_SOUND_TWEAK, RTC_NOTIFICATION_EVENT_TYPE,
		},
	},
	sending::{Destination, SendingEvent},
};

mod client;

#[expect(dead_code)] // Only put_event is used from the event add-on.
#[path = "client/event.rs"]
mod event;

#[path = "client/get.rs"]
mod get;

#[path = "client/membership.rs"]
mod membership;

#[path = "client/put.rs"]
mod put;

#[path = "client/state.rs"]
mod state;

type CapturedRequest = (String, Vec<u8>);
type CaptureRx = UnboundedReceiver<CapturedRequest>;
type CaptureTx = UnboundedSender<CapturedRequest>;
type StubPusher = (Pusher, PusherAction, CaptureRx, AbortOnDrop);

struct StubPusherConfig<'a> {
	path: &'a str,
	event_id_only: bool,
	disable_badge_count: bool,
	response_body: &'a str,
}

const APP_ID: &str = "app.tuwunel.test";
const EVENT_ID: &str = "$push:remote.example";
const MISSING_PUSHER_EVENT_ID: &str = "$push-missing:remote.example";
const PERMANENT_EVENT_ID: &str = "$push-permanent:remote.example";
const RETRY_EVENT_ID_1: &str = "$push-retry-one:remote.example";
const RETRY_EVENT_ID_2: &str = "$push-retry-two:remote.example";
const SENDER: &str = "@alice:remote.example";
const NOTIFY_PATH: &str = "/_matrix/push/v1/notify";
const PUSH_FIXTURE_SHORT_ROOM_ID: u64 = 0x543;
const RETRY_QUIET_WINDOW: Duration = Duration::from_secs(4);

impl<'a> StubPusherConfig<'a> {
	fn new(response_body: &'a str) -> Self {
		Self {
			path: NOTIFY_PATH,
			event_id_only: false,
			disable_badge_count: false,
			response_body,
		}
	}
}

/// Inputs shared by the delivery cases; the event is driven straight through
/// the pusher service rather than through a real room and timeline.
struct Fixture<'a> {
	services: &'a Services,
	user: &'a UserId,
	device: &'a DeviceId,
	ruleset: &'a Ruleset,
	pdu: &'a Pdu,
	room_id: &'a str,
}

/// Exercises the homeserver's Push Gateway API client role end to end against a
/// stub gateway: delayed-start badge recovery, URL validation, the full and
/// event-id-only notification formats, and the pushkey removal that honoring
/// the gateway `rejected` list requires.
///
/// One server boot runs the cases sequentially.
#[test]
fn pusher_notify() -> Result {
	let listener = StdTcpListener::bind(("127.0.0.1", 0))?;
	let port = listener.local_addr()?.port();
	let db_path = Args::test_database_path("test-pusher-notify");

	let args = Args::default_test(&["fresh", "cleanup"])
		.with_database_path(&db_path)
		.with_loopback_listener(port)
		.with_option("listening=true")
		.with_option("network.http.ip_range_denylist=[]")
		.with_option("sending.startup_netburst=true")
		.with_option("push.suppress_when_active=true")
		.with_option("sending.retry_backoff_limit=1");

	let runtime = Runtime::new(Some(&args))?;
	let server = Server::new(Some(&args), Some(&runtime))?;
	let result: Result = runtime.block_on(async {
		let services = Services::build(server.server.clone()).await?;
		let mut recovery = prepare_badge_recovery(&services).await?;
		let mut push_retry = prepare_push_retry(&services).await?;
		let services = services.start().await?;
		_ = server
			.services
			.lock()
			.await
			.insert(services.clone());

		let base = format!("http://127.0.0.1:{port}");
		drop(listener);

		let exercise = async {
			let outcome = async {
				verify_badge_recovery(&services, &mut recovery).await?;
				verify_push_retry(&services, &mut push_retry).await?;
				verify_badge_retry(&services).await?;
				verify_permanent_push_error(&services).await?;
				verify_missing_pusher_reap(&services).await?;
				run_cases(&services).await?;
				exercise_call_ring_cases(&services, &base).await
			}
			.await;
			let shutdown = server.server.shutdown();
			outcome.and(shutdown)
		};

		let (run_result, outcome) = tokio::join!(async_run(&server), exercise);
		drop(services);
		async_stop(&server).await?;
		run_result?;

		outcome
	});

	drop(runtime);
	remove_dir_all(&db_path).ok();

	result
}

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
	fn drop(&mut self) { self.0.abort(); }
}

struct BadgeRecovery {
	destination: Destination,
	rx: CaptureRx,
	_stub: AbortOnDrop,
}

struct PushRetry {
	destination: Destination,
	first_id: RawPduId,
	second_id: RawPduId,
	permits: UnboundedSender<()>,
	rx: CaptureRx,
	_stub: AbortOnDrop,
}

async fn prepare_badge_recovery(services: &Services) -> Result<BadgeRecovery> {
	let server_name = services.globals.server_name();
	let user = UserId::parse_with_server_name("badge-recovery", server_name)?;
	let pushkey = "pk-badge-recovery";

	let listener = TcpListener::bind("127.0.0.1:0").await?;
	let url = format!("http://{}{NOTIFY_PATH}", listener.local_addr()?);
	let action = pusher_action(pushkey, url, false, false);

	services
		.pusher
		.set_pusher(&user, device_id!("BADGERECOVERY"), &action)
		.await?;

	let (tx, rx) = unbounded_channel();
	let stub = AbortOnDrop(spawn(stub_gateway(listener, tx, r#"{"rejected":[]}"#.to_owned())));

	services.sending.refresh_push_badge(&user).await?;
	services.sending.refresh_push_badge(&user).await?;

	Ok(BadgeRecovery {
		destination: Destination::Push(user, pushkey.to_owned()),
		rx,
		_stub: stub,
	})
}

async fn prepare_push_retry(services: &Services) -> Result<PushRetry> {
	let server_name = services.globals.server_name();
	let user = UserId::parse_with_server_name("push-retry", server_name)?;
	let pushkey = "pk-push-retry";
	let room_id = OwnedRoomId::from_parts('!', "push-retry", Some(server_name.as_str()))?;

	let listener = TcpListener::bind("127.0.0.1:0").await?;
	let url = format!("http://{}{NOTIFY_PATH}", listener.local_addr()?);
	let action = pusher_action(pushkey, url, false, true);

	services
		.pusher
		.set_pusher(&user, device_id!("PUSHRETRY"), &action)
		.await?;

	let (tx, rx) = unbounded_channel();
	let (permits, permit_rx) = unbounded_channel();
	let stub = AbortOnDrop(spawn(stub_gateway_scripted(
		listener,
		tx,
		r#"{"rejected":[]}"#.to_owned(),
		1,
		permit_rx,
	)));

	let first_id = persist_message_pdu(services, &room_id, RETRY_EVENT_ID_1, 1)?;
	let second_id = persist_message_pdu(services, &room_id, RETRY_EVENT_ID_2, 2)?;

	services
		.sending
		.send_pdu_push(&first_id, &user, pushkey.to_owned())?;

	services
		.sending
		.send_pdu_push(&second_id, &user, pushkey.to_owned())?;

	services.sending.refresh_push_badge(&user).await?;

	Ok(PushRetry {
		destination: Destination::Push(user, pushkey.to_owned()),
		first_id,
		second_id,
		permits,
		rx,
		_stub: stub,
	})
}

async fn verify_badge_recovery(services: &Services, recovery: &mut BadgeRecovery) -> Result {
	let (path, body) = recv(&mut recovery.rx).await?;

	if path != NOTIFY_PATH {
		return Err!("recovered badge notification hit unexpected path {path}");
	}

	let body = serde_json::from_slice(&body)
		.map_err(|e| err!("recovered badge notification body was not json: {e}"))?;

	let recovered = notification(&body)?;

	assert_eq!(recovered.get("counts"), Some(&json!({"unread": 0})));

	for field in ["event_id", "room_id", "sender", "type", "content", "prio"] {
		expect_absent(recovered, field)?;
	}

	expect_absent(first_device(recovered)?, "tweaks")?;
	wait_for_queue_cleanup(services, &recovery.destination).await?;

	let Destination::Push(user_id, _) = &recovery.destination else {
		unreachable!("badge recovery destination is push");
	};

	// A changed count keeps the barrier an observed POST under the delivery
	// memo, and proves the memo-differs arm on the same wire.
	let server_name = services.globals.server_name();
	let room_id = OwnedRoomId::from_parts('!', "badge-recovery", Some(server_name.as_str()))?;
	let joined = services.db.get("userroomid_joined")?;
	let unread = services.db.get("userroomid_notificationcount")?;

	joined.put((user_id, &room_id), 1_u64);
	unread.put((user_id, &room_id), 1_u64);

	// This wake sits behind the two pre-start messages in the worker channel.
	// Its completed row is a deterministic barrier for stale-wake processing.
	services
		.sending
		.refresh_push_badge(user_id)
		.await?;

	wait_for_queue_cleanup(services, &recovery.destination).await?;

	let (path, body) = recv(&mut recovery.rx).await?;

	if path != NOTIFY_PATH {
		return Err!("barrier badge notification hit unexpected path {path}");
	}

	let body = serde_json::from_slice(&body)
		.map_err(|e| err!("barrier badge notification body was not json: {e}"))?;

	assert_eq!(notification(&body)?.get("counts"), Some(&json!({"unread": 1})));

	match recovery.rx.try_recv() {
		| Err(TryRecvError::Empty) => Ok(()),
		| Err(TryRecvError::Disconnected) =>
			Err!("stub gateway channel closed after badge recovery"),
		| Ok(_) => Err!("stale badge wake produced a duplicate notification"),
	}
}

async fn verify_push_retry(services: &Services, retry: &mut PushRetry) -> Result {
	let (first_path, first_body) = recv(&mut retry.rx).await?;

	if first_path != NOTIFY_PATH {
		return Err!("queued push notification hit unexpected path {first_path}");
	}

	let first = captured_event_id(&first_body)?;

	permit_response(&retry.permits)?;

	let (second_path, second_body) = recv(&mut retry.rx).await?;

	if second_path != NOTIFY_PATH {
		return Err!("queued push notification hit unexpected path {second_path}");
	}

	let second = captured_event_id(&second_body)?;

	if first == second {
		return Err!("initial push transaction delivered the same event twice");
	}

	let initial = [first.as_str(), second.as_str()];

	if !initial.contains(&RETRY_EVENT_ID_1) || !initial.contains(&RETRY_EVENT_ID_2) {
		return Err!("initial push transaction did not contain both fixture events");
	}

	permit_response(&retry.permits)?;

	let (retry_path, retry_body) = recv(&mut retry.rx).await?;

	if retry_path != NOTIFY_PATH {
		return Err!("queued push notification hit unexpected path {retry_path}");
	}

	let retried = captured_event_id(&retry_body)?;

	if retried != first {
		return Err!("push retry did not contain only the failed event");
	}

	let expected_id = retry_pdu_id(retry, &first)?;

	expect_retry_active_set(services, &retry.destination, expected_id).await?;
	permit_response(&retry.permits)?;
	wait_for_queue_cleanup(services, &retry.destination).await?;

	expect_quiescent(&mut retry.rx, "push retry")
}

fn permit_response(permits: &UnboundedSender<()>) -> Result {
	permits
		.send(())
		.map_err(|_| err!("scripted gateway stopped before response permit"))
}

fn retry_pdu_id<'a>(retry: &'a PushRetry, event_id: &EventId) -> Result<&'a RawPduId> {
	match event_id.as_str() {
		| RETRY_EVENT_ID_1 => Ok(&retry.first_id),
		| RETRY_EVENT_ID_2 => Ok(&retry.second_id),
		| _ => Err!("captured unexpected retry fixture event {event_id}"),
	}
}

async fn expect_retry_active_set(
	services: &Services,
	destination: &Destination,
	expected: &RawPduId,
) -> Result {
	let queued = services
		.sending
		.db
		.queued_requests(destination)
		.ready_any(|_| true)
		.await;

	if queued {
		return Err!("mixed push result left a queued request");
	}

	let (pdu_count, expected_active, badge_count, other_count) = services
		.sending
		.db
		.active_requests_for(destination)
		.ready_fold((0_usize, false, 0_usize, 0_usize), |state, (_, event)| {
			let (pdu_count, expected_active, badge_count, other_count) = state;

			match event {
				| SendingEvent::Pdu(pdu_id) => (
					pdu_count.saturating_add(1),
					expected_active || &pdu_id == expected,
					badge_count,
					other_count,
				),
				| SendingEvent::BadgeRefresh =>
					(pdu_count, expected_active, badge_count.saturating_add(1), other_count),
				| _ => (pdu_count, expected_active, badge_count, other_count.saturating_add(1)),
			}
		})
		.await;

	match (pdu_count, expected_active, badge_count, other_count) {
		| (1, true, 1, 0) => Ok(()),
		| (1, false, 1, 0) => Err!("mixed push result retained the wrong PDU"),
		| state => Err!("mixed push result retained unexpected active state {state:?}"),
	}
}

/// Covers timer-driven retry and queue cleanup after a gateway failure.
///
/// The one-shot fourth-failure escalation remains a manual log check because
/// this harness does not install a tracing capture layer.
async fn verify_badge_retry(services: &Services) -> Result {
	let server_name = services.globals.server_name();
	let user = UserId::parse_with_server_name("badge-retry", server_name)?;
	let pushkey = "pk-badge-retry";

	let listener = TcpListener::bind("127.0.0.1:0").await?;
	let url = format!("http://{}{NOTIFY_PATH}", listener.local_addr()?);
	let action = pusher_action(pushkey, url, false, false);

	services
		.pusher
		.set_pusher(&user, device_id!("BADGERETRY"), &action)
		.await?;

	let (tx, mut rx) = unbounded_channel();
	let (permits, permit_rx) = unbounded_channel();
	let _stub = AbortOnDrop(spawn(stub_gateway_scripted(
		listener,
		tx,
		r#"{"rejected":[]}"#.to_owned(),
		1,
		permit_rx,
	)));

	let destination = Destination::Push(user.clone(), pushkey.to_owned());

	services.sending.refresh_push_badge(&user).await?;

	let (first_path, first_body) = recv(&mut rx).await?;

	if first_path != NOTIFY_PATH {
		return Err!("refused badge notification hit unexpected path {first_path}");
	}

	permit_response(&permits)?;

	let (second_path, second_body) = recv(&mut rx).await?;

	if second_path != NOTIFY_PATH {
		return Err!("badge retry hit unexpected path {second_path}");
	}

	let active = services
		.sending
		.db
		.active_requests_for(&destination)
		.ready_any(|_| true)
		.await;

	if !active {
		return Err!("failed badge notification did not remain active");
	}

	let first_count = captured_unread_count(&first_body, "refused badge notification")?;
	let second_count = captured_unread_count(&second_body, "badge retry")?;

	if first_count != second_count {
		return Err!("badge retry changed the unread count");
	}

	permit_response(&permits)?;
	wait_for_queue_cleanup(services, &destination).await?;

	expect_quiescent_for(&mut rx, "badge retry", RETRY_QUIET_WINDOW).await
}

fn captured_unread_count(body: &[u8], case: &str) -> Result<u64> {
	let body: Value =
		serde_json::from_slice(body).map_err(|e| err!("{case} body was not json: {e}"))?;

	notification(&body)?
		.get("counts")
		.and_then(|counts| counts.get("unread"))
		.and_then(Value::as_u64)
		.ok_or_else(|| err!("{case} had no unread count"))
}

async fn verify_permanent_push_error(services: &Services) -> Result {
	let server_name = services.globals.server_name();
	let user = UserId::parse_with_server_name("push-permanent", server_name)?;
	let pushkey = "pk-push-permanent";
	let room_id = OwnedRoomId::from_parts('!', "push-permanent", Some(server_name.as_str()))?;
	let action =
		pusher_action(pushkey, "ftp://127.0.0.1/_matrix/push/v1/notify".to_owned(), false, false);

	// Bypass creation validation to model a legacy or corrupt stored pusher.
	services
		.db
		.get("senderkey_pusher")?
		.put((&user, pushkey), Json(&action));

	services.pusher.get_pusher(&user, pushkey).await?;

	let pdu_id = persist_message_pdu(services, &room_id, PERMANENT_EVENT_ID, 3)?;
	let destination = Destination::Push(user.clone(), pushkey.to_owned());

	services
		.sending
		.send_pdu_push(&pdu_id, &user, pushkey.to_owned())?;

	wait_for_queue_cleanup(services, &destination).await?;
	services.sending.refresh_push_badge(&user).await?;
	wait_for_queue_cleanup(services, &destination).await?;

	match services.pusher.get_pusher(&user, pushkey).await {
		| Ok(_) => Ok(()),
		| Err(error) if error.is_not_found() =>
			Err!("permanently invalid pusher was removed during delivery"),
		| Err(error) => Err(error),
	}
}

async fn verify_missing_pusher_reap(services: &Services) -> Result {
	let server_name = services.globals.server_name();
	let user = UserId::parse_with_server_name("push-missing", server_name)?;
	let pushkey = "pk-push-missing";
	let room_id = OwnedRoomId::from_parts('!', "push-missing", Some(server_name.as_str()))?;
	let pdu_id = persist_message_pdu(services, &room_id, MISSING_PUSHER_EVENT_ID, 4)?;
	let destination = Destination::Push(user.clone(), pushkey.to_owned());

	services
		.sending
		.send_pdu_push(&pdu_id, &user, pushkey.to_owned())?;

	wait_for_queue_cleanup(services, &destination).await
}

async fn wait_for_queue_cleanup(services: &Services, destination: &Destination) -> Result {
	let cleared = async {
		loop {
			let queued = services
				.sending
				.db
				.queued_requests(destination)
				.ready_any(|_| true)
				.await;

			let active = services
				.sending
				.db
				.active_requests_for(destination)
				.ready_any(|_| true)
				.await;

			if !queued && !active {
				break;
			}

			sleep(Duration::from_millis(10)).await;
		}
	};

	timeout(Duration::from_secs(10), cleared)
		.await
		.map_err(|_| err!("timed out waiting for sender queue cleanup"))
}

async fn run_cases(services: &Services) -> Result {
	let server_name = services.globals.server_name();
	let user = UserId::parse_with_server_name("pushtest", server_name)?;

	services
		.users
		.create(&user, Some("password"), None)
		.await?;

	let room_id = RoomId::parse(format!("!push:{server_name}"))?;
	let other_room_id = RoomId::parse(format!("!push-other:{server_name}"))?;
	let joined = services.db.get("userroomid_joined")?;
	let unread = services.db.get("userroomid_notificationcount")?;

	for room_id in [&room_id, &other_room_id] {
		joined.put((&user, room_id), 1_u64);
		unread.put((&user, room_id), 1_u64);
	}

	let pdu = message_event(room_id.as_str(), EVENT_ID)?;
	let ruleset = Ruleset::server_default(&user);

	let fixture = Fixture {
		services,
		user: &user,
		device: device_id!("PUSHDEV"),
		ruleset: &ruleset,
		pdu: &pdu,
		room_id: room_id.as_str(),
	};

	reject_bad_url(&fixture).await?;
	reject_email_without_smtp(&fixture).await?;
	full_format_delivery(&fixture).await?;
	event_id_only_delivery(&fixture).await?;
	gateway_url_paths(&fixture).await?;
	rejected_pushkey_removed(&fixture).await?;
	foreign_rejected_key_noop(&fixture).await?;
	legacy_actions(&fixture).await?;
	counts_only_delivery(&fixture, &room_id, &other_room_id).await?;
	account_wide_count_delivery(&fixture, &room_id).await?;
	badge_count_opt_out(&fixture).await?;
	badge_delivery_memo(&fixture, &room_id).await?;
	badge_bypasses_suppression(&fixture).await?;
	corrupt_count_rows_skipped(&fixture, &room_id).await
}

fn message_event(room_id: &str, event_id: &str) -> Result<Pdu> {
	serde_json::from_value(json!({
		"type": "m.room.message",
		"content": { "msgtype": "m.text", "body": "hello world" },
		"event_id": event_id,
		"room_id": room_id,
		"sender": SENDER,
		"prev_events": ["$prev:remote.example"],
		"auth_events": ["$auth:remote.example"],
		"origin_server_ts": 1_838_188_000,
		"depth": 12,
		"hashes": { "sha256": "thishashcoversallfieldsincasethisisredacted" },
	}))
	.map_err(|e| err!("invalid test pdu: {e}"))
}

fn persist_message_pdu(
	services: &Services,
	room_id: &RoomId,
	event_id: &str,
	count: u64,
) -> Result<RawPduId> {
	let pdu = message_event(room_id.as_str(), event_id)?;
	let pdu_id: RawPduId = PduId {
		shortroomid: PUSH_FIXTURE_SHORT_ROOM_ID,
		count: PduCount::Normal(count),
	}
	.into();

	services
		.db
		.get("pduid_pdu")?
		.raw_put(pdu_id, Json(&pdu));

	Ok(pdu_id)
}

/// A pusher URL that is neither http nor https is rejected at creation.
async fn reject_bad_url(fixture: &Fixture<'_>) -> Result {
	let action = pusher_action("pk-badscheme", "ftp://127.0.0.1/notify".to_owned(), false, false);

	let outcome = fixture
		.services
		.pusher
		.set_pusher(fixture.user, fixture.device, &action)
		.await;

	outcome
		.is_err()
		.then_some(())
		.ok_or_else(|| err!("set_pusher accepted a non-HTTP(S) pusher URL"))
}

/// An email pusher is refused while the server has no SMTP transport.
///
/// The refusal comes before anything is written, so the address finds no
/// stored pusher.
async fn reject_email_without_smtp(fixture: &Fixture<'_>) -> Result {
	let address = "pushtest@example.com";
	let action = email_pusher_action(address);

	let refusal = fixture
		.services
		.pusher
		.set_pusher(fixture.user, fixture.device, &action)
		.await
		.err()
		.ok_or_else(|| err!("set_pusher accepted an email pusher without SMTP"))?;

	assert!(
		matches!(refusal.kind(), ErrorKind::InvalidParam),
		"email pusher refusal without SMTP: {refusal}"
	);

	let stored = fixture
		.services
		.pusher
		.get_pusher(fixture.user, address)
		.await;

	assert!(stored.is_err(), "a refused email pusher was stored");

	Ok(())
}

fn email_pusher_action(address: &str) -> PusherAction {
	let ids = PusherIds::new(address.to_owned(), "m.email".to_owned());

	post_action(ids, PusherKind::Email(EmailPusherData::new()))
}

/// A full-format notification carries the event, sender, content, and device
/// identity, and leaves the pusher in place.
async fn full_format_delivery(fixture: &Fixture<'_>) -> Result {
	let pushkey = "pk-full";
	let (path, body) = deliver(fixture, pushkey, false, false, r#"{"rejected":[]}"#).await?;

	if path != NOTIFY_PATH {
		return Err!("full-format notification hit unexpected path {path}");
	}

	let notification = notification(&body)?;

	expect_str(notification, "event_id", EVENT_ID)?;
	expect_str(notification, "room_id", fixture.room_id)?;
	expect_str(notification, "prio", "low")?;
	expect_str(notification, "sender", SENDER)?;

	assert_eq!(notification.get("counts"), Some(&json!({"unread": 2})));

	let content = notification
		.get("content")
		.ok_or_else(|| err!("full-format notification had no content"))?;

	expect_str(content, "body", "hello world")?;

	let device = first_device(notification)?;

	expect_str(device, "app_id", APP_ID)?;
	expect_str(device, "pushkey", pushkey)?;

	fixture
		.services
		.pusher
		.get_pusher(fixture.user, pushkey)
		.await
		.map(|_| ())
		.map_err(|_| err!("full-format pusher was unexpectedly removed"))
}

/// The event-id-only format ships only the identifiers, stripping content,
/// sender, and device tweaks.
async fn event_id_only_delivery(fixture: &Fixture<'_>) -> Result {
	let pushkey = "pk-eventidonly";
	let (_path, body) = deliver(fixture, pushkey, true, false, r#"{"rejected":[]}"#).await?;

	let notification = notification(&body)?;

	expect_str(notification, "event_id", EVENT_ID)?;
	expect_str(notification, "room_id", fixture.room_id)?;
	expect_absent(notification, "content")?;
	expect_absent(notification, "sender")?;

	expect_absent(first_device(notification)?, "tweaks")
}

async fn gateway_url_paths(fixture: &Fixture<'_>) -> Result {
	let cases = [
		(
			"pk-url-mid-path",
			"/_matrix/push/v1/notify/gw",
			"/_matrix/push/v1/notify/gw/_matrix/push/v1/notify",
		),
		("pk-url-prefix", "/gw/_matrix/push/v1/notify", "/gw/_matrix/push/v1/notify"),
		("pk-url-trailing-slash", "/_matrix/push/v1/notify/", NOTIFY_PATH),
	];

	for (pushkey, registered_path, expected_path) in cases {
		let config = StubPusherConfig {
			path: registered_path,
			..StubPusherConfig::new(r#"{"rejected":[]}"#)
		};
		let (pusher, _action, mut rx, _stub) = stub_pusher(fixture, pushkey, config).await?;

		fixture
			.services
			.pusher
			.send_push_notice(fixture.user, &pusher, fixture.ruleset, fixture.pdu)
			.await?;

		let (request_path, _body) = recv(&mut rx).await?;

		if request_path != expected_path {
			return Err!(
				"path {registered_path} produced {request_path}, expected {expected_path}"
			);
		}
	}

	Ok(())
}

/// A pushkey the gateway names in `rejected` is removed along with its pusher.
async fn rejected_pushkey_removed(fixture: &Fixture<'_>) -> Result {
	let pushkey = "pk-rejected";
	let response = format!(r#"{{"rejected":["{pushkey}"]}}"#);
	let (path, _body) = deliver(fixture, pushkey, false, false, &response).await?;

	if path != NOTIFY_PATH {
		return Err!("rejected-case notification hit unexpected path {path}");
	}

	if fixture
		.services
		.pusher
		.get_pusher(fixture.user, pushkey)
		.await
		.is_ok()
	{
		return Err!("pusher survived the gateway rejecting its pushkey");
	}

	if fixture
		.services
		.pusher
		.get_pushkeys(fixture.user)
		.ready_any(|key| key == pushkey)
		.await
	{
		return Err!("get_pushkeys still yields the rejected pushkey");
	}

	Ok(())
}

/// A rejected key that is not ours leaves our pusher intact.
async fn foreign_rejected_key_noop(fixture: &Fixture<'_>) -> Result {
	let pushkey = "pk-foreign";
	deliver(fixture, pushkey, false, false, r#"{"rejected":["unrelated-key"]}"#).await?;

	fixture
		.services
		.pusher
		.get_pusher(fixture.user, pushkey)
		.await
		.map(|_| ())
		.map_err(|_| err!("pusher was removed for a foreign rejected key"))
}

async fn legacy_actions(fixture: &Fixture<'_>) -> Result {
	let pushkey = "pk-legacy-actions";
	let config = StubPusherConfig::new(r#"{"rejected":[]}"#);
	let (pusher, _action, mut rx, _stub) = stub_pusher(fixture, pushkey, config).await?;

	let notify_ruleset = legacy_ruleset(&json!(["notify", "dont_notify"]))?;

	fixture
		.services
		.pusher
		.send_push_notice(fixture.user, &pusher, &notify_ruleset, fixture.pdu)
		.await?;

	let (_path, body) = recv(&mut rx).await?;
	let body = serde_json::from_slice(&body)
		.map_err(|e| err!("legacy-action notification body was not json: {e}"))?;

	expect_str(notification(&body)?, "event_id", EVENT_ID)?;

	let quiet_ruleset = legacy_ruleset(&json!(["dont_notify", "coalesce"]))?;

	fixture
		.services
		.pusher
		.send_push_notice(fixture.user, &pusher, &quiet_ruleset, fixture.pdu)
		.await?;

	expect_quiescent(&mut rx, "quiet legacy actions")
}

fn legacy_ruleset(actions: &Value) -> Result<Ruleset> {
	serde_json::from_value(json!({
		"override": [{
			"rule_id": "tuwunel.test.legacy",
			"default": false,
			"enabled": true,
			"conditions": [],
			"actions": actions,
		}],
	}))
	.map_err(|e| err!("invalid test ruleset: {e}"))
}

async fn counts_only_delivery(
	fixture: &Fixture<'_>,
	room_id: &RoomId,
	other_room_id: &RoomId,
) -> Result {
	let pusher = &fixture.services.pusher;

	pusher
		.reset_notification_counts_for_thread(fixture.user, room_id, None, &ReceiptThread::Main)
		.await;

	pusher
		.reset_notification_counts_for_thread(
			fixture.user,
			other_room_id,
			None,
			&ReceiptThread::Main,
		)
		.await;

	let remaining = pusher
		.global_notification_count(fixture.user)
		.await;

	if remaining != 0 {
		return Err!("reset left an account-wide unread total of {remaining}");
	}

	let (path, body) =
		deliver(fixture, "pk-badge-zero", false, true, r#"{"rejected":[]}"#).await?;

	if path != NOTIFY_PATH {
		return Err!("counts-only notification hit unexpected path {path}");
	}

	let notification = notification(&body)?;

	assert_eq!(notification.get("counts"), Some(&json!({"unread": 0})));

	for field in ["event_id", "room_id", "sender", "type", "content", "prio"] {
		expect_absent(notification, field)?;
	}

	expect_absent(first_device(notification)?, "tweaks")
}

async fn account_wide_count_delivery(fixture: &Fixture<'_>, room_id: &RoomId) -> Result {
	let unread = fixture
		.services
		.db
		.get("userroomid_notificationcount")?;

	let root = EventId::parse(EVENT_ID)?;
	let stale_room_id =
		RoomId::parse(format!("!push-stale:{}", fixture.services.globals.server_name()))?;

	unread.put((fixture.user, room_id, &root), 7_u64);
	unread.put((fixture.user, &stale_room_id), 41_u64);

	let (_, body) =
		deliver(fixture, "pk-badge-thread", false, true, r#"{"rejected":[]}"#).await?;

	let thread_notification = notification(&body)?;

	assert_eq!(thread_notification.get("counts"), Some(&json!({"unread": 7})));

	unread.put((fixture.user, room_id), u64::MAX);

	let (_, body) = deliver(fixture, "pk-badge-max", false, true, r#"{"rejected":[]}"#).await?;
	let max_notification = notification(&body)?;

	assert_eq!(max_notification.get("counts"), Some(&json!({"unread": UInt::MAX})));

	Ok(())
}

async fn badge_count_opt_out(fixture: &Fixture<'_>) -> Result {
	let pushkey = "pk-badge-disabled";
	let config = StubPusherConfig {
		disable_badge_count: true,
		..StubPusherConfig::new(r#"{"rejected":[]}"#)
	};
	let (pusher, _action, mut rx, _stub) = stub_pusher(fixture, pushkey, config).await?;

	fixture
		.services
		.pusher
		.send_push_notice(fixture.user, &pusher, fixture.ruleset, fixture.pdu)
		.await?;

	let (path, body) = recv(&mut rx).await?;
	if path != NOTIFY_PATH {
		return Err!("badge opt-out notification hit unexpected path {path}");
	}

	let body = serde_json::from_slice(&body)
		.map_err(|e| err!("badge opt-out notification body was not json: {e}"))?;

	expect_absent(notification(&body)?, "counts")?;

	fixture
		.services
		.pusher
		.send_badge_notice(fixture.user, &pusher)
		.await?;

	match rx.try_recv() {
		| Err(TryRecvError::Empty) => Ok(()),
		| Err(TryRecvError::Disconnected) =>
			Err!("stub gateway channel closed after badge opt-out"),
		| Ok(_) => Err!("badge opt-out emitted a counts-only notification"),
	}
}

/// Covers the badge delivery memo.
///
/// An unchanged total is not re-sent, a changed total is, an event
/// notification stamps the memo, and pusher replacement forgets it.
async fn badge_delivery_memo(fixture: &Fixture<'_>, room_id: &RoomId) -> Result {
	let unread = fixture
		.services
		.db
		.get("userroomid_notificationcount")?;

	let root = EventId::parse(EVENT_ID)?;

	unread.put((fixture.user, room_id), 0_u64);
	unread.put((fixture.user, room_id, &root), 0_u64);

	let pushkey = "pk-badge-memo";
	let config = StubPusherConfig::new(r#"{"rejected":[]}"#);
	let (pusher, action, mut rx, _stub) = stub_pusher(fixture, pushkey, config).await?;

	let refresh = || {
		fixture
			.services
			.pusher
			.send_badge_notice(fixture.user, &pusher)
	};

	refresh().await?;

	let (_, body) = recv(&mut rx).await?;
	let body = serde_json::from_slice(&body)
		.map_err(|e| err!("first memo delivery body was not json: {e}"))?;

	assert_eq!(notification(&body)?.get("counts"), Some(&json!({"unread": 0})));

	refresh().await?;
	match rx.try_recv() {
		| Err(TryRecvError::Empty) => (),
		| Err(TryRecvError::Disconnected) =>
			return Err!("stub gateway channel closed during memo dedupe"),
		| Ok(_) => return Err!("unchanged badge total was re-sent to the gateway"),
	}

	unread.put((fixture.user, room_id), 3_u64);
	refresh().await?;

	let (_, body) = recv(&mut rx).await?;
	let body = serde_json::from_slice(&body)
		.map_err(|e| err!("changed memo delivery body was not json: {e}"))?;

	assert_eq!(notification(&body)?.get("counts"), Some(&json!({"unread": 3})));

	// The event notification carries the new total and stamps the memo, so
	// the refresh behind it has nothing to add.
	unread.put((fixture.user, room_id), 5_u64);
	fixture
		.services
		.pusher
		.send_push_notice(fixture.user, &pusher, fixture.ruleset, fixture.pdu)
		.await?;

	recv(&mut rx).await?;

	refresh().await?;
	match rx.try_recv() {
		| Err(TryRecvError::Empty) => (),
		| Err(TryRecvError::Disconnected) =>
			return Err!("stub gateway channel closed after the event notice"),
		| Ok(_) => return Err!("event-stamped badge total was re-sent to the gateway"),
	}

	// Replacement forgets the record: the unchanged total is sent again.
	fixture
		.services
		.pusher
		.set_pusher(fixture.user, fixture.device, &action)
		.await?;

	refresh().await?;

	let (path, _) = recv(&mut rx).await?;

	if path != NOTIFY_PATH {
		return Err!("post-replacement badge notification hit unexpected path {path}");
	}

	Ok(())
}

/// Counts-only refreshes deliver while pushes are suppressed.
///
/// An active user's reads must still reconcile the gateway; only event
/// notifications are deferred by suppression.
async fn badge_bypasses_suppression(fixture: &Fixture<'_>) -> Result {
	let pushkey = "pk-badge-suppressed";
	let config = StubPusherConfig::new(r#"{"rejected":[]}"#);
	let (_pusher, _action, mut rx, _stub) = stub_pusher(fixture, pushkey, config).await?;

	// Drive the active heuristic: online presence plus a fresh sync stamp.
	fixture
		.services
		.presence
		.maybe_ping_presence(fixture.user, Ping::default())
		.await?;

	fixture
		.services
		.presence
		.note_sync(fixture.user, None)
		.await;

	// Assert the heuristic's inputs registered, or a driving failure would
	// let the POST arrive unsuppressed and pass this case vacuously.
	let presence = fixture
		.services
		.presence
		.get_presence(fixture.user)
		.await?;

	if presence.content.presence != PresenceState::Online {
		return Err!("fixture user's presence did not register as online");
	}

	if presence
		.content
		.last_active_ago
		.is_none_or(|age| u64::from(age) >= 65_000)
	{
		return Err!("fixture user's presence is not inside the active window");
	}

	if fixture
		.services
		.presence
		.last_sync_gap_ms(fixture.user)
		.await
		.is_none_or(|gap| gap >= 32_000)
	{
		return Err!("fixture user's sync activity is not inside the active window");
	}

	fixture
		.services
		.sending
		.refresh_push_badge(fixture.user)
		.await?;

	let (path, _) = recv(&mut rx).await?;

	if path != NOTIFY_PATH {
		return Err!("suppressed-window badge notification hit unexpected path {path}");
	}

	Ok(())
}

/// Rows that do not decode leave the account-wide total at the readable sum.
///
/// Runs last, since a corrupt thread row panics any later thread count read in
/// a debug build. Each row carries a count that would move the total if read.
async fn corrupt_count_rows_skipped(fixture: &Fixture<'_>, room_id: &RoomId) -> Result {
	let unread = fixture
		.services
		.db
		.get("userroomid_notificationcount")?;

	let total = || {
		fixture
			.services
			.pusher
			.global_notification_count(fixture.user)
	};

	unread.put((fixture.user, room_id), 3_u64);
	assert_eq!(total().await, 3, "a readable row was not counted");

	unread.put_raw((fixture.user, room_id, "$short"), [1_u8; 7]);
	assert_eq!(total().await, 3, "a short count moved the total");

	unread.put_raw((fixture.user, room_id, "$long"), [1_u8; 9]);
	assert_eq!(total().await, 3, "a long count moved the total");

	unread.put((fixture.user, "\u{1}no-sigil"), 11_u64);
	assert_eq!(total().await, 3, "an unreadable key moved the total");

	Ok(())
}

/// Registers a pusher for `pushkey` at a fresh stub gateway, drives one
/// notification, and returns the request path and parsed body the gateway
/// received. The gateway answers `response_body`.
async fn deliver(
	fixture: &Fixture<'_>,
	pushkey: &str,
	event_id_only: bool,
	badge_only: bool,
	response_body: &str,
) -> Result<(String, Value)> {
	let config = StubPusherConfig {
		event_id_only,
		..StubPusherConfig::new(response_body)
	};
	let (pusher, _action, mut rx, _stub) = stub_pusher(fixture, pushkey, config).await?;

	let pusher_service = &fixture.services.pusher;

	match badge_only {
		| true =>
			pusher_service
				.send_badge_notice(fixture.user, &pusher)
				.await?,
		| false =>
			pusher_service
				.send_push_notice(fixture.user, &pusher, fixture.ruleset, fixture.pdu)
				.await?,
	}

	let (path, body) = recv(&mut rx).await?;

	let body = serde_json::from_slice(&body)
		.map_err(|e| err!("push notification body was not json: {e}"))?;

	Ok((path, body))
}

/// Registers a pusher for `pushkey` at a fresh stub gateway.
///
/// Returns the stored pusher, the action for re-registration, and the
/// gateway's capture channel; the stub aborts when its handle drops.
async fn stub_pusher(
	fixture: &Fixture<'_>,
	pushkey: &str,
	config: StubPusherConfig<'_>,
) -> Result<StubPusher> {
	let listener = TcpListener::bind("127.0.0.1:0").await?;
	let url = format!("http://{}{}", listener.local_addr()?, config.path);
	let action = pusher_action(pushkey, url, config.event_id_only, config.disable_badge_count);

	fixture
		.services
		.pusher
		.set_pusher(fixture.user, fixture.device, &action)
		.await?;

	let pusher = fixture
		.services
		.pusher
		.get_pusher(fixture.user, pushkey)
		.await?;

	let (tx, rx) = unbounded_channel();
	let stub = AbortOnDrop(spawn(stub_gateway(listener, tx, config.response_body.to_owned())));

	Ok((pusher, action, rx, stub))
}

fn pusher_action(
	pushkey: &str,
	url: String,
	event_id_only: bool,
	disable_badge_count: bool,
) -> PusherAction {
	let mut data = HttpPusherData::new(url);
	data.format = event_id_only.then_some(PushFormat::EventIdOnly);
	data.data
		.insert("disable_badge_count".into(), Value::Bool(disable_badge_count));

	let ids = PusherIds::new(pushkey.to_owned(), APP_ID.to_owned());

	post_action(ids, PusherKind::Http(data))
}

fn post_action(ids: PusherIds, kind: PusherKind) -> PusherAction {
	let pusher: Pusher = PusherInit {
		ids,
		kind,
		app_display_name: "Tuwunel Test".into(),
		device_display_name: "Test Device".into(),
		profile_tag: None,
		lang: "en".into(),
	}
	.into();

	SetPusherRequest::post(pusher).action
}

async fn recv(rx: &mut CaptureRx) -> Result<CapturedRequest> {
	timeout(Duration::from_secs(10), rx.recv())
		.await
		.map_err(|_| err!("timed out waiting for a push notification"))?
		.ok_or_else(|| err!("stub gateway channel closed"))
}

fn captured_event_id(body: &[u8]) -> Result<OwnedEventId> {
	let body: Value = serde_json::from_slice(body)
		.map_err(|e| err!("queued push notification body was not json: {e}"))?;

	let event_id = notification(&body)?
		.get("event_id")
		.and_then(Value::as_str)
		.ok_or_else(|| err!("queued push notification had no event_id"))?;

	EventId::parse(event_id)
		.map_err(|e| err!("queued push notification had invalid event_id: {e}"))
}

fn expect_quiescent(rx: &mut CaptureRx, case: &str) -> Result {
	match rx.try_recv() {
		| Err(TryRecvError::Empty) => Ok(()),
		| Err(TryRecvError::Disconnected) => Err!("stub gateway channel closed after {case}"),
		| Ok(_) => Err!("{case} produced a spurious notification"),
	}
}

async fn expect_quiescent_for(rx: &mut CaptureRx, case: &str, window: Duration) -> Result {
	match timeout(window, rx.recv()).await {
		| Err(_) => Ok(()),
		| Ok(None) => Err!("stub gateway channel closed after {case}"),
		| Ok(Some(_)) => Err!("{case} produced a delayed spurious notification"),
	}
}

async fn stub_gateway(listener: TcpListener, tx: CaptureTx, response_body: String) {
	let response = http_response("200 OK", &response_body);

	while let Ok((mut socket, _)) = listener.accept().await {
		let Some((path, body)) = read_request(&mut socket).await else {
			continue;
		};

		if tx.send((path, body)).is_err() {
			return;
		}

		socket.write_all(response.as_bytes()).await.ok();
		socket.flush().await.ok();
	}
}

async fn stub_gateway_scripted(
	listener: TcpListener,
	tx: CaptureTx,
	response_body: String,
	mut failures: usize,
	mut permits: UnboundedReceiver<()>,
) {
	let accepted = http_response("200 OK", &response_body);
	let refused = http_response("502 Bad Gateway", "{}");

	while let Ok((mut socket, _)) = listener.accept().await {
		let Some((path, body)) = read_request(&mut socket).await else {
			continue;
		};

		if tx.send((path, body)).is_err() {
			break;
		}

		if permits.recv().await.is_none() {
			break;
		}

		let response = match failures {
			| 0 => &accepted,
			| _ => &refused,
		};

		failures = failures.saturating_sub(1);

		socket.write_all(response.as_bytes()).await.ok();
		socket.flush().await.ok();
	}
}

fn http_response(status: &str, body: &str) -> String {
	format!(
		"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: \
		 {}\r\nConnection: close\r\n\r\n{body}",
		body.len(),
	)
}

async fn read_request(socket: &mut TcpStream) -> Option<(String, Vec<u8>)> {
	let mut buf = Vec::new();
	let mut chunk = [0_u8; 4096];
	loop {
		if let Some(head_end) = find(&buf, b"\r\n\r\n") {
			let content_length = content_length(&buf[..head_end])?;
			let body_start = head_end.checked_add(4)?;
			let body_end = body_start.checked_add(content_length)?;
			if buf.len() >= body_end {
				let path = request_path(&buf[..head_end])?;
				let body = buf[body_start..body_end].to_vec();

				return Some((path, body));
			}
		}

		let read = socket.read(&mut chunk).await.ok()?;
		if read == 0 {
			return None;
		}

		buf.extend_from_slice(&chunk[..read]);
	}
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
	haystack
		.windows(needle.len())
		.position(|window| window == needle)
}

fn content_length(head: &[u8]) -> Option<usize> {
	from_utf8(head)
		.ok()?
		.lines()
		.find_map(|line| {
			line.split_once(':')
				.filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
		})
		.and_then(|(_, value)| value.trim().parse().ok())
}

fn request_path(head: &[u8]) -> Option<String> {
	from_utf8(head)
		.ok()?
		.lines()
		.next()?
		.split(' ')
		.nth(1)?
		.split('?')
		.next()
		.map(ToOwned::to_owned)
}

fn notification(body: &Value) -> Result<&Value> {
	body.get("notification")
		.ok_or_else(|| err!("push body had no notification object: {body}"))
}

fn first_device(notification: &Value) -> Result<&Value> {
	notification
		.get("devices")
		.and_then(Value::as_array)
		.and_then(|devices| devices.first())
		.ok_or_else(|| err!("notification had no devices entry: {notification}"))
}

fn expect_str(value: &Value, name: &str, want: &str) -> Result {
	let got = value.get(name).and_then(Value::as_str);
	(got == Some(want))
		.then_some(())
		.ok_or_else(|| err!("field {name}: expected {want:?}, got {got:?}"))
}

fn expect_absent(value: &Value, name: &str) -> Result {
	value
		.get(name)
		.map_or(Ok(()), |found| Err!("unexpected field {name}: {found}"))
}

const ALICE_RING_TOKEN: &str = "call-ring-push-rules-test-alice-access-token";
const BOB_RING_TOKEN: &str = "call-ring-push-rules-test-bob-access-token";
const CHARLIE_RING_TOKEN: &str = "call-ring-push-rules-test-charlie-access-token";
const DANA_RING_TOKEN: &str = "call-ring-push-rules-test-dana-access-token";

struct RingTestUsers<'a> {
	alice: client::Client<'a>,
	bob: client::Client<'a>,
	charlie: client::Client<'a>,
	dana: client::Client<'a>,
	alice_id: tuwunel_core::ruma::OwnedUserId,
	bob_id: tuwunel_core::ruma::OwnedUserId,
	charlie_id: tuwunel_core::ruma::OwnedUserId,
	dana_id: tuwunel_core::ruma::OwnedUserId,
}

async fn exercise_call_ring_cases(services: &Services, base: &str) -> Result {
	client::wait_until_ready(services, base).await?;

	let alice_id = client::register(services, "alice", ALICE_RING_TOKEN).await?;
	let bob_id = client::register(services, "bob", BOB_RING_TOKEN).await?;
	let charlie_id = client::register(services, "charlie", CHARLIE_RING_TOKEN).await?;
	let dana_id = client::register(services, "dana", DANA_RING_TOKEN).await?;

	let users = RingTestUsers {
		alice: client::Client { services, base, token: ALICE_RING_TOKEN },
		bob: client::Client { services, base, token: BOB_RING_TOKEN },
		charlie: client::Client {
			services,
			base,
			token: CHARLIE_RING_TOKEN,
		},
		dana: client::Client { services, base, token: DANA_RING_TOKEN },
		alice_id,
		bob_id,
		charlie_id,
		dana_id,
	};

	let room2 = create_room_with(&users.alice, &[(&users.bob, &users.bob_id)]).await?;
	let room3 = create_room_with(&users.alice, &[
		(&users.bob, &users.bob_id),
		(&users.charlie, &users.charlie_id),
	])
	.await?;
	let room4 = create_room_with(&users.alice, &[
		(&users.bob, &users.bob_id),
		(&users.charlie, &users.charlie_id),
		(&users.dana, &users.dana_id),
	])
	.await?;
	let other_room = create_room_with(&users.alice, &[(&users.bob, &users.bob_id)]).await?;

	let (mut bob_rx, _bob_stub) =
		register_stub_pusher(services, &users.bob_id, "bob-ring-key").await?;

	verify_live_clear_and_encrypted_rings(services, &users, &room2, &room3, &mut bob_rx).await?;
	verify_invalid_ring_references(services, &users, &room2, &other_room).await?;
	verify_four_members_and_group_notification(services, &users, &room2, &room4).await?;
	verify_room_mute_and_ignore_precedence(services, &users, &mut bob_rx).await?;
	verify_old_ruleset_merge_get_and_disable_before_get(services, &users, &room2).await
}

async fn create_room_with(
	creator: &client::Client<'_>,
	members: &[(&client::Client<'_>, &UserId)],
) -> Result<OwnedRoomId> {
	let room_id = creator
		.create_room(&json!({
			"preset": "private_chat",
			"power_level_content_override": {
				"events": { CALL_MEMBER_EVENT_TYPE: 0 },
			},
		}))
		.await?;
	for (member_client, member_id) in members {
		creator
			.admit(&room_id, member_client, member_id)
			.await?;
	}
	Ok(room_id)
}

async fn register_stub_pusher(
	services: &Services,
	user: &UserId,
	pushkey: &str,
) -> Result<(CaptureRx, AbortOnDrop)> {
	let listener = TcpListener::bind("127.0.0.1:0").await?;
	let url = format!("http://{}{NOTIFY_PATH}", listener.local_addr()?);
	let action = pusher_action(pushkey, url, false, false);
	services
		.pusher
		.set_pusher(user, device_id!("RINGDEV"), &action)
		.await?;

	let (tx, rx) = unbounded_channel();
	let stub = AbortOnDrop(spawn(stub_gateway(listener, tx, r#"{"rejected":[]}"#.to_owned())));
	Ok((rx, stub))
}

fn active_call_member_content(device_id: &str) -> Value {
	json!({
		"application": "m.call",
		"call_id": "",
		"device_id": device_id,
		"focus_active": { "type": "livekit", "focus_selection": "oldest_membership" },
		"foci_preferred": [],
		"scope": "m.room",
	})
}

async fn send_clear_ring(
	sender: &client::Client<'_>,
	room_id: &RoomId,
	txn_id: &str,
	notification_type: &str,
	membership_id: Option<&EventId>,
) -> Result<OwnedEventId> {
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
	let path = format!("rooms/{room_id}/send/{RTC_NOTIFICATION_EVENT_TYPE}/{txn_id}");
	sender.put_event(&path, &content).await
}

async fn send_encrypted_ring(
	sender: &client::Client<'_>,
	room_id: &RoomId,
	txn_id: &str,
	membership_id: Option<&EventId>,
) -> Result<OwnedEventId> {
	let mut content = json!({
		"algorithm": "m.megolm.v1.aes-sha2",
		"ciphertext": "encrypted ring payload",
		"device_id": "DEV1",
		"sender_key": "sender-key",
		"session_id": "session-1",
		RING_HINT_KEY: {
			"lifetime": 30_000,
			"m.call.intent": "audio",
			"notification_type": "ring",
			"sender_ts": 1_795_300_000_000_i64,
		},
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
	let path = format!("rooms/{room_id}/send/m.room.encrypted/{txn_id}");
	sender.put_event(&path, &content).await
}

async fn eval_actions_for_event(
	services: &Services,
	user: &UserId,
	event_id: &EventId,
) -> Result<Vec<Action>> {
	let pdu = services.timeline.get_pdu(event_id).await?;
	let ruleset = services.pusher.ruleset(user).await;
	let power_levels = services
		.state_accessor
		.get_power_levels(pdu.room_id())
		.await
		.ok();
	let serialized = pdu.to_format();
	let actions = services
		.pusher
		.get_actions(Evaluate {
			user,
			ruleset: &ruleset,
			power_levels: power_levels.as_ref(),
			pdu: &serialized,
			room_id: pdu.room_id(),
			related_events: None,
		})
		.await;
	Ok(actions.to_vec())
}

fn has_ring_sound_action(actions: &[Action]) -> bool {
	actions.iter().any(|action| {
		matches!(action, Action::SetTweak(Tweak::Sound(sound)) if sound.as_str() == RING_SOUND_TWEAK)
	})
}

fn has_any_sound_action(actions: &[Action]) -> bool {
	actions
		.iter()
		.any(|action| matches!(action, Action::SetTweak(Tweak::Sound(_))))
}

async fn recv_push_for_event(rx: &mut CaptureRx, want_event_id: &EventId) -> Result<Value> {
	let deadline = Duration::from_secs(5);
	timeout(deadline, async {
		loop {
			let Some((_path, body)) = rx.recv().await else {
				return Err!("stub gateway closed before delivering {want_event_id}");
			};
			let parsed: Value = serde_json::from_slice(&body)?;
			let notif = notification(&parsed)?;
			if notif.get("event_id").and_then(Value::as_str) == Some(want_event_id.as_str()) {
				return Ok(parsed);
			}
		}
	})
	.await
	.map_err(|_| err!("timed out waiting for push for {want_event_id}"))?
}

fn drain_pushes(rx: &mut CaptureRx) { while rx.try_recv().is_ok() {} }

async fn assert_no_push_arrives(rx: &mut CaptureRx, wait: Duration) -> Result {
	match timeout(wait, rx.recv()).await {
		| Err(_) | Ok(None) => Ok(()),
		| Ok(Some((path, body))) =>
			Err!("unexpected push delivered to {path}: {}", String::from_utf8_lossy(&body)),
	}
}

async fn verify_live_clear_and_encrypted_rings(
	services: &Services,
	users: &RingTestUsers<'_>,
	room2: &RoomId,
	room3: &RoomId,
	bob_rx: &mut CaptureRx,
) -> Result {
	// 1. Clear ring in 2-member room with live call membership delivers sound: ring.
	let mem_clear = users
		.alice
		.put_state(
			room2,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_dev1",
			&active_call_member_content("DEV1"),
		)
		.await?;
	let clear_ring_1 =
		send_clear_ring(&users.alice, room2, "clear-1", "ring", Some(&mem_clear)).await?;

	let push = recv_push_for_event(bob_rx, &clear_ring_1).await?;
	let notif = notification(&push)?;
	if !matches!(notif.get("prio").and_then(Value::as_str), None | Some("high")) {
		return Err!(
			"expected high priority push (omitted or high), got {:?}",
			notif.get("prio")
		);
	}
	expect_str(notif, "type", RTC_NOTIFICATION_EVENT_TYPE)?;
	let device = first_device(notif)?;
	expect_str(&device["tweaks"], "sound", RING_SOUND_TWEAK)?;
	expect_absent(&device["tweaks"], "highlight")?;

	// 2. Second clear ring for the same membership is not audible.
	let clear_ring_2 =
		send_clear_ring(&users.alice, room2, "clear-2", "ring", Some(&mem_clear)).await?;
	let second_clear_actions =
		eval_actions_for_event(services, &users.bob_id, &clear_ring_2).await?;
	if !second_clear_actions.is_empty() {
		return Err!(
			"second clear ring for same membership should be silent: {second_clear_actions:?}"
		);
	}
	assert_no_push_arrives(bob_rx, Duration::from_millis(300)).await?;

	// 3. Encrypted ring in 2-member room with live call membership delivers sound: ring.
	let mem_enc = users
		.alice
		.put_state(
			room2,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_dev2",
			&active_call_member_content("DEV2"),
		)
		.await?;
	let enc_ring_1 = send_encrypted_ring(&users.alice, room2, "enc-1", Some(&mem_enc)).await?;

	let push = recv_push_for_event(bob_rx, &enc_ring_1).await?;
	let notif = notification(&push)?;
	if !matches!(notif.get("prio").and_then(Value::as_str), None | Some("high")) {
		return Err!(
			"expected high priority push (omitted or high), got {:?}",
			notif.get("prio")
		);
	}
	expect_str(notif, "type", "m.room.encrypted")?;
	let device = first_device(notif)?;
	expect_str(&device["tweaks"], "sound", RING_SOUND_TWEAK)?;
	expect_absent(&device["tweaks"], "highlight")?;

	// 4. 3-member room: first encrypted ring gives sound: ring; second for same membership is not audible.
	let mem_3 = users
		.alice
		.put_state(
			room3,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_r3",
			&active_call_member_content("DEV3"),
		)
		.await?;
	let enc_r3_1 = send_encrypted_ring(&users.alice, room3, "enc-r3-1", Some(&mem_3)).await?;
	let push = recv_push_for_event(bob_rx, &enc_r3_1).await?;
	let device = first_device(notification(&push)?)?;
	expect_str(&device["tweaks"], "sound", RING_SOUND_TWEAK)?;

	let enc_r3_2 = send_encrypted_ring(&users.alice, room3, "enc-r3-2", Some(&mem_3)).await?;
	let second_enc_actions = eval_actions_for_event(services, &users.bob_id, &enc_r3_2).await?;
	if has_any_sound_action(&second_enc_actions) {
		return Err!(
			"second encrypted ring for same membership in 3-member room must not be audible: \
			 {second_enc_actions:?}"
		);
	}
	drain_pushes(bob_rx);

	Ok(())
}

async fn assert_ring_refused(
	services: &Services,
	bob_id: &UserId,
	alice: &client::Client<'_>,
	room_id: &RoomId,
	label: &str,
	membership_id: Option<&EventId>,
) -> Result {
	let clear_id =
		send_clear_ring(alice, room_id, &format!("refuse-clear-{label}"), "ring", membership_id)
			.await?;
	let clear_actions = eval_actions_for_event(services, bob_id, &clear_id).await?;
	if !clear_actions.is_empty() {
		return Err!("{label}: clear ring should get empty actions, got {clear_actions:?}");
	}

	let enc_id =
		send_encrypted_ring(alice, room_id, &format!("refuse-enc-{label}"), membership_id)
			.await?;
	let enc_actions = eval_actions_for_event(services, bob_id, &enc_id).await?;
	if has_ring_sound_action(&enc_actions) {
		return Err!("{label}: encrypted ring must not get sound=ring, got {enc_actions:?}");
	}
	if !enc_actions.iter().any(Action::should_notify) {
		return Err!(
			"{label}: encrypted ring should still get today's notify action, got {enc_actions:?}"
		);
	}

	Ok(())
}

async fn verify_invalid_ring_references(
	services: &Services,
	users: &RingTestUsers<'_>,
	room2: &RoomId,
	other_room: &RoomId,
) -> Result {
	// No reference.
	assert_ring_refused(services, &users.bob_id, &users.alice, room2, "no-ref", None).await?;

	// Another user's call membership.
	let bob_mem = users
		.bob
		.put_state(
			room2,
			CALL_MEMBER_EVENT_TYPE,
			"_bob_dev1",
			&active_call_member_content("BOB1"),
		)
		.await?;
	assert_ring_refused(
		services,
		&users.bob_id,
		&users.alice,
		room2,
		"other-user",
		Some(&bob_mem),
	)
	.await?;

	// Left membership (empty content `{}`).
	let left_empty = users
		.alice
		.put_state(room2, CALL_MEMBER_EVENT_TYPE, "_alice_left_empty", &json!({}))
		.await?;
	assert_ring_refused(
		services,
		&users.bob_id,
		&users.alice,
		room2,
		"left-empty",
		Some(&left_empty),
	)
	.await?;

	// Left membership (legacy empty `memberships: []`).
	let left_legacy = users
		.alice
		.put_state(
			room2,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_left_legacy",
			&json!({ "memberships": [] }),
		)
		.await?;
	assert_ring_refused(
		services,
		&users.bob_id,
		&users.alice,
		room2,
		"left-legacy",
		Some(&left_legacy),
	)
	.await?;

	// Replaced membership (joined, then replaced by leave on the same state key).
	let replaced_mem = users
		.alice
		.put_state(
			room2,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_replaced",
			&active_call_member_content("DEV_R"),
		)
		.await?;
	users
		.alice
		.put_state(room2, CALL_MEMBER_EVENT_TYPE, "_alice_replaced", &json!({}))
		.await?;
	assert_ring_refused(
		services,
		&users.bob_id,
		&users.alice,
		room2,
		"replaced",
		Some(&replaced_mem),
	)
	.await?;

	// Leave event with only `leave_reason`.
	let left_reason = users
		.alice
		.put_state(
			room2,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_leave_reason",
			&json!({ "leave_reason": "user_left" }),
		)
		.await?;
	assert_ring_refused(
		services,
		&users.bob_id,
		&users.alice,
		room2,
		"leave-reason",
		Some(&left_reason),
	)
	.await?;

	// Malformed call.member content (missing required focus_active / device_id).
	let malformed_mem = users
		.alice
		.put_state(
			room2,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_malformed",
			&json!({ "application": "m.call" }),
		)
		.await?;
	assert_ring_refused(
		services,
		&users.bob_id,
		&users.alice,
		room2,
		"malformed",
		Some(&malformed_mem),
	)
	.await?;

	// Expired call.member (created_ts + expires in the past).
	let expired_mem = users
		.alice
		.put_state(
			room2,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_expired",
			&json!({
				"application": "m.call",
				"call_id": "",
				"created_ts": 1_000,
				"device_id": "DEV_EXP",
				"expires": 1_000,
				"focus_active": { "type": "livekit", "focus_selection": "oldest_membership" },
				"scope": "m.room",
			}),
		)
		.await?;
	assert_ring_refused(
		services,
		&users.bob_id,
		&users.alice,
		room2,
		"expired",
		Some(&expired_mem),
	)
	.await?;

	// Live legacy `memberships` list shape is accepted.
	let legacy_live_mem = users
		.alice
		.put_state(
			room2,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_legacy_live",
			&json!({
				"memberships": [active_call_member_content("DEV_LEG")],
			}),
		)
		.await?;
	let legacy_ring =
		send_clear_ring(&users.alice, room2, "legacy-live-clear", "ring", Some(&legacy_live_mem))
			.await?;
	let legacy_actions = eval_actions_for_event(services, &users.bob_id, &legacy_ring).await?;
	if !has_ring_sound_action(&legacy_actions) {
		return Err!("live legacy memberships list should ring, got {legacy_actions:?}");
	}

	// Failed validation does not mutate the event: a fallback user rule reading
	// `content.notification_type` still sees the original event untouched.
	let unmutated_ring = send_clear_ring(&users.alice, room2, "unmutated-clear", "ring", None).await?;
	let mut fallback_ruleset = services.pusher.ruleset(&users.bob_id).await;
	fallback_ruleset
		.insert(
			NewPushRule::Underride(NewConditionalPushRule::new(
				"custom_sees_original_ring_field".into(),
				vec![PushCondition::EventPropertyIs(EventPropertyIsConditionData::new(
					"content.notification_type".into(),
					ScalarJsonValue::String("ring".into()),
				))]
				.into(),
				vec![Action::Notify].into(),
			)),
			None,
			None,
		)
		.map_err(|e| err!("insert custom fallback rule: {e}"))?;
	let unmutated_pdu = services.timeline.get_pdu(&unmutated_ring).await?;
	let unmutated_raw = unmutated_pdu.to_format();
	let fallback_actions = services
		.pusher
		.get_actions(Evaluate {
			user: &users.bob_id,
			ruleset: &fallback_ruleset,
			power_levels: None,
			pdu: &unmutated_raw,
			room_id: room2,
			related_events: None,
		})
		.await;
	if !fallback_actions.iter().any(Action::should_notify) || has_ring_sound_action(fallback_actions) {
		return Err!(
			"fallback user rule must see unmutated original event without ring tweak: 			 {fallback_actions:?}"
		);
	}

	// Case-variant event type does not match the exact ring rules even with a live membership.
	let live_case_mem = users
		.alice
		.put_state(
			room2,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_case",
			&active_call_member_content("DEV_CASE"),
		)
		.await?;
	let upper_type_event = users
		.alice
		.put_event(
			&format!("rooms/{room2}/send/ORG.MATRIX.MSC4075.RTC.NOTIFICATION/case-variant"),
			&json!({
				"lifetime": 30_000,
				"m.call.intent": "audio",
				"m.relates_to": { "rel_type": "m.reference", "event_id": live_case_mem },
				"notification_type": "ring",
				"sender_ts": 1_795_300_000_000_i64,
			}),
		)
		.await?;
	let upper_actions = eval_actions_for_event(services, &users.bob_id, &upper_type_event).await?;
	if has_ring_sound_action(&upper_actions) {
		return Err!("case-variant event type must not get sound=ring: {upper_actions:?}");
	}

	// Sender no longer joined to the room: ring referencing their old state is rejected.
	let leave_room = create_room_with(&users.alice, &[(&users.bob, &users.bob_id)]).await?;
	let left_sender_mem = users
		.alice
		.put_state(
			&leave_room,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_left_room",
			&active_call_member_content("DEV_LEAVE"),
		)
		.await?;
	let ring_before_leave =
		send_clear_ring(&users.alice, &leave_room, "before-leave", "ring", Some(&left_sender_mem))
			.await?;
	users.alice.act(&leave_room, "leave").await?;
	let after_leave_actions =
		eval_actions_for_event(services, &users.bob_id, &ring_before_leave).await?;
	if !after_leave_actions.is_empty() {
		return Err!(
			"ring from sender who left the room must not ring: {after_leave_actions:?}"
		);
	}

	// Live membership in another room.
	let other_room_mem = users
		.alice
		.put_state(
			other_room,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_other_room",
			&active_call_member_content("DEV_O"),
		)
		.await?;
	assert_ring_refused(
		services,
		&users.bob_id,
		&users.alice,
		room2,
		"other-room",
		Some(&other_room_mem),
	)
	.await
}

async fn verify_four_members_and_group_notification(
	services: &Services,
	users: &RingTestUsers<'_>,
	room2: &RoomId,
	room4: &RoomId,
) -> Result {
	let mem4 = users
		.alice
		.put_state(
			room4,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_r4",
			&active_call_member_content("DEV4"),
		)
		.await?;
	let clear4 = send_clear_ring(&users.alice, room4, "r4-clear", "ring", Some(&mem4)).await?;
	let actions4 = eval_actions_for_event(services, &users.bob_id, &clear4).await?;
	if !actions4.is_empty() {
		return Err!("four-member room clear ring must not ring: {actions4:?}");
	}

	let mem4_enc = users
		.alice
		.put_state(
			room4,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_r4_enc",
			&active_call_member_content("DEV4E"),
		)
		.await?;
	let enc4 = send_encrypted_ring(&users.alice, room4, "r4-enc", Some(&mem4_enc)).await?;
	let enc_actions4 = eval_actions_for_event(services, &users.bob_id, &enc4).await?;
	if has_ring_sound_action(&enc_actions4) {
		return Err!(
			"four-member room encrypted ring must not emit sound=ring: {enc_actions4:?}"
		);
	}

	let mem_group = users
		.alice
		.put_state(
			room2,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_group_notif",
			&active_call_member_content("DEVG"),
		)
		.await?;
	let group_notif =
		send_clear_ring(&users.alice, room2, "group-notif", "notification", Some(&mem_group))
			.await?;
	let group_actions = eval_actions_for_event(services, &users.bob_id, &group_notif).await?;
	if !group_actions.is_empty() {
		return Err!("group notification should be unchanged (empty actions): {group_actions:?}");
	}

	Ok(())
}

async fn verify_room_mute_and_ignore_precedence(
	services: &Services,
	users: &RingTestUsers<'_>,
	bob_rx: &mut CaptureRx,
) -> Result {
	// Mute room2 for Bob with a user override rule.
	let mute_room = create_room_with(&users.alice, &[(&users.bob, &users.bob_id)]).await?;
	users
		.bob
		.put(
			&format!("pushrules/global/override/{mute_room}"),
			&json!({
				"conditions": [{ "kind": "event_match", "key": "room_id", "pattern": mute_room }],
				"actions": [],
			}),
		)
		.await?;
	drain_pushes(bob_rx);

	let mem_mute = users
		.alice
		.put_state(
			&mute_room,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_mute",
			&active_call_member_content("DEVM"),
		)
		.await?;
	let muted_clear =
		send_clear_ring(&users.alice, &mute_room, "muted-clear", "ring", Some(&mem_mute)).await?;
	if !eval_actions_for_event(services, &users.bob_id, &muted_clear)
		.await?
		.is_empty()
	{
		return Err!("muted room must suppress clear ring");
	}
	let mem_mute_enc = users
		.alice
		.put_state(
			&mute_room,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_mute_enc",
			&active_call_member_content("DEVME"),
		)
		.await?;
	let muted_enc =
		send_encrypted_ring(&users.alice, &mute_room, "muted-enc", Some(&mem_mute_enc)).await?;
	if !eval_actions_for_event(services, &users.bob_id, &muted_enc)
		.await?
		.is_empty()
	{
		return Err!("muted room must suppress encrypted ring");
	}
	assert_no_push_arrives(bob_rx, Duration::from_millis(300)).await?;

	// Room-kind mute (`RuleKind::Room` with non-notifying actions, as matrix-js-sdk writes):
	// both clear and encrypted rings must stay silent, and disabling the room mute restores ringing.
	let room_kind_mute = create_room_with(&users.alice, &[(&users.bob, &users.bob_id)]).await?;
	users
		.bob
		.put(&format!("pushrules/global/room/{room_kind_mute}"), &json!({ "actions": [] }))
		.await?;
	drain_pushes(bob_rx);

	let mem_rm_clear = users
		.alice
		.put_state(
			&room_kind_mute,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_rm_clear",
			&active_call_member_content("DEV_RMC"),
		)
		.await?;
	let rm_clear =
		send_clear_ring(&users.alice, &room_kind_mute, "rm-clear", "ring", Some(&mem_rm_clear))
			.await?;
	if !eval_actions_for_event(services, &users.bob_id, &rm_clear)
		.await?
		.is_empty()
	{
		return Err!("room-kind mute must suppress clear ring");
	}
	let mem_rm_enc = users
		.alice
		.put_state(
			&room_kind_mute,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_rm_enc",
			&active_call_member_content("DEV_RME"),
		)
		.await?;
	let rm_enc =
		send_encrypted_ring(&users.alice, &room_kind_mute, "rm-enc", Some(&mem_rm_enc)).await?;
	if !eval_actions_for_event(services, &users.bob_id, &rm_enc)
		.await?
		.is_empty()
	{
		return Err!("room-kind mute must suppress encrypted ring");
	}
	assert_no_push_arrives(bob_rx, Duration::from_millis(300)).await?;

	// Disabling the room-kind mute restores ringing for the same membership (slot was not claimed).
	users
		.bob
		.put(
			&format!("pushrules/global/room/{room_kind_mute}/enabled"),
			&json!({ "enabled": false }),
		)
		.await?;
	let unmuted_actions = eval_actions_for_event(services, &users.bob_id, &rm_clear).await?;
	if !has_ring_sound_action(&unmuted_actions) {
		return Err!(
			"disabling room-kind mute should allow ring (and muted check must not claim dedup): 			 {unmuted_actions:?}"
		);
	}
	drain_pushes(bob_rx);

	// Ignore Alice for Dana, then send rings in a 2-member room with Alice and Dana.
	let ignore_room = create_room_with(&users.alice, &[(&users.dana, &users.dana_id)]).await?;
	let (mut dana_rx, _dana_stub) =
		register_stub_pusher(services, &users.dana_id, "dana-ignore-key").await?;
	users
		.dana
		.put(
			&format!("user/{}/account_data/m.ignored_user_list", users.dana_id),
			&json!({ "ignored_users": { users.alice_id.as_str(): {} } }),
		)
		.await?;

	let dana_notifs_before = services
		.pusher
		.get_notifications(&users.dana_id, None)
		.count()
		.await;

	let mem_ign = users
		.alice
		.put_state(
			&ignore_room,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_ign",
			&active_call_member_content("DEVI"),
		)
		.await?;
	send_clear_ring(&users.alice, &ignore_room, "ign-clear", "ring", Some(&mem_ign)).await?;
	let mem_ign_enc = users
		.alice
		.put_state(
			&ignore_room,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_ign_enc",
			&active_call_member_content("DEVIE"),
		)
		.await?;
	send_encrypted_ring(&users.alice, &ignore_room, "ign-enc", Some(&mem_ign_enc)).await?;
	assert_no_push_arrives(&mut dana_rx, Duration::from_millis(300)).await?;

	let dana_notifs_after = services
		.pusher
		.get_notifications(&users.dana_id, None)
		.count()
		.await;
	if dana_notifs_after != dana_notifs_before {
		return Err!(
			"ignored sender must not record notifications for recipient: \
			 before={dana_notifs_before}, 			 after={dana_notifs_after}"
		);
	}
	Ok(())
}

fn override_rule_ids(value: &Value) -> Vec<String> {
	value
		.get("override")
		.and_then(Value::as_array)
		.into_iter()
		.flatten()
		.filter_map(|rule| {
			rule.get("rule_id")
				.and_then(Value::as_str)
				.map(ToOwned::to_owned)
		})
		.collect()
}

async fn store_old_ruleset(services: &Services, user: &UserId) -> Result {
	let ty = GlobalAccountDataEventType::PushRules;
	let event = PushRulesEvent {
		content: PushRulesEventContent { global: Ruleset::server_default(user) },
	};
	services
		.account_data
		.update(None, user, ty.to_string().into(), &serde_json::to_value(event)?)
		.await
}

async fn verify_old_ruleset_merge_get_and_disable_before_get(
	services: &Services,
	users: &RingTestUsers<'_>,
	room2: &RoomId,
) -> Result {
	// Store an old ruleset (without MindRoom's ring rules) for Bob.
	store_old_ruleset(services, &users.bob_id).await?;

	// Evaluation merges on read immediately.
	let mem_old = users
		.alice
		.put_state(
			room2,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_old_rules",
			&active_call_member_content("DEVO"),
		)
		.await?;
	let clear_old =
		send_clear_ring(&users.alice, room2, "old-rules-clear", "ring", Some(&mem_old)).await?;
	let actions = eval_actions_for_event(services, &users.bob_id, &clear_old).await?;
	if !has_ring_sound_action(&actions) {
		return Err!("stored old ruleset must merge ring rules for evaluation: {actions:?}");
	}

	// Both GET pushrules endpoints return the merged ring rules.
	let all_rules = users.bob.get("pushrules/").await?;
	let all_ids = override_rule_ids(&all_rules["global"]);
	if !all_ids.iter().any(|id| id == RING_RULE_ID)
		|| !all_ids
			.iter()
			.any(|id| id == RING_ENCRYPTED_RULE_ID)
	{
		return Err!("GET /pushrules/ missing ring rules: {all_ids:?}");
	}

	let global_rules = users.bob.get("pushrules/global/").await?;
	let global_ids = override_rule_ids(&global_rules);
	if !global_ids.iter().any(|id| id == RING_RULE_ID)
		|| !global_ids
			.iter()
			.any(|id| id == RING_ENCRYPTED_RULE_ID)
	{
		return Err!("GET /pushrules/global/ missing ring rules: {global_ids:?}");
	}

	let enabled = users
		.bob
		.get(&format!("pushrules/global/override/{RING_RULE_ID}/enabled"))
		.await?;
	if enabled.get("enabled").and_then(Value::as_bool) != Some(true) {
		return Err!("single-rule GET enabled should return true: {enabled}");
	}

	// None of the GET calls wrote the merged ruleset to storage.
	let stored_after_get: PushRulesEvent = services
		.account_data
		.get_global(&users.bob_id, GlobalAccountDataEventType::PushRules)
		.await?;
	if stored_after_get
		.content
		.global
		.get(RuleKind::Override, RING_RULE_ID)
		.is_some()
		|| stored_after_get
			.content
			.global
			.get(RuleKind::Override, RING_ENCRYPTED_RULE_ID)
			.is_some()
	{
		return Err!("GET pushrules must not write ring rules to stored account data");
	}

	// Genuinely legacy stored ruleset containing `.m.rule.contains_display_name` (and missing
	// `.m.rule.is_user_mention`): ordinary message evaluation is unchanged, ring rules are
	// present for evaluation and GET, single-rule GET does not write, and full GET legacy-repair
	// persists without ring rules (matching main).
	let mut legacy_ruleset = Ruleset::server_default(&users.bob_id);
	legacy_ruleset
		.override_
		.shift_remove(PredefinedOverrideRuleId::IsUserMention.as_str());
	#[expect(deprecated)]
	legacy_ruleset
		.override_
		.insert(ConditionalPushRule::contains_display_name());
	let legacy_event = PushRulesEvent {
		content: PushRulesEventContent { global: legacy_ruleset },
	};
	services
		.account_data
		.update(
			None,
			&users.bob_id,
			GlobalAccountDataEventType::PushRules
				.to_string()
				.into(),
			&serde_json::to_value(legacy_event)?,
		)
		.await?;

	// Ordinary message containing Bob's display name still triggers highlight via legacy rule.
	let display_msg = users
		.alice
		.put_event(
			&format!("rooms/{room2}/send/m.room.message/legacy-display-name"),
			&json!({ "msgtype": "m.text", "body": "hello bob" }),
		)
		.await?;
	let legacy_msg_actions = eval_actions_for_event(services, &users.bob_id, &display_msg).await?;
	let has_highlight = legacy_msg_actions.iter().any(|action| {
		matches!(action, Action::SetTweak(Tweak::Highlight(HighlightTweakValue::Yes)))
	});
	if !has_highlight {
		return Err!(
			"genuinely legacy ruleset ordinary message evaluation must remain unchanged: 			 {legacy_msg_actions:?}"
		);
	}

	// Ring rule still evaluates on the legacy ruleset.
	let mem_leg_eval = users
		.alice
		.put_state(
			room2,
			CALL_MEMBER_EVENT_TYPE,
			"_alice_leg_eval",
			&active_call_member_content("DEV_LEGEV"),
		)
		.await?;
	let ring_leg_eval =
		send_clear_ring(&users.alice, room2, "legacy-eval-ring", "ring", Some(&mem_leg_eval))
			.await?;
	let ring_leg_actions = eval_actions_for_event(services, &users.bob_id, &ring_leg_eval).await?;
	if !has_ring_sound_action(&ring_leg_actions) {
		return Err!("ring rule must evaluate on genuinely legacy ruleset: {ring_leg_actions:?}");
	}

	// Single-rule GET sees the ring rule without writing or stripping the legacy rule.
	let leg_enabled = users
		.bob
		.get(&format!("pushrules/global/override/{RING_RULE_ID}/enabled"))
		.await?;
	if leg_enabled.get("enabled").and_then(Value::as_bool) != Some(true) {
		return Err!("single-rule GET on legacy ruleset should return true: {leg_enabled}");
	}
	let stored_before_full_get: PushRulesEvent = services
		.account_data
		.get_global(&users.bob_id, GlobalAccountDataEventType::PushRules)
		.await?;
	#[expect(deprecated)]
	let contains_dn_id = PredefinedOverrideRuleId::ContainsDisplayName.as_str();
	if stored_before_full_get
		.content
		.global
		.get(RuleKind::Override, contains_dn_id)
		.is_none()
		|| stored_before_full_get
			.content
			.global
			.get(RuleKind::Override, RING_RULE_ID)
			.is_some()
	{
		return Err!("single-rule GET must not mutate legacy stored ruleset");
	}

	// Full GET returns ring rules, while existing legacy-repair persists without ring rules.
	let leg_all = users.bob.get("pushrules/").await?;
	let leg_all_ids = override_rule_ids(&leg_all["global"]);
	if !leg_all_ids.iter().any(|id| id == RING_RULE_ID)
		|| !leg_all_ids.iter().any(|id| id == RING_ENCRYPTED_RULE_ID)
	{
		return Err!("GET /pushrules/ on legacy ruleset missing ring rules: {leg_all_ids:?}");
	}
	let stored_after_legacy_get: PushRulesEvent = services
		.account_data
		.get_global(&users.bob_id, GlobalAccountDataEventType::PushRules)
		.await?;
	if stored_after_legacy_get
		.content
		.global
		.get(RuleKind::Override, RING_RULE_ID)
		.is_some()
		|| stored_after_legacy_get
			.content
			.global
			.get(RuleKind::Override, RING_ENCRYPTED_RULE_ID)
			.is_some()
	{
		return Err!("GET legacy repair must not persist ring defaults");
	}

	// A user with an old stored ruleset can disable a ring rule before any GET.
	store_old_ruleset(services, &users.charlie_id).await?;
	users
		.charlie
		.put(
			&format!("pushrules/global/override/{RING_RULE_ID}/enabled"),
			&json!({ "enabled": false }),
		)
		.await?;

	// That write persisted the merged ruleset with the clear ring rule disabled.
	let stored_charlie: PushRulesEvent = services
		.account_data
		.get_global(&users.charlie_id, GlobalAccountDataEventType::PushRules)
		.await?;
	let clear_rule = stored_charlie
		.content
		.global
		.get(RuleKind::Override, RING_RULE_ID)
		.ok_or_else(|| err!("clear ring rule not persisted on disable write"))?;
	if clear_rule.enabled() {
		return Err!("clear ring rule should be persisted as disabled");
	}
	let enc_rule = stored_charlie
		.content
		.global
		.get(RuleKind::Override, RING_ENCRYPTED_RULE_ID)
		.ok_or_else(|| err!("encrypted ring rule not persisted on disable write"))?;
	if !enc_rule.enabled() {
		return Err!("encrypted ring rule should remain enabled");
	}

	Ok(())
}
