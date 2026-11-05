mod support;

#[cfg(test)]
mod tests {
	use std::time::{Duration, Instant};

	use axum::Router;
	use serde_json::{Value, json};
	use tuwunel_core::{Result, http::StatusCode, ruma::user_id};

	use super::support::{
		Harness,
		openid::{authed_get, post, put, send, setup},
	};

	const CALL_MEMBER: &str = "org.matrix.msc3401.call.member";
	const STATE_KEY: &str = "_%40alice%3Alocalhost_OPENID";

	async fn call_member(router: &Router, state: &str) -> Value {
		let (status, content) = send(router, authed_get(state)).await;
		assert_eq!(status, StatusCode::OK, "call member state: {content}");
		content
	}

	/// A MatrixRTC client schedules its own leave and keeps restarting it, in
	/// the path form matrix-js-sdk calls or the body form matrix-rust-sdk calls;
	/// when the client stops, as on a crash, the server sends the leave. Other
	/// state, such as a room membership, cannot be delayed, nor can oversized
	/// content or a suspended account's membership.
	#[test]
	fn delayed_call_member_leave_is_sent_once_restarts_stop() -> Result {
		let harness = Harness::new("mindroom_delayed_call_membership", [])?;

		harness.with_services(async |services| {
			let (router, _guard) = setup(&services).await?;
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

			let state =
				format!("/_matrix/client/v3/rooms/{room_id}/state/{CALL_MEMBER}/{STATE_KEY}");
			let joined = json!({"application": "m.call", "call_id": "", "device_id": "OPENID"});
			let (status, body) = send(&router, put(&state, &joined)).await;
			assert_eq!(status, StatusCode::OK, "join: {body}");

			let (status, body) =
				send(&router, put(&format!("{state}?org.matrix.msc4140.delay=2000"), &json!({})))
					.await;
			assert_eq!(status, StatusCode::OK, "delayed leave: {body}");
			let delay_id = body["delay_id"]
				.as_str()
				.expect("delayed leave returns delay_id")
				.to_owned();

			tokio::time::sleep(Duration::from_millis(1200)).await;
			let restart = format!(
				"/_matrix/client/unstable/org.matrix.msc4140/delayed_events/{delay_id}/restart"
			);
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
