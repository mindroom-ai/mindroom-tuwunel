//! A thread root's bundled `m.thread.count`, end to end: thread replies and
//! their redaction sent through the client API, the root as `/event` serves
//! it, and `debug rebuild-thread-index` correcting a count kept before
//! redaction decremented it.

mod support;

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use axum::{Router, body::Body};
	use serde_json::{Value as JsonValue, json};
	use tower::ServiceExt;
	use tuwunel_core::{
		Result,
		http::{Request, StatusCode, header},
		ruma::{CanonicalJsonObject, EventId, user_id},
	};
	use tuwunel_service::Services;

	use super::support::Harness;

	const TOKEN: &str = "mindroom-test-access-token-thread-count-0123456";

	/// One harness per process (the tracing subscriber is global).
	#[test]
	fn thread_count_excludes_redacted_replies() -> Result {
		let harness = Harness::new("mindroom_thread_count_redaction", [])?;

		harness.with_services(async |services| {
			tuwunel_admin::init(&services.admin);
			let result = exercise(&services).await;
			tuwunel_admin::fini(&services.admin);

			result
		})
	}

	async fn exercise(services: &Arc<Services>) -> Result {
		let alice = user_id!("@alice:localhost");
		services
			.users
			.create(alice, Some("password"), None)
			.await?;
		services
			.users
			.create_device(alice, None, (Some(TOKEN), None), None, None, None)
			.await?;

		let (state, _guard) = tuwunel_api::router::state::create(services.clone());
		let router =
			tuwunel_api::router::build(Router::new(), &services.server).with_state(state);
		let created = request(
			&router,
			"POST",
			"/_matrix/client/v3/createRoom",
			Some(json!({"preset": "private_chat"})),
		)
		.await;
		let room = created["room_id"]
			.as_str()
			.expect("createRoom returns room_id")
			.to_owned();

		let root =
			send(&router, &room, "root", json!({"msgtype": "m.text", "body": "root"})).await;
		let first = send(&router, &room, "first", reply(&root, &root, "first")).await;
		let second = send(&router, &room, "second", reply(&root, &first, "second")).await;

		assert_eq!(thread_count(&router, &room, &root).await, 2);

		redact(&router, &room, &first, "redact-first").await;

		assert_eq!(thread_count(&router, &room, &root).await, 1);

		redact(&router, &room, &first, "redact-first-again").await;

		assert_eq!(thread_count(&router, &room, &root).await, 1);

		// Every relation is still served, so clients learn of the redaction, but
		// the thread lists exactly as many replies as the root counts.
		assert_eq!(relations(&router, &room, &root, "").await, [second.as_str(), &first]);
		assert_eq!(relations(&router, &room, &root, "/m.thread").await, [second.as_str()]);

		// A count kept by an earlier version still includes the redacted reply.
		let root_id = services
			.timeline
			.get_pdu_id(<&EventId>::try_from(root.as_str())?)
			.await?;
		let mut stored = serde_json::to_value(
			services
				.timeline
				.get_pdu_json_from_id(&root_id)
				.await?,
		)?;
		stored["unsigned"]["m.relations"]["m.thread"]["count"] = json!(2);
		let stored: CanonicalJsonObject = serde_json::from_value(stored)?;
		services
			.timeline
			.replace_pdu(&root_id, &stored)
			.await?;

		assert_eq!(thread_count(&router, &room, &root).await, 2);

		let output = match services
			.admin
			.command_in_place("debug rebuild-thread-index".to_owned(), None)
			.await
		{
			| Ok(output) => output.map(|output| output.as_str().to_owned()),
			| Err(output) => panic!("rebuild-thread-index failed: {}", output.as_str()),
		};

		assert!(
			output.is_some_and(
				|output| output.contains("Thread roots checked: 1, corrected: 1, failed: 0.")
			),
			"rebuild-thread-index reports one corrected root"
		);
		assert_eq!(thread_count(&router, &room, &root).await, 1);

		Ok(())
	}

	/// A thread reply as clients send it, with the reply fallback.
	fn reply(root: &str, latest: &str, body: &str) -> JsonValue {
		json!({
			"msgtype": "m.text",
			"body": body,
			"m.relates_to": {
				"rel_type": "m.thread",
				"event_id": root,
				"is_falling_back": true,
				"m.in_reply_to": {"event_id": latest},
			},
		})
	}

	/// The root's thread count as `/event` and `/threads` serve it, which
	/// must agree.
	async fn thread_count(router: &Router, room: &str, root: &str) -> JsonValue {
		let event = request(
			router,
			"GET",
			&format!("/_matrix/client/v3/rooms/{}/event/{}", enc(room), enc(root)),
			None,
		)
		.await;
		let threads = request(
			router,
			"GET",
			&format!("/_matrix/client/v1/rooms/{}/threads", enc(room)),
			None,
		)
		.await;
		let listed = threads["chunk"]
			.as_array()
			.expect("threads returns a chunk")
			.iter()
			.find(|thread| thread["event_id"] == root)
			.expect("threads lists the root");

		let count = event["unsigned"]["m.relations"]["m.thread"]["count"].clone();

		assert_eq!(
			listed["unsigned"]["m.relations"]["m.thread"]["count"], count,
			"/threads and /event serve the same count"
		);

		count
	}

	/// Event IDs `/relations` serves for `target`, newest first.
	async fn relations(router: &Router, room: &str, target: &str, filter: &str) -> Vec<String> {
		let relations = request(
			router,
			"GET",
			&format!("/_matrix/client/v1/rooms/{}/relations/{}{filter}", enc(room), enc(target)),
			None,
		)
		.await;

		relations["chunk"]
			.as_array()
			.expect("relations returns a chunk")
			.iter()
			.map(|event| {
				event["event_id"]
					.as_str()
					.expect("related event has an event_id")
					.to_owned()
			})
			.collect()
	}

	async fn redact(router: &Router, room: &str, event: &str, txn_id: &str) {
		request(
			router,
			"PUT",
			&format!("/_matrix/client/v3/rooms/{}/redact/{}/{txn_id}", enc(room), enc(event)),
			Some(json!({})),
		)
		.await;
	}

	async fn send(router: &Router, room: &str, txn_id: &str, content: JsonValue) -> String {
		let sent = request(
			router,
			"PUT",
			&format!("/_matrix/client/v3/rooms/{}/send/m.room.message/{txn_id}", enc(room)),
			Some(content),
		)
		.await;

		sent["event_id"]
			.as_str()
			.expect("send returns event_id")
			.to_owned()
	}

	async fn request(
		router: &Router,
		method: &str,
		uri: &str,
		body: Option<JsonValue>,
	) -> JsonValue {
		let request = Request::builder()
			.method(method)
			.uri(uri)
			.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
			.header(header::CONTENT_TYPE, "application/json")
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
			.expect("readable response body");
		let body = serde_json::from_slice(&bytes).expect("JSON response body");
		assert_eq!(status, StatusCode::OK, "{method} {uri} should succeed: {body}");

		body
	}

	/// Percent-encode a room or event ID for use as a URI path segment.
	fn enc(id: &str) -> String {
		id.replace('$', "%24")
			.replace('!', "%21")
			.replace(':', "%3A")
			.replace('+', "%2B")
			.replace('/', "%2F")
			.replace('=', "%3D")
	}
}
