#![cfg(test)]

use futures::future::join_all;
use reqwest::{Response, StatusCode};
use serde_json::{Value, json};
use tuwunel_core::{
	Result, implement,
	ruma::{OwnedRoomId, RoomId},
};
use tuwunel_service::Services;

use self::{
	client::{Client, register},
	fixture::boot,
};

mod client;
mod fixture;

const ALICE_TOKEN: &str = "ephemeral-msc2477-alice-access-token";
const BOB_TOKEN: &str = "ephemeral-msc2477-bob-access-token";
const LIVE: &str = "io.mindroom.live";

/// Every boot leaves the process-wide tracing subscriber alone, so the tests
/// of this binary can boot their servers side by side.
const OPTIONS: [&str; 1] = ["log_enable=false"];

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
	let options = ["log_enable=false", "mindroom_ephemeral_events_enabled=false"];

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
