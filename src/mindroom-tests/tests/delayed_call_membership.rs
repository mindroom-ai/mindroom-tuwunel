mod support;

#[cfg(test)]
mod tests {
	use std::time::{Duration, Instant};

	use axum::Router;
	use serde_json::{Value, json};
	use tuwunel_core::{
		Result,
		http::StatusCode,
		ruma::{device_id, user_id},
	};

	use super::support::{
		Harness,
		openid::{authed_get, post, post_as, put, send, setup},
	};

	const CALL_MEMBER: &str = "org.matrix.msc3401.call.member";
	const STATE_KEY: &str = "_%40alice%3Alocalhost_OPENID";
	const BOB_TOKEN: &str = "delayed-events-bob-access-token-0123456789";

	async fn call_member(router: &Router, state: &str) -> Value {
		let (status, content) = send(router, authed_get(state)).await;
		assert_eq!(status, StatusCode::OK, "call member state: {content}");
		content
	}

	async fn schedule_leave(router: &Router, state: &str, millis: u64) -> String {
		let delayed = format!("{state}?org.matrix.msc4140.delay={millis}");
		let (status, body) = send(router, put(&delayed, &json!({}))).await;
		assert_eq!(status, StatusCode::OK, "delayed leave: {body}");
		body["delay_id"]
			.as_str()
			.expect("delayed leave returns delay_id")
			.to_owned()
	}

	fn update(delay_id: &str, action: &str) -> String {
		format!("/_matrix/client/unstable/org.matrix.msc4140/delayed_events/{delay_id}/{action}")
	}

	/// A MatrixRTC client schedules its own leave and keeps restarting it, in
	/// the path form matrix-js-sdk calls or the body form matrix-rust-sdk calls;
	/// when the client stops, as on a crash, the server sends the leave. The
	/// client can also cancel the leave or send it at once, and nobody else can
	/// touch it. Other state, such as a room membership, cannot be delayed, nor
	/// can oversized content or a suspended account's membership.
	#[test]
	fn delayed_call_member_leave_is_sent_once_restarts_stop() -> Result {
		let harness = Harness::new("mindroom_delayed_call_membership", [])?;

		harness.with_services(async |services| {
			let (router, _guard) = setup(&services).await?;
			let bob = user_id!("@bob:localhost");
			services
				.users
				.create(bob, Some("password"), None)
				.await?;
			services
				.users
				.create_device(
					bob,
					Some(device_id!("BOB")),
					(Some(BOB_TOKEN), None),
					None,
					None,
					None,
				)
				.await?;

			let (status, created) =
				send(&router, post("/_matrix/client/v3/createRoom", Some(json!({})))).await;
			assert_eq!(status, StatusCode::OK, "createRoom: {created}");
			let room_id = created["room_id"]
				.as_str()
				.expect("createRoom returns room_id");

			let member = format!(
				"/_matrix/client/v3/rooms/{room_id}/state/m.room.member/%40alice%3Alocalhost?\
				 org.matrix.msc4140.delay=1000"
			);
			let (status, body) =
				send(&router, put(&member, &json!({"membership": "leave"}))).await;
			assert_eq!(status, StatusCode::FORBIDDEN, "delayed membership: {body}");
			assert_eq!(body["errcode"], "M_FORBIDDEN", "delayed membership: {body}");

			let topic = format!(
				"/_matrix/client/v3/rooms/{room_id}/state/m.room.topic?org.matrix.msc4140.\
				 delay=1000"
			);
			let (status, body) = send(&router, put(&topic, &json!({"topic": "later"}))).await;
			assert_eq!(status, StatusCode::FORBIDDEN, "delayed empty-key state: {body}");

			let state =
				format!("/_matrix/client/v3/rooms/{room_id}/state/{CALL_MEMBER}/{STATE_KEY}");
			let joined = json!({"application": "m.call", "call_id": "", "device_id": "OPENID"});
			let (status, body) = send(&router, put(&state, &joined)).await;
			assert_eq!(status, StatusCode::OK, "join: {body}");

			let delay_id = schedule_leave(&router, &state, 2000).await;

			tokio::time::sleep(Duration::from_millis(1200)).await;
			let restart = update(&delay_id, "restart");
			let (status, body) = send(&router, post(&restart, None)).await;
			assert_eq!(status, StatusCode::OK, "restart: {body}");

			tokio::time::sleep(Duration::from_millis(1200)).await;
			let restart_in_body =
				format!("/_matrix/client/unstable/org.matrix.msc4140/delayed_events/{delay_id}");
			let (status, body) =
				send(&router, post(&restart_in_body, Some(json!({"action": "restart"})))).await;
			assert_eq!(status, StatusCode::OK, "restart in body: {body}");

			tokio::time::sleep(Duration::from_millis(1200)).await;
			assert_eq!(call_member(&router, &state).await, joined, "restarts postpone the leave");

			let deadline = Instant::now() + Duration::from_secs(10);
			while call_member(&router, &state).await != json!({}) {
				assert!(Instant::now() < deadline, "the delayed leave was never sent");
				tokio::time::sleep(Duration::from_millis(50)).await;
			}

			// The SDKs schedule a new leave when a restart finds none.
			let (status, body) = send(&router, post(&restart, None)).await;
			assert_eq!(status, StatusCode::NOT_FOUND, "restart after the leave: {body}");
			assert_eq!(body["errcode"], "M_NOT_FOUND", "restart after the leave: {body}");

			let (status, body) = send(&router, put(&state, &joined)).await;
			assert_eq!(status, StatusCode::OK, "rejoin: {body}");
			let delay_id = schedule_leave(&router, &state, 60_000).await;
			let (status, body) =
				send(&router, post_as(BOB_TOKEN, &update(&delay_id, "send"), None)).await;
			assert_eq!(status, StatusCode::NOT_FOUND, "another user's send: {body}");
			let (status, body) = send(&router, post(&update(&delay_id, "cancel"), None)).await;
			assert_eq!(status, StatusCode::OK, "cancel: {body}");
			let (status, body) = send(&router, post(&update(&delay_id, "restart"), None)).await;
			assert_eq!(status, StatusCode::NOT_FOUND, "restart after cancel: {body}");
			assert_eq!(
				call_member(&router, &state).await,
				joined,
				"a cancelled leave is not sent"
			);

			// matrix-js-sdk sends the scheduled leave at once on a normal hang-up.
			let delay_id = schedule_leave(&router, &state, 60_000).await;
			let (status, body) = send(&router, post(&update(&delay_id, "send"), None)).await;
			assert_eq!(status, StatusCode::OK, "send: {body}");
			assert_eq!(call_member(&router, &state).await, json!({}), "send leaves at once");

			let delayed = format!("{state}?org.matrix.msc4140.delay=1000");
			let oversized = json!({"padding": "x".repeat(65_536)});
			let (status, body) = send(&router, put(&delayed, &oversized)).await;
			assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "oversized: {body}");
			assert_eq!(body["errcode"], "M_TOO_LARGE", "oversized: {body}");

			let alice = user_id!("@alice:localhost");
			services.users.set_suspended(alice, alice);
			let (status, body) = send(&router, put(&delayed, &json!({}))).await;
			assert_eq!(status, StatusCode::FORBIDDEN, "suspended: {body}");
			assert_eq!(body["errcode"], "M_USER_SUSPENDED", "suspended: {body}");

			Ok(())
		})
	}
}
