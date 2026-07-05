//! MSC2675/MSC2676 read-time bundling paired with the fork's edit purge:
//! history endpoints must attach the latest same-sender `m.replace` event as
//! a full-event bundle at `unsigned.m.relations.m.replace`, because the purge
//! deletes all superseded edits and sync compaction collapses the rest — the
//! bundle is the only redundancy protecting clients from permanently caching
//! the pre-edit placeholder body.

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
		ruma::user_id,
	};
	use tuwunel_service::Services;

	use super::support::Harness;

	const ALICE_TOKEN: &str = "mindroom-test-access-token-alice-0123456789";
	const BOB_TOKEN: &str = "mindroom-test-access-token-bob-0123456789ab";

	/// One harness per process (the tracing subscriber is global), so all
	/// endpoint scenarios run against a single server.
	#[test]
	fn history_endpoints_bundle_latest_replacement() -> Result {
		let harness = Harness::new("mindroom_bundled_edit_aggregations", [
			"mindroom_edit_purge_enabled=false".to_owned(),
		])?;

		harness.with_services(|services| async move {
			let (router, room_id) = setup_room(&services).await?;

			relations_and_threads_cases(&router, &room_id).await;
			event_and_context_cases(&router, &room_id).await;
			search_case(&router, &room_id).await;
			encrypted_case(&router, &room_id).await;
			redaction_case(&router, &room_id).await;
			visibility_case(&router).await;
			messages_cases(&services, &router, &room_id).await?;
			// Runs last: it deletes PDUs that are DAG forward-extremities, which
			// would break the prev_event resolution of any later send.
			scan_limit_case(&services, &router, &room_id).await?;

			Ok(())
		})
	}

	/// `/messages`: originals carry the latest same-sender edit as a full
	/// event bundle; cross-sender and unedited events do not bundle; purge
	/// artifacts (dangling relation-index entries) fall through to the newest
	/// surviving edit.
	async fn messages_cases(services: &Arc<Services>, router: &Router, room_id: &str) -> Result {
		let room = enc(room_id);

		// msg1: original with three same-sender edits (case a).
		let msg1 = send_text(router, &room, ALICE_TOKEN, "m1", "thinking…").await;
		let edit1 = send_edit(router, &room, ALICE_TOKEN, "m2", &msg1, "draft one").await;
		let edit2 = send_edit(router, &room, ALICE_TOKEN, "m3", &msg1, "draft two").await;
		let edit3 = send_edit(router, &room, ALICE_TOKEN, "m4", &msg1, "final answer").await;

		// An edit of an edit must not put a bundle on the edit itself.
		let _edit_of_edit =
			send_edit(router, &room, ALICE_TOKEN, "m5", &edit3, "edit of edit").await;

		// msg2: no edits at all (case d). msg3: only a cross-sender replace,
		// which must not bundle (case c).
		let msg2 = send_text(router, &room, ALICE_TOKEN, "m6", "untouched").await;
		let msg3 = send_text(router, &room, ALICE_TOKEN, "m7", "alice original").await;
		let _bob_edit = send_edit(router, &room, BOB_TOKEN, "m8", &msg3, "bob takeover").await;

		let chunk = messages_chunk(router, &room, ALICE_TOKEN).await;
		let bundle = replace_bundle(find_event(&chunk, &msg1));
		assert_full_edit_bundle(bundle, &edit3, room_id, "final answer");

		// A different requester gets the same bundle, but with the sender's
		// local-echo transaction_id stripped from the bundled edit.
		let bob_chunk = messages_chunk(router, &room, BOB_TOKEN).await;
		let bob_bundle = replace_bundle(find_event(&bob_chunk, &msg1));
		assert_eq!(bob_bundle["event_id"], edit3, "bob sees the same latest edit");
		assert!(
			bob_bundle["unsigned"]
				.get("transaction_id")
				.is_none(),
			"transaction_id must be stripped for non-senders: {bob_bundle}"
		);

		let msg2_event = find_event(&chunk, &msg2);
		assert!(
			msg2_event["unsigned"]
				.get("m.relations")
				.is_none(),
			"unedited event must not grow m.relations: {msg2_event}"
		);

		let msg3_event = find_event(&chunk, &msg3);
		assert!(
			msg3_event["unsigned"]["m.relations"]
				.get("m.replace")
				.is_none(),
			"cross-sender replace must not be bundled: {msg3_event}"
		);

		let edit3_event = find_event(&chunk, &edit3);
		assert!(
			edit3_event["unsigned"]["m.relations"]
				.get("m.replace")
				.is_none(),
			"replacement event must not carry its own m.replace bundle: {edit3_event}"
		);

		// (case b) purge artifacts: delete edit1 (superseded) and edit3 (the
		// newest) the way the purge deletes events, leaving their
		// tofrom_relation index entries dangling. The bundle must skip the
		// dangling newest entry and fall through to the newest surviving
		// edit, edit2.
		for event_id in [&edit1, &edit3] {
			purge_event_rows(services, event_id).await?;
		}

		let chunk = messages_chunk(router, &room, ALICE_TOKEN).await;
		let bundle = replace_bundle(find_event(&chunk, &msg1));
		assert_full_edit_bundle(bundle, &edit2, room_id, "draft two");

		Ok(())
	}

	/// The candidate walk is bounded by the number of relation-index entries
	/// examined, not the PDUs it yields. Dangling entries (a purged edit's
	/// index row, or one a client minted from another room) must consume that
	/// budget too, or a served event with many dangling newer relations would
	/// scan the whole index on every history read. Here an edit sits behind
	/// more than the scan limit of dangling newer entries and is therefore no
	/// longer found — the safe degradation to no bundle. (Before the bound was
	/// moved onto the key walk, the dangling entries yielded nothing, the
	/// output cap never tripped, and the edit was still served.)
	async fn scan_limit_case(services: &Arc<Services>, router: &Router, room_id: &str) -> Result {
		// Keep in sync with REPLACEMENT_SCAN_LIMIT in pdu_metadata.
		const SCAN_LIMIT: usize = 100;

		let room = enc(room_id);

		let msg = send_text(router, &room, ALICE_TOKEN, "s0", "thinking…").await;
		// The edit still exists after this case; only the bounded read gives up.
		let _edit = send_edit(router, &room, ALICE_TOKEN, "s0e", &msg, "buried edit").await;

		// Reactions on the original, all newer than the edit. Send them all
		// first (each builds on the previous as a prev_event), then delete
		// their PDU rows so their relation-index entries dangle. More than the
		// scan limit of them sit between the newest-first walk and the edit.
		let mut reactions = Vec::new();
		for i in 0..=SCAN_LIMIT {
			reactions.push(
				send_typed(
					router,
					&room,
					ALICE_TOKEN,
					"m.reaction",
					&format!("s0r{i}"),
					json!({
						"m.relates_to": {
							"rel_type": "m.annotation",
							"event_id": msg,
							"key": format!("👍{i}"),
						},
					}),
				)
				.await,
			);
		}
		for reaction in &reactions {
			purge_event_rows(services, reaction).await?;
		}

		let chunk = messages_chunk(router, &room, ALICE_TOKEN).await;
		let msg_event = find_event(&chunk, &msg);
		assert!(
			msg_event["unsigned"]
				.get("m.relations")
				.and_then(|relations| relations.get("m.replace"))
				.is_none(),
			"an edit behind more than the scan limit of dangling entries must not be found \
			 (dangling keys must consume the scan budget): {msg_event}"
		);

		Ok(())
	}

	/// `/event/{eventId}` and `/context/{eventId}` both carry the bundle on
	/// the (base) event.
	async fn event_and_context_cases(router: &Router, room_id: &str) {
		let room = enc(room_id);

		let msg = send_text(router, &room, ALICE_TOKEN, "c1", "thinking…").await;
		let _edit1 = send_edit(router, &room, ALICE_TOKEN, "c2", &msg, "draft").await;
		let edit2 = send_edit(router, &room, ALICE_TOKEN, "c3", &msg, "context final").await;

		let event = request(
			router,
			"GET",
			&format!("/_matrix/client/v3/rooms/{room}/event/{}", enc(&msg)),
			ALICE_TOKEN,
			None,
		)
		.await;
		assert_full_edit_bundle(replace_bundle(&event), &edit2, room_id, "context final");

		let context = request(
			router,
			"GET",
			&format!("/_matrix/client/v3/rooms/{room}/context/{}?limit=10", enc(&msg)),
			ALICE_TOKEN,
			None,
		)
		.await;
		assert_full_edit_bundle(
			replace_bundle(&context["event"]),
			&edit2,
			room_id,
			"context final",
		);
	}

	/// `/relations` (plain with `recurse`, and the `m.thread` variant): the
	/// RELATED events being returned — thread replies — carry bundles, not
	/// just the pagination target. `/threads` bundles onto the listed roots.
	async fn relations_and_threads_cases(router: &Router, room_id: &str) {
		let room = enc(room_id);

		let root = send_text(router, &room, ALICE_TOKEN, "r1", "thread root").await;
		let reply = send_message(
			router,
			&room,
			ALICE_TOKEN,
			"r2",
			json!({
				"msgtype": "m.text", "body": "thread reply",
				"m.relates_to": {"rel_type": "m.thread", "event_id": root},
			}),
		)
		.await;
		let reply_edit =
			send_edit(router, &room, ALICE_TOKEN, "r3", &reply, "edited reply").await;

		for uri in [
			format!(
				"/_matrix/client/v1/rooms/{room}/relations/{}?dir=b&limit=100&recurse=true",
				enc(&root),
			),
			format!(
				"/_matrix/client/v1/rooms/{room}/relations/{}/m.thread?dir=b&limit=100",
				enc(&root),
			),
		] {
			let relations = request(router, "GET", &uri, ALICE_TOKEN, None).await;
			let chunk = relations["chunk"]
				.as_array()
				.expect("relations chunk");
			let bundle = replace_bundle(find_event(chunk, &reply));
			assert_full_edit_bundle(bundle, &reply_edit, room_id, "edited reply");
		}

		// /threads: the listed thread roots carry bundles too.
		let root_edit = send_edit(router, &room, ALICE_TOKEN, "r4", &root, "edited root").await;
		let threads = request(
			router,
			"GET",
			&format!("/_matrix/client/v1/rooms/{room}/threads?limit=100"),
			ALICE_TOKEN,
			None,
		)
		.await;
		let chunk = threads["chunk"]
			.as_array()
			.expect("threads chunk");
		let bundle = replace_bundle(find_event(chunk, &root));
		assert_full_edit_bundle(bundle, &root_edit, room_id, "edited root");
	}

	/// `/search`: both the matched originals and the surrounding context
	/// events carry bundles.
	async fn search_case(router: &Router, room_id: &str) {
		let room = enc(room_id);

		// An edited neighbor directly before the search hit, so it lands in
		// the hit's context.events_before.
		let neighbor = send_text(router, &room, ALICE_TOKEN, "q0", "context neighbor").await;
		let neighbor_edit =
			send_edit(router, &room, ALICE_TOKEN, "q0e", &neighbor, "neighbor final").await;

		let msg = send_text(router, &room, ALICE_TOKEN, "q1", "wombat haystack").await;
		let edit = send_edit(router, &room, ALICE_TOKEN, "q2", &msg, "search final").await;

		let results = request(
			router,
			"POST",
			"/_matrix/client/v3/search",
			ALICE_TOKEN,
			Some(json!({
				"search_categories": {"room_events": {
					"search_term": "haystack",
					"event_context": {"before_limit": 3, "after_limit": 3},
				}},
			})),
		)
		.await;
		let results = results["search_categories"]["room_events"]["results"]
			.as_array()
			.expect("search results");
		let hit = results
			.iter()
			.find(|result| result["result"]["event_id"] == msg)
			.unwrap_or_else(|| panic!("search must find {msg}: {results:?}"));
		assert_full_edit_bundle(replace_bundle(&hit["result"]), &edit, room_id, "search final");

		let events_before = hit["context"]["events_before"]
			.as_array()
			.expect("search context events_before");
		let bundle = replace_bundle(find_event(events_before, &neighbor));
		assert_full_edit_bundle(bundle, &neighbor_edit, room_id, "neighbor final");
	}

	/// Encrypted events: `m.relates_to` lives in cleartext beside the
	/// ciphertext, so encrypted edits bundle exactly like plaintext ones.
	async fn encrypted_case(router: &Router, room_id: &str) {
		let room = enc(room_id);

		let msg = send_encrypted(router, &room, "e1", None).await;
		let _edit1 = send_encrypted(router, &room, "e2", Some(&msg)).await;
		let edit2 = send_encrypted(router, &room, "e3", Some(&msg)).await;

		let chunk = messages_chunk(router, &room, ALICE_TOKEN).await;
		let bundle = replace_bundle(find_event(&chunk, &msg));
		assert_eq!(bundle["event_id"], edit2, "latest encrypted edit: {bundle}");
		assert_eq!(bundle["type"], "m.room.encrypted", "bundle type: {bundle}");
		assert_eq!(bundle["room_id"], *room_id, "bundle room_id: {bundle}");
		assert!(bundle["origin_server_ts"].is_u64(), "bundle origin_server_ts: {bundle}");
	}

	/// A redacted newest edit loses its `m.relates_to` content, so the bundle
	/// falls back to the newest surviving unredacted edit.
	async fn redaction_case(router: &Router, room_id: &str) {
		let room = enc(room_id);

		let msg = send_text(router, &room, ALICE_TOKEN, "d1", "thinking…").await;
		let edit1 = send_edit(router, &room, ALICE_TOKEN, "d2", &msg, "kept draft").await;
		let edit2 = send_edit(router, &room, ALICE_TOKEN, "d3", &msg, "redacted final").await;

		request(
			router,
			"PUT",
			&format!("/_matrix/client/v3/rooms/{room}/redact/{}/d4", enc(&edit2)),
			ALICE_TOKEN,
			Some(json!({"reason": "test"})),
		)
		.await;

		let chunk = messages_chunk(router, &room, ALICE_TOKEN).await;
		let bundle = replace_bundle(find_event(&chunk, &msg));
		assert_full_edit_bundle(bundle, &edit1, room_id, "kept draft");
	}

	/// The bundle obeys event visibility: with history_visibility=joined, a
	/// user who left before the edit was sent must not receive the edit's
	/// content bundled onto an original they can see.
	async fn visibility_case(router: &Router) {
		let body = request(
			router,
			"POST",
			"/_matrix/client/v3/createRoom",
			ALICE_TOKEN,
			Some(json!({
				"preset": "private_chat",
				"invite": ["@bob:localhost"],
				"initial_state": [{
					"type": "m.room.history_visibility",
					"state_key": "",
					"content": {"history_visibility": "joined"},
				}],
			})),
		)
		.await;
		let room_id = body["room_id"]
			.as_str()
			.expect("createRoom returns room_id")
			.to_owned();
		let room = enc(&room_id);

		request(
			router,
			"POST",
			&format!("/_matrix/client/v3/rooms/{room}/join"),
			BOB_TOKEN,
			Some(json!({})),
		)
		.await;

		let msg = send_text(router, &room, ALICE_TOKEN, "v1", "pre-leave original").await;

		request(
			router,
			"POST",
			&format!("/_matrix/client/v3/rooms/{room}/leave"),
			BOB_TOKEN,
			Some(json!({})),
		)
		.await;

		let edit = send_edit(router, &room, ALICE_TOKEN, "v2", &msg, "post-leave edit").await;

		// Alice sees the bundle; Bob sees the original he was joined for,
		// but not the edit sent after he left.
		let alice_chunk = messages_chunk(router, &room, ALICE_TOKEN).await;
		let bundle = replace_bundle(find_event(&alice_chunk, &msg));
		assert_eq!(bundle["event_id"], edit, "alice gets the bundle: {bundle}");

		let bob_chunk = messages_chunk(router, &room, BOB_TOKEN).await;
		let bob_msg = find_event(&bob_chunk, &msg);
		assert!(
			bob_msg["unsigned"]["m.relations"]
				.get("m.replace")
				.is_none(),
			"an edit the requester cannot see must not be bundled: {bob_msg}"
		);
	}

	/// Create alice and bob with devices, build the router, create a public
	/// room as alice, and join it as bob.
	async fn setup_room(services: &Arc<Services>) -> Result<(Router, String)> {
		let alice = user_id!("@alice:localhost");
		let bob = user_id!("@bob:localhost");
		for (user, token) in [(alice, ALICE_TOKEN), (bob, BOB_TOKEN)] {
			services
				.users
				.create(user, Some("password"), None)
				.await?;
			services
				.users
				.create_device(user, None, (Some(token), None), None, None, None)
				.await?;
		}

		let (state, _guard) = tuwunel_api::router::state::create(services.clone());
		let router =
			tuwunel_api::router::build(Router::new(), &services.server).with_state(state);

		let body = request(
			&router,
			"POST",
			"/_matrix/client/v3/createRoom",
			ALICE_TOKEN,
			Some(json!({"preset": "public_chat"})),
		)
		.await;
		let room_id = body["room_id"]
			.as_str()
			.expect("createRoom returns room_id")
			.to_owned();

		request(
			&router,
			"POST",
			&format!("/_matrix/client/v3/rooms/{}/join", enc(&room_id)),
			BOB_TOKEN,
			Some(json!({})),
		)
		.await;

		Ok((router, room_id))
	}

	/// Delete an event's PDU rows the way the purge's `delete_event` does,
	/// leaving its `tofrom_relation` index entry dangling.
	async fn purge_event_rows(services: &Arc<Services>, event_id: &str) -> Result {
		let pdu_id = services
			.timeline
			.get_pdu_id(event_id.try_into().expect("valid event id"))
			.await?;
		services.db["pduid_pdu"].remove(pdu_id.as_bytes());
		services.db["eventid_pduid"].remove(event_id.as_bytes());

		Ok(())
	}

	async fn messages_chunk(router: &Router, room: &str, token: &str) -> Vec<JsonValue> {
		let messages = request(
			router,
			"GET",
			&format!("/_matrix/client/v3/rooms/{room}/messages?dir=b&limit=100"),
			token,
			None,
		)
		.await;

		messages["chunk"]
			.as_array()
			.expect("messages chunk")
			.clone()
	}

	/// The bundle must be the full replacement event in client format; the
	/// MindRoom Cinny client ignores bundles without `origin_server_ts` and
	/// hydrates the replacement from this object verbatim.
	fn assert_full_edit_bundle(bundle: &JsonValue, edit_id: &str, room_id: &str, new_body: &str) {
		assert_eq!(
			bundle["event_id"], *edit_id,
			"bundle must be the latest same-sender edit: {bundle}"
		);
		assert_eq!(bundle["sender"], "@alice:localhost", "bundle sender: {bundle}");
		assert_eq!(bundle["room_id"], *room_id, "bundle must carry room_id: {bundle}");
		assert_eq!(bundle["type"], "m.room.message", "bundle type: {bundle}");
		assert!(
			bundle["origin_server_ts"].is_u64(),
			"bundle must carry origin_server_ts: {bundle}"
		);
		assert_eq!(
			bundle["content"]["m.new_content"]["body"], *new_body,
			"bundle content must carry m.new_content: {bundle}"
		);
		assert_eq!(
			bundle["content"]["m.relates_to"]["rel_type"], "m.replace",
			"bundle content must retain m.relates_to: {bundle}"
		);
	}

	fn replace_bundle(event: &JsonValue) -> &JsonValue {
		let bundle = &event["unsigned"]["m.relations"]["m.replace"];
		assert!(bundle.is_object(), "event must carry an m.replace bundle: {event}");
		bundle
	}

	fn find_event<'a>(chunk: &'a [JsonValue], event_id: &str) -> &'a JsonValue {
		chunk
			.iter()
			.find(|event| event["event_id"] == *event_id)
			.unwrap_or_else(|| panic!("event {event_id} not found in chunk"))
	}

	async fn send_text(
		router: &Router,
		room: &str,
		token: &str,
		txn_id: &str,
		body: &str,
	) -> String {
		send_message(
			router,
			room,
			token,
			txn_id,
			json!({
				"msgtype": "m.text", "body": body,
			}),
		)
		.await
	}

	async fn send_message(
		router: &Router,
		room: &str,
		token: &str,
		txn_id: &str,
		content: JsonValue,
	) -> String {
		send_typed(router, room, token, "m.room.message", txn_id, content).await
	}

	/// Send an event of an arbitrary type; returns its event_id.
	async fn send_typed(
		router: &Router,
		room: &str,
		token: &str,
		event_type: &str,
		txn_id: &str,
		content: JsonValue,
	) -> String {
		let body = request(
			router,
			"PUT",
			&format!("/_matrix/client/v3/rooms/{room}/send/{event_type}/{txn_id}"),
			token,
			Some(content),
		)
		.await;

		body["event_id"]
			.as_str()
			.expect("send returns event_id")
			.to_owned()
	}

	async fn send_encrypted(
		router: &Router,
		room: &str,
		txn_id: &str,
		replaces: Option<&str>,
	) -> String {
		let mut content = json!({
			"algorithm": "m.megolm.v1.aes-sha2",
			"ciphertext": "AwgAEnACgAkLmt6qF84IK",
			"device_id": "TESTDEVICE",
			"sender_key": "sender+key",
			"session_id": "session-id",
		});
		if let Some(target) = replaces {
			content["m.relates_to"] = json!({"rel_type": "m.replace", "event_id": target});
		}

		let body = request(
			router,
			"PUT",
			&format!("/_matrix/client/v3/rooms/{room}/send/m.room.encrypted/{txn_id}"),
			ALICE_TOKEN,
			Some(content),
		)
		.await;

		body["event_id"]
			.as_str()
			.expect("send returns event_id")
			.to_owned()
	}

	async fn send_edit(
		router: &Router,
		room: &str,
		token: &str,
		txn_id: &str,
		target: &str,
		new_body: &str,
	) -> String {
		send_message(
			router,
			room,
			token,
			txn_id,
			json!({
				"msgtype": "m.text",
				"body": format!("* {new_body}"),
				"m.new_content": {"msgtype": "m.text", "body": new_body},
				"m.relates_to": {"rel_type": "m.replace", "event_id": target},
			}),
		)
		.await
	}

	async fn request(
		router: &Router,
		method: &str,
		uri: &str,
		token: &str,
		body: Option<JsonValue>,
	) -> JsonValue {
		let request = Request::builder()
			.method(method)
			.uri(uri)
			.header(header::AUTHORIZATION, format!("Bearer {token}"))
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
		assert_eq!(
			status,
			StatusCode::OK,
			"{method} {uri} should succeed: {}",
			String::from_utf8_lossy(&bytes),
		);

		serde_json::from_slice(&bytes).expect("JSON response body")
	}

	/// Percent-encode a room/event ID for use as a URI path segment.
	fn enc(id: &str) -> String {
		id.bytes()
			.map(|byte| match byte {
				| b'$' => "%24".to_owned(),
				| b'!' => "%21".to_owned(),
				| b':' => "%3A".to_owned(),
				| b'+' => "%2B".to_owned(),
				| b'/' => "%2F".to_owned(),
				| b'=' => "%3D".to_owned(),
				| b'?' => "%3F".to_owned(),
				| b'#' => "%23".to_owned(),
				| other => char::from(other).to_string(),
			})
			.collect()
	}
}
