#![cfg(test)]

use std::time::{Duration, Instant};

use futures::future::{join, join_all};
use reqwest::{Response, StatusCode};
use serde_json::{Value, json};
use tokio::time::{sleep, timeout};
use tuwunel_core::{
	Result, implement,
	ruma::{OwnedRoomId, RoomId},
};
use tuwunel_service::Services;

use self::{
	client::{Client, field, register},
	fixture::boot,
};

mod client;
mod fixture;

const ALICE_TOKEN: &str = "ephemeral-msc2477-alice-access-token";
const BOB_TOKEN: &str = "ephemeral-msc2477-bob-access-token";
const LIVE: &str = "io.mindroom.live";

/// Every boot leaves the process-wide tracing subscriber alone, so the tests
/// of this binary can boot their servers side by side, and lets a sync with
/// `timeout=0` answer at once.
const OPTIONS: [&str; 2] = ["log_enable=false", "client_sync_timeout_min=0"];

/// Both flags are on by default, so the runtime knows frames reach opted-in
/// syncs.
#[test]
fn versions_advertise_msc2477_and_opt_in() -> Result {
	boot("ephemeral-msc2477-versions", OPTIONS, async |services, base| {
		let features = unstable_features(services, base).await?;

		assert_eq!(features["org.matrix.msc2477"], true, "{features}");
		assert_eq!(features["io.mindroom.ephemeral_opt_in"], true, "{features}");

		Ok(())
	})
}

/// With the option off the route is unknown and neither flag is advertised.
#[test]
fn disabled_config_hides_route_and_flags() -> Result {
	let options = [OPTIONS[0], OPTIONS[1], "mindroom_ephemeral_events_enabled=false"];

	boot("ephemeral-msc2477-disabled", options, async |services, base| {
		let features = unstable_features(services, base).await?;

		assert!(features.get("org.matrix.msc2477").is_none(), "{features}");
		assert!(
			features
				.get("io.mindroom.ephemeral_opt_in")
				.is_none(),
			"{features}"
		);

		let (alice, _, room_id) = joined_pair(services, base).await?;
		let response = alice
			.put_ephemeral(&room_id, LIVE, "disabled", &json!({ "x": 1 }))
			.await?;

		expect_refusal(response, StatusCode::NOT_FOUND, "M_UNRECOGNIZED").await?;

		Ok(())
	})
}

#[test]
fn refuses_builtin_types() -> Result {
	boot("ephemeral-msc2477-builtin", OPTIONS, async |services, base| {
		let (alice, _, room_id) = joined_pair(services, base).await?;
		let response = alice
			.put_ephemeral(&room_id, "m.typing", "builtin", &json!({ "user_ids": [] }))
			.await?;

		let body = expect_refusal(response, StatusCode::BAD_REQUEST, "M_UNKNOWN").await?;

		assert!(
			body["error"]
				.as_str()
				.is_some_and(|error| error
					.contains("Cannot send built-in ephemeral types with this endpoint")),
			"{body}"
		);

		Ok(())
	})
}

#[test]
fn refuses_non_members() -> Result {
	boot("ephemeral-msc2477-non-member", OPTIONS, async |services, base| {
		register(services, "alice", ALICE_TOKEN).await?;
		register(services, "bob", BOB_TOKEN).await?;

		let alice = Client { services, base, token: ALICE_TOKEN };
		let bob = Client { services, base, token: BOB_TOKEN };
		let room_id = alice
			.create_room(&json!({ "preset": "private_chat" }))
			.await?;

		let response = bob
			.put_ephemeral(&room_id, LIVE, "outsider", &json!({ "x": 1 }))
			.await?;

		expect_refusal(response, StatusCode::FORBIDDEN, "M_FORBIDDEN").await?;

		Ok(())
	})
}

/// The limit is on the body's bytes: 65536 is accepted, one more is not.
#[test]
fn refuses_oversized_body() -> Result {
	boot("ephemeral-msc2477-oversized", OPTIONS, async |services, base| {
		let (alice, _, room_id) = joined_pair(services, base).await?;

		// `{"x":""}` is eight bytes.
		let fits = json!({ "x": "a".repeat(65_536 - 8) });
		let response = alice
			.put_ephemeral(&room_id, LIVE, "fits", &fits)
			.await?;

		assert_eq!(response.status(), StatusCode::OK, "{}", response.text().await?);

		let oversized = json!({ "x": "a".repeat(65_537 - 8) });
		let response = alice
			.put_ephemeral(&room_id, LIVE, "oversized", &oversized)
			.await?;

		expect_refusal(response, StatusCode::PAYLOAD_TOO_LARGE, "M_TOO_LARGE").await?;

		Ok(())
	})
}

