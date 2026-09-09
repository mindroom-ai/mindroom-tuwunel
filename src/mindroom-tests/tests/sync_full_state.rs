//! Full-state sync must include quiet joined rooms, including encryption state.

mod support;

use axum::{Router, body::Body};
use serde_json::{Value, json};
use support::Harness;
use tower::ServiceExt;
use tuwunel_core::{
	Result,
	http::{Request, StatusCode, header},
	ruma::user_id,
};

const ACCESS_TOKEN: &str = "full-state-test-access-token-0123456789";

#[test]
fn full_state_includes_unchanged_joined_rooms() -> Result {
	let harness = Harness::new("mindroom_sync_full_state", [])?;
	harness.with_services(|services| async move {
		let user = user_id!("@alice:localhost");
		services
			.users
			.create(user, Some("password"), None)
			.await?;
		services
			.users
			.create_device(user, None, (Some(ACCESS_TOKEN), None), None, None, None)
			.await?;
		let (state, _guard) = tuwunel_api::router::state::create(services.clone());
		let router =
			tuwunel_api::router::build(Router::new(), &services.server).with_state(state);

		let plain = create_room(&router, false).await;
		let encrypted = create_room(&router, true).await;
		let initial = request(&router, "GET", "/_matrix/client/v3/sync?timeout=0", None).await;
		let since = initial["next_batch"]
			.as_str()
			.expect("sync cursor");
		let incremental = request(
			&router,
			"GET",
			&format!("/_matrix/client/v3/sync?timeout=0&since={since}"),
			None,
		)
		.await;
		for room in [&plain, &encrypted] {
			assert!(incremental["rooms"]["join"].get(room).is_none(), "quiet incremental room");
		}

		let full = request(
			&router,
			"GET",
			&format!("/_matrix/client/v3/sync?timeout=0&since={since}&full_state=true"),
			None,
		)
		.await;
		for room in [&plain, &encrypted] {
			let joined = &full["rooms"]["join"][room];
			let state = joined["state"]["events"]
				.as_array()
				.expect("full state includes the quiet room");
			assert!(
				state
					.iter()
					.any(|event| event["type"] == "m.room.create")
			);
			assert!(state.iter().any(|event| {
				event["type"] == "m.room.member"
					&& event["state_key"] == "@alice:localhost"
					&& event["content"]["membership"] == "join"
			}));
			assert!(
				joined["timeline"]["events"]
					.as_array()
					.is_none_or(Vec::is_empty)
			);
			let encryption = state
				.iter()
				.find(|event| event["type"] == "m.room.encryption");
			if room == &encrypted {
				assert_eq!(
					encryption.expect("encryption state must not be omitted")["content"]
						["algorithm"],
					"m.megolm.v1.aes-sha2"
				);
			} else {
				assert!(encryption.is_none());
			}
		}
		Ok(())
	})
}

async fn create_room(router: &Router, encrypted: bool) -> String {
	let mut body = json!({"preset": "private_chat"});
	if encrypted {
		body["initial_state"] = json!([{
			"type": "m.room.encryption",
			"state_key": "",
			"content": {"algorithm": "m.megolm.v1.aes-sha2"},
		}]);
	}
	request(router, "POST", "/_matrix/client/v3/createRoom", Some(body)).await["room_id"]
		.as_str()
		.expect("created room ID")
		.to_owned()
}

async fn request(router: &Router, method: &str, uri: &str, body: Option<Value>) -> Value {
	let request = Request::builder()
		.method(method)
		.uri(uri)
		.header(header::CONTENT_TYPE, "application/json")
		.header(header::AUTHORIZATION, format!("Bearer {ACCESS_TOKEN}"))
		.header("X-Forwarded-For", "127.0.0.1")
		.body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
		.expect("valid request");
	let response = router
		.clone()
		.oneshot(request)
		.await
		.expect("router response");
	let status = response.status();
	let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
		.await
		.expect("response body");
	let value: Value = serde_json::from_slice(&bytes).expect("JSON response");
	assert_eq!(status, StatusCode::OK, "request must succeed: {value}");
	value
}
