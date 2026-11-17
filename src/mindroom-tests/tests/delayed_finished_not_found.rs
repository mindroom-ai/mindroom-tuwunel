mod support;

#[cfg(test)]
mod tests {
	use axum::Router;
	use serde_json::{Value, json};
	use tuwunel_core::{Result, http::StatusCode};

	use super::support::{
		Harness,
		openid::{post, put, send, setup},
	};

	const UNSTABLE: &str = "/_matrix/client/unstable/org.matrix.msc4140";
	const STABLE: &str = "/_matrix/client/v1";
	const CALL_MEMBER: &str = "org.matrix.msc3401.call.member";
	const STATE_KEY: &str = "_%40alice%3Alocalhost_OPENID_m.call";

	async fn schedule_leave(router: &Router, state: &str) -> String {
		let delayed = format!("{state}?org.matrix.msc4140.delay=60000");
		let (status, body) = send(router, put(&delayed, &json!({}))).await;
		assert_eq!(status, StatusCode::OK, "delayed leave: {body}");
		body["delay_id"]
			.as_str()
			.expect("delayed leave returns delay_id")
			.to_owned()
	}

	async fn act(
		router: &Router,
		base: &str,
		delay_id: &str,
		action: &str,
	) -> (StatusCode, Value) {
		let uri = format!("{base}/delayed_events/{delay_id}/{action}");

		send(router, post(&uri, None)).await
	}

	/// Each action answers 404 `M_NOT_FOUND` on the unstable path and upstream's
	/// 409 `M_UNKNOWN` on the stable one.
	async fn assert_finished(router: &Router, delay_id: &str, actions: [&str; 2]) {
		for action in actions {
			let (status, body) = act(router, UNSTABLE, delay_id, action).await;
			assert_eq!(status, StatusCode::NOT_FOUND, "unstable {action}: {body}");
			assert_eq!(body["errcode"], "M_NOT_FOUND", "unstable {action}: {body}");

			let (status, body) = act(router, STABLE, delay_id, action).await;
			assert_eq!(status, StatusCode::CONFLICT, "stable {action}: {body}");
			assert_eq!(body["errcode"], "M_UNKNOWN", "stable {action}: {body}");
		}

		let in_body = format!("{UNSTABLE}/delayed_events/{delay_id}");
		let action = json!({"action": actions[0]});
		let (status, body) = send(router, post(&in_body, Some(action))).await;
		assert_eq!(status, StatusCode::NOT_FOUND, "unstable {} in body: {body}", actions[0]);
		assert_eq!(body["errcode"], "M_NOT_FOUND", "unstable {} in body: {body}", actions[0]);
	}

	/// The action does not conflict with the finished delay, so it succeeds
	/// again on both paths.
	async fn assert_repeats(router: &Router, delay_id: &str, action: &str) {
		for base in [UNSTABLE, STABLE] {
			let (status, body) = act(router, base, delay_id, action).await;
			assert_eq!(status, StatusCode::OK, "{base} repeated {action}: {body}");
		}
	}

	/// A MatrixRTC client keeps restarting its delayed leave. Once the leave is
	/// sent or cancelled, matrix-js-sdk schedules a replacement when the restart
	/// or cancel answers `M_NOT_FOUND`, and does not handle a conflict, which
	/// fails the call. It calls the unstable paths, so those answer an action
	/// that conflicts with a finished delay as missing, in the path form and
	/// the earlier body form; the stable path keeps upstream's conflict. A
	/// repeated send of a sent leave and a repeated cancel of a cancelled one
	/// do not conflict, and succeed on both paths.
	#[test]
	fn finished_delay_is_not_found_on_unstable_paths() -> Result {
		let harness = Harness::new("mindroom_delayed_finished_not_found", [])?;

		harness.with_services(async |services| {
			let (router, _guard) = setup(&services).await?;

			let (status, created) =
				send(&router, post("/_matrix/client/v3/createRoom", Some(json!({})))).await;
			assert_eq!(status, StatusCode::OK, "createRoom: {created}");
			let room_id = created["room_id"]
				.as_str()
				.expect("createRoom returns room_id");

			let state =
				format!("/_matrix/client/v3/rooms/{room_id}/state/{CALL_MEMBER}/{STATE_KEY}");

			let sent = schedule_leave(&router, &state).await;
			let (status, body) = act(&router, UNSTABLE, &sent, "send").await;
			assert_eq!(status, StatusCode::OK, "send: {body}");
			assert_finished(&router, &sent, ["restart", "cancel"]).await;
			assert_repeats(&router, &sent, "send").await;

			let cancelled = schedule_leave(&router, &state).await;
			let (status, body) = act(&router, UNSTABLE, &cancelled, "cancel").await;
			assert_eq!(status, StatusCode::OK, "cancel: {body}");
			assert_finished(&router, &cancelled, ["restart", "send"]).await;
			assert_repeats(&router, &cancelled, "cancel").await;

			Ok(())
		})
	}
}