#[test]
fn refuses_non_object_body() -> Result {
	boot("ephemeral-msc2477-non-object", OPTIONS, async |services, base| {
		let (alice, _, room_id) = joined_pair(services, base).await?;
		let response = alice
			.put_ephemeral(&room_id, LIVE, "array", &json!([1, 2]))
			.await?;

		expect_refusal(response, StatusCode::BAD_REQUEST, "M_BAD_JSON").await?;

		Ok(())
	})
}

/// A burst over 30 events in one second is refused with the time until the
/// window ends.
#[test]
fn rate_limits_bursts() -> Result {
	boot("ephemeral-msc2477-rate", OPTIONS, async |services, base| {
		let (alice, _, room_id) = joined_pair(services, base).await?;
		let content = json!({ "x": 1 });
		let txn_ids: Vec<String> = (0..40).map(|i| format!("burst-{i}")).collect();
		let responses = join_all(
			txn_ids
				.iter()
				.map(|txn_id| alice.put_ephemeral(&room_id, LIVE, txn_id, &content)),
		)
		.await;

		let mut limited = 0_usize;
		for response in responses {
			let response = response?;
			if response.status() == StatusCode::OK {
				continue;
			}

			let body =
				expect_refusal(response, StatusCode::TOO_MANY_REQUESTS, "M_LIMIT_EXCEEDED")
					.await?;

			let retry_after_ms = body["retry_after_ms"].as_u64().unwrap_or(0);

			assert!((1..=1000).contains(&retry_after_ms), "{body}");
			limited += 1;
		}

		assert!(limited > 0, "a burst of 40 was not rate limited");

		Ok(())
	})
}

/// An opted-in incremental sync gets the event once, in the MSC2477 shape.
#[test]
fn opted_in_sync_receives_event_once() -> Result {
	boot("ephemeral-msc2477-once", OPTIONS, async |services, base| {
		let (alice, bob, room_id) = joined_pair(services, base).await?;
		let since = bob.next_batch(Some(&opt_in())).await?;

		alice
			.put_ephemeral(&room_id, LIVE, "once", &json!({ "x": 1 }))
			.await?
			.error_for_status()?;

		let sync = bob.sync(Some(&since), Some(&opt_in()), 0).await?;
		let events = live_events(&sync, &room_id);

		assert_eq!(events.len(), 1, "{sync}");

		let event = &events[0];
		let alice_id = format!("@alice:{}", services.globals.server_name());

		assert_eq!(event["content"], json!({ "x": 1 }), "{event}");
		assert_eq!(event["sender"], alice_id, "{event}");
		assert!(event["origin_server_ts"].is_u64(), "{event}");
		assert!(event.get("room_id").is_none(), "{event}");

		let since = field(&sync, "next_batch")?;
		let sync = bob.sync(Some(since), Some(&opt_in()), 0).await?;

		assert!(live_events(&sync, &room_id).is_empty(), "{sync}");

		Ok(())
	})
}

#[test]
fn two_events_between_syncs_arrive_in_order() -> Result {
	boot("ephemeral-msc2477-order", OPTIONS, async |services, base| {
		let (alice, bob, room_id) = joined_pair(services, base).await?;
		let since = bob.next_batch(Some(&opt_in())).await?;

		for (txn_id, x) in [("first", 1), ("second", 2)] {
			alice
				.put_ephemeral(&room_id, LIVE, txn_id, &json!({ "x": x }))
				.await?
				.error_for_status()?;
		}

		let sync = bob.sync(Some(&since), Some(&opt_in()), 0).await?;
		let contents: Vec<Value> = live_events(&sync, &room_id)
			.into_iter()
			.map(|event| event["content"].clone())
			.collect();

		assert_eq!(contents, [json!({ "x": 1 }), json!({ "x": 2 })], "{sync}");

		Ok(())
	})
}

/// Without the type in `room.ephemeral.types`, or with it in `not_types`, a
/// sync gets nothing.
#[test]
fn not_opted_in_sync_gets_nothing() -> Result {
	boot("ephemeral-msc2477-not-opted-in", OPTIONS, async |services, base| {
		let (alice, bob, room_id) = joined_pair(services, base).await?;
		let other_types =
			json!({ "room": { "ephemeral": { "types": ["m.typing", "m.receipt"] } } });
		let not_types = json!({
			"room": { "ephemeral": { "types": ["*"], "not_types": [LIVE] } },
		});

		let filters = [None, Some(other_types), Some(not_types)];
		let mut tokens = Vec::new();
		for filter in &filters {
			tokens.push(bob.next_batch(filter.as_ref()).await?);
		}

		alice
			.put_ephemeral(&room_id, LIVE, "unseen", &json!({ "x": 1 }))
			.await?
			.error_for_status()?;

		for (filter, since) in filters.iter().zip(&tokens) {
			let sync = bob.sync(Some(since), filter.as_ref(), 0).await?;

			assert!(live_events(&sync, &room_id).is_empty(), "{filter:?}: {sync}");
		}

		Ok(())
	})
}

#[test]
fn initial_sync_gets_nothing() -> Result {
	boot("ephemeral-msc2477-initial", OPTIONS, async |services, base| {
		let (alice, bob, room_id) = joined_pair(services, base).await?;

		alice
			.put_ephemeral(&room_id, LIVE, "fresh", &json!({ "x": 1 }))
			.await?
			.error_for_status()?;

		let sync = bob.sync(None, Some(&opt_in()), 0).await?;

		assert!(live_events(&sync, &room_id).is_empty(), "{sync}");

		Ok(())
	})
}

#[test]
fn ignored_sender_is_skipped() -> Result {
	boot("ephemeral-msc2477-ignored", OPTIONS, async |services, base| {
		let (alice, bob, room_id) = joined_pair(services, base).await?;
		let server_name = services.globals.server_name();
		let ignored = json!({ "ignored_users": { format!("@alice:{server_name}"): {} } });

		bob.put(&format!("user/@bob:{server_name}/account_data/m.ignored_user_list"), &ignored)
			.await?;

		let since = bob.next_batch(Some(&opt_in())).await?;

		alice
			.put_ephemeral(&room_id, LIVE, "ignored", &json!({ "x": 1 }))
			.await?
			.error_for_status()?;

		let sync = bob.sync(Some(&since), Some(&opt_in()), 0).await?;

		assert!(live_events(&sync, &room_id).is_empty(), "{sync}");

		Ok(())
	})
}

#[test]
fn left_member_gets_nothing() -> Result {
	boot("ephemeral-msc2477-left", OPTIONS, async |services, base| {
		let (alice, bob, room_id) = joined_pair(services, base).await?;

		bob.post(&format!("rooms/{room_id}/leave"), &json!({}))
			.await?;

		let since = bob.next_batch(Some(&opt_in())).await?;

		alice
			.put_ephemeral(&room_id, LIVE, "departed", &json!({ "x": 1 }))
			.await?
			.error_for_status()?;

		let sync = bob.sync(Some(&since), Some(&opt_in()), 0).await?;

		assert!(live_events(&sync, &room_id).is_empty(), "{sync}");
		assert!(sync["rooms"]["leave"][room_id.as_str()]["ephemeral"].is_null(), "{sync}");

		Ok(())
	})
}

/// A long poll that opts in returns the event soon after the send.
#[test]
fn long_poll_wakes_when_opted_in() -> Result {
	boot("ephemeral-msc2477-wake", OPTIONS, async |services, base| {
		let (alice, bob, room_id) = joined_pair(services, base).await?;
		let since = bob.next_batch(Some(&opt_in())).await?;
		let filter = opt_in();

		let poll = async {
			let sync = bob
				.sync(Some(&since), Some(&filter), 10_000)
				.await;

			(sync, Instant::now())
		};

		let send = async {
			sleep(PARK).await;

			let sent = Instant::now();
			let response = alice
				.put_ephemeral(&room_id, LIVE, "wake", &json!({ "x": 1 }))
				.await;

			(response, sent)
		};

		let ((sync, returned), (response, sent)) = join(poll, send).await;

		response?.error_for_status()?;

		let sync = sync?;
		let waited = returned.saturating_duration_since(sent);

		assert_eq!(live_events(&sync, &room_id).len(), 1, "{sync}");
		assert!(waited < Duration::from_secs(1), "returned {waited:?} after the send");

		Ok(())
	})
}

/// A long poll that does not opt in keeps waiting through the send.
#[test]
fn long_poll_stays_asleep_when_not_opted_in() -> Result {
	boot("ephemeral-msc2477-asleep", OPTIONS, async |services, base| {
		let (alice, bob, room_id) = joined_pair(services, base).await?;
		let since = bob.next_batch(None).await?;

		let poll =
			timeout(PARK + Duration::from_millis(1_500), bob.sync(Some(&since), None, 10_000));

		let send = async {
			sleep(PARK).await;
			alice
				.put_ephemeral(&room_id, LIVE, "asleep", &json!({ "x": 1 }))
				.await
		};

		let (poll, response) = join(poll, send).await;

		response?.error_for_status()?;

		assert!(poll.is_err(), "the long poll returned: {:?}", poll.map(Result::ok));

		Ok(())
	})
}

/// How long a test waits for the other user's long poll to park before it
/// sends.
const PARK: Duration = Duration::from_millis(500);

/// The filter MindRoom Chat syncs with.
fn opt_in() -> Value {
	json!({ "room": { "ephemeral": { "types": ["m.typing", "m.receipt", LIVE] } } })
}

/// The `io.mindroom.live` events a sync carries for a joined room.
fn live_events(sync: &Value, room_id: &RoomId) -> Vec<Value> {
	sync["rooms"]["join"][room_id.as_str()]["ephemeral"]["events"]
		.as_array()
		.into_iter()
		.flatten()
		.filter(|event| event["type"] == LIVE)
		.cloned()
		.collect()
}

/// Register Alice and Bob, and join both to a room Alice creates.
async fn joined_pair<'a>(
	services: &'a Services,
	base: &'a str,
) -> Result<(Client<'a>, Client<'a>, OwnedRoomId)> {
	register(services, "alice", ALICE_TOKEN).await?;
	let bob_id = register(services, "bob", BOB_TOKEN).await?;

	let alice = Client { services, base, token: ALICE_TOKEN };
	let bob = Client { services, base, token: BOB_TOKEN };
	let room_id = alice
		.create_room(&json!({ "preset": "private_chat" }))
		.await?;

	alice
		.post(&format!("rooms/{room_id}/invite"), &json!({ "user_id": bob_id }))
		.await?;

	bob.post(&format!("rooms/{room_id}/join"), &json!({}))
		.await?;

	Ok((alice, bob, room_id))
}

/// The `unstable_features` map `/versions` advertises.
async fn unstable_features(services: &Services, base: &str) -> Result<Value> {
	let versions: Value = services
		.client
		.clients
		.default
		.get(format!("{base}/_matrix/client/versions"))
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(versions["unstable_features"].clone())
}

/// Assert a refusal's status and errcode and return its body.
async fn expect_refusal(response: Response, status: StatusCode, errcode: &str) -> Result<Value> {
	let actual = response.status();
	let body: Value = response.json().await?;

	assert_eq!(actual, status, "{body}");
	assert_eq!(body["errcode"], errcode, "{body}");

	Ok(body)
}

/// Sync once as this user and parse the reply.
#[implement(Client, params = "<'_>")]
async fn sync(
	&self,
	since: Option<&str>,
	filter: Option<&Value>,
	timeout_ms: u64,
) -> Result<Value> {
	let mut query = vec![("timeout", timeout_ms.to_string())];
	query.extend(since.map(|since| ("since", since.to_owned())));
	query.extend(filter.map(|filter| ("filter", filter.to_string())));

	let response = self
		.services
		.client
		.clients
		.default
		.get(self.url("sync"))
		.bearer_auth(self.token)
		.query(&query)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(response)
}

/// The token an immediate sync with `filter` ends at.
#[implement(Client, params = "<'_>")]
async fn next_batch(&self, filter: Option<&Value>) -> Result<String> {
	let sync = self.sync(None, filter, 0).await?;

	Ok(field(&sync, "next_batch")?.to_owned())
}

/// Put a JSON body to one client-API endpoint path as this user.
#[implement(Client, params = "<'_>")]
async fn put(&self, path: &str, body: &Value) -> Result {
	self.services
		.client
		.clients
		.default
		.put(self.url(path))
		.bearer_auth(self.token)
		.json(body)
		.send()
		.await?
		.error_for_status()?;

	Ok(())
}

/// Put one ephemeral event as this user and keep the raw reply.
#[implement(Client, params = "<'_>")]
async fn put_ephemeral(
	&self,
	room_id: &RoomId,
	event_type: &str,
	txn_id: &str,
	content: &Value,
) -> Result<Response> {
	let url = format!(
		"{}/_matrix/client/unstable/org.matrix.msc2477/rooms/{room_id}/ephemeral/{event_type}/\
		 {txn_id}",
		self.base
	);

	let response = self
		.services
		.client
		.clients
		.default
		.put(url)
		.bearer_auth(self.token)
		.json(content)
		.send()
		.await?;

	Ok(response)
}
