//! Fresh-device thread previews travel with each root through shared bundling.
mod support;

#[cfg(test)]
mod tests {

	use std::{collections::BTreeMap, sync::Arc, time::Instant};

	use axum::{Extension, Router, body::Body};
	use serde_json::{Value, json};
	use tower::ServiceExt;
	use tuwunel_core::{
		Result,
		http::{Request, StatusCode, header},
		matrix::{PduCount, PduId, RawPduId},
		ruma::{EventId, user_id},
	};
	use tuwunel_service::Services;

	use super::support::Harness;

	const OWNER: &str = "preview-owner-token-01234567890123456789";
	const MEMBER: &str = "preview-member-token-0123456789012345678";

	#[test]
	fn thread_previews() -> Result {
		Harness::new("mindroom_thread_previews", [])?.with_services(run)
	}

	struct ThreadCase {
		root: String,
		reply: String,
		first_title: String,
		edit: String,
		newest_title: String,
	}

	async fn run(services: Arc<Services>) -> Result {
		for (user, token) in
			[(user_id!("@owner:localhost"), OWNER), (user_id!("@member:localhost"), MEMBER)]
		{
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
		let router = tuwunel_api::router::build(Router::new(), &services.server)
			.with_state(state)
			.layer(Extension(Arc::new(tokio::sync::Notify::new())));
		let room = request(
			&router,
			"POST",
			"/_matrix/client/v3/createRoom",
			OWNER,
			json!({"preset":"public_chat"}),
		)
		.await["room_id"]
			.as_str()
			.expect("valid test fixture")
			.to_owned();
		let base = format!("/_matrix/client/v3/rooms/{}", enc(&room));
		request(&router, "POST", &format!("{base}/join"), MEMBER, json!({})).await;

		let case = fresh_device(&router, &base).await;
		let outer = endpoint_bundles(&services, &router, &base, &room, &case).await;
		redactions_and_rebuild(&services, &router, &base, &case, &outer).await?;
		ignored_users(&router, &base).await;
		bounded_seeks(&router, &base).await;
		history_visibility(&router, &base, &case).await;
		authorship(&router, &base).await;
		backfilled_root(&services, &router, &base).await?;
		large_room(&services, &router, &base).await;
		marker_and_encryption(&router, &base, &case.root).await;
		Ok(())
	}

	async fn fresh_device(router: &Router, base: &str) -> ThreadCase {
		let root =
			send(router, base, OWNER, "root", json!({"msgtype":"m.text","body":"question"}))
				.await;
		let reply = send(router, base, OWNER, "reply", thread(&root, "m.notice", "answer")).await;
		let first_title = send(router, base, OWNER, "title", title(&root, "topic")).await;
		let served = event(router, base, MEMBER, &root).await;
		let summary = &served["unsigned"]["m.relations"]["m.thread"];
		assert_eq!(
			summary["latest_event"]["event_id"], first_title,
			"metadata remains the standard latest event"
		);
		assert_eq!(summary["count"], 2, "title notices count as direct thread events");
		assert_eq!(
			summary["current_user_participated"], false,
			"a reader who never sent a thread event has not participated"
		);
		assert_eq!(
			summary["io.mindroom.title"]["event_id"], first_title,
			"fresh device needs title: {summary}"
		);
		assert_eq!(
			summary["io.mindroom.latest_reply"]["event_id"], reply,
			"notice replies remain conversational"
		);

		let edit = send(router, base, OWNER, "edit", json!({"msgtype":"m.notice","body":"* final answer","m.new_content":{"msgtype":"m.notice","body":"final answer"},"m.relates_to":{"rel_type":"m.replace","event_id":reply}})).await;
		let newest_title =
			send(router, base, OWNER, "new-title", title(&root, "new topic")).await;
		// A marked ordinary member's event remains a standard thread event,
		// but cannot overwrite the trusted title or become a display reply.
		let spoof = send(router, base, MEMBER, "spoof", title(&root, "forged")).await;
		let served = event(router, base, MEMBER, &root).await;
		let summary = &served["unsigned"]["m.relations"]["m.thread"];
		assert_eq!(
			summary["latest_event"]["event_id"], spoof,
			"unauthorized metadata remains the standard latest event"
		);
		assert_eq!(summary["count"], 4, "edits do not increment the direct thread count");
		assert_eq!(
			summary["current_user_participated"], true,
			"metadata sends still count as participation"
		);
		assert_preview(summary, &newest_title, &reply, &edit);
		assert!(
			summary["io.mindroom.latest_reply"]["unsigned"]
				.get("transaction_id")
				.is_none(),
			"another sender transaction ID must not leak"
		);
		ThreadCase {
			root,
			reply,
			first_title,
			edit,
			newest_title,
		}
	}

	async fn endpoint_bundles(
		services: &Services,
		router: &Router,
		base: &str,
		room: &str,
		case: &ThreadCase,
	) -> String {
		let root = case.root.as_str();
		let reply = case.reply.as_str();
		let edit = case.edit.as_str();
		let newest_title = case.newest_title.as_str();
		for path in [
			format!("{base}/messages?dir=b&limit=100"),
			format!("{base}/context/{}?limit=10", enc(root)),
			format!("{}/threads?limit=100", base.replace("/v3/", "/v1/")),
			"/_matrix/client/v3/sync?timeout=0".to_owned(),
			"/_matrix/client/unstable/org.matrix.simplified_msc3575/sync".to_owned(),
		] {
			let response = if path.contains("msc3575") {
				request(
					router,
					"POST",
					&path,
					MEMBER,
					json!({"room_subscriptions":{(room):{"timeline_limit":100,"required_state":[]}}}),
				)
				.await
			} else {
				request(router, "GET", &path, MEMBER, Value::Null).await
			};
			let found = find_event(&response, root)
				.unwrap_or_else(|| panic!("root missing in {path}: {response}"));
			assert_preview(
				&found["unsigned"]["m.relations"]["m.thread"],
				newest_title,
				reply,
				edit,
			);
		}
		// Local sends forbid nested threads. Link the existing root into a test
		// relation index to exercise /relations' root-bundling response path.
		let target = send(
			router,
			base,
			OWNER,
			"relation-target",
			json!({"msgtype":"m.text","body":"target"}),
		)
		.await;
		let root_count = services
			.timeline
			.get_pdu_count(root.try_into().expect("event ID"))
			.await
			.expect("stored root");
		let target_count = services
			.timeline
			.get_pdu_count(target.as_str().try_into().expect("event ID"))
			.await
			.expect("stored target");
		services
			.pdu_metadata
			.add_relation(root_count, target_count);
		let relations = request(
			router,
			"GET",
			&format!("{}/relations/{}?dir=b", base.replace("/v3/", "/v1/"), enc(&target)),
			MEMBER,
			Value::Null,
		)
		.await;
		let served = find_event(&relations, root).expect("relations child root");
		assert_preview(&served["unsigned"]["m.relations"]["m.thread"], newest_title, reply, edit);
		send(router, base, OWNER, "outer", thread(root, "m.text", "newer reply")).await
	}

	async fn redactions_and_rebuild(
		services: &Services,
		router: &Router,
		base: &str,
		case: &ThreadCase,
		outer: &str,
	) -> Result {
		let root = case.root.as_str();
		let reply = case.reply.as_str();
		let first_title = case.first_title.as_str();
		let edit = case.edit.as_str();
		let newest_title = case.newest_title.as_str();
		// Redacting selected metadata/replies falls back to live predecessors.
		redact(router, base, newest_title, "redact-title").await;
		let served = event(router, base, MEMBER, root).await;
		assert_eq!(
			served["unsigned"]["m.relations"]["m.thread"]["io.mindroom.title"]["event_id"],
			first_title,
			"a redacted title must fall back to the prior title"
		);
		redact(router, base, outer, "redact-reply").await;
		let served = event(router, base, MEMBER, root).await;
		assert_preview(&served["unsigned"]["m.relations"]["m.thread"], first_title, reply, edit);
		redact(router, base, edit, "redact-edit").await;
		let served = event(router, base, MEMBER, root).await;
		assert!(
			served["unsigned"]["m.relations"]["m.thread"]["io.mindroom.latest_reply"]["unsigned"]
				["m.relations"]
				.get("m.replace")
				.is_none(),
			"redacted edits must not be bundled"
		);

		// Older binaries never wrote preview rows. Reads must use native relations,
		// even when the typed relation index has no preview data after rollback.
		services.db["relatesto_typed"].clear().await;
		let unindexed = event(router, base, MEMBER, root).await;
		assert_eq!(
			unindexed["unsigned"]["m.relations"]["m.thread"]["io.mindroom.title"]["event_id"],
			first_title,
			"previews must not depend on derived index rows after rollback"
		);
		services
			.pdu_metadata
			.rebuild_typed_relations()
			.await?;
		let served = event(router, base, MEMBER, root).await;
		assert_eq!(
			served["unsigned"]["m.relations"]["m.thread"]["io.mindroom.title"]["event_id"],
			first_title,
			"native edit-index rebuild must not change thread titles"
		);

		Ok(())
	}

	async fn ignored_users(router: &Router, base: &str) {
		// Ignore changes take effect on the next read, without a shared cache.
		let own_root =
			send(router, base, MEMBER, "own-root", json!({"msgtype":"m.text","body":"own root"}))
				.await;
		let own_reply =
			send(router, base, MEMBER, "own-reply", thread(&own_root, "m.text", "own reply"))
				.await;
		send(
			router,
			base,
			OWNER,
			"ignored-reply",
			thread(&own_root, "m.text", "ignored reply"),
		)
		.await;
		send(router, base, OWNER, "ignored-title", title(&own_root, "ignored title")).await;
		request(
			router,
			"PUT",
			"/_matrix/client/v3/user/%40member%3Alocalhost/account_data/m.ignored_user_list",
			MEMBER,
			json!({"ignored_users":{"@owner:localhost":{}}}),
		)
		.await;
		let served = event(router, base, MEMBER, &own_root).await;
		let summary = &served["unsigned"]["m.relations"]["m.thread"];
		assert!(
			summary.get("io.mindroom.title").is_none(),
			"ignored title authors must be omitted"
		);
		assert_eq!(
			summary["io.mindroom.latest_reply"]["event_id"], own_reply,
			"an ignored latest reply must fall back to an unignored reply"
		);
		request(
			router,
			"PUT",
			"/_matrix/client/v3/user/%40member%3Alocalhost/account_data/m.ignored_user_list",
			MEMBER,
			json!({"ignored_users":{}}),
		)
		.await;
	}

	async fn bounded_seeks(router: &Router, base: &str) {
		let root = send(
			router,
			base,
			OWNER,
			"bounded-root",
			json!({"msgtype":"m.text","body":"bounded lookup"}),
		)
		.await;
		send(router, base, OWNER, "bounded-reply", thread(&root, "m.text", "answer")).await;
		send(router, base, OWNER, "bounded-title", title(&root, "trusted")).await;
		for index in 0..50 {
			send(
				router,
				base,
				MEMBER,
				&format!("bounded-forgery-{index}"),
				title(&root, "forged"),
			)
			.await;
		}
		let served = event(router, base, OWNER, &root).await;
		let summary = &served["unsigned"]["m.relations"]["m.thread"];
		assert!(
			summary.get("io.mindroom.title").is_none(),
			"exhausted seek must stop before the older authorized title"
		);
		assert!(
			summary.get("io.mindroom.latest_reply").is_none(),
			"a reply outside the shared fifty-row bound is omitted"
		);
	}

	async fn history_visibility(router: &Router, base: &str, case: &ThreadCase) {
		let root = case.root.as_str();
		let first_title = case.first_title.as_str();
		// A current member cannot read a title sent while they were absent
		// under joined history, even when the standard latest event is visible.
		request(
			router,
			"PUT",
			&format!("{base}/state/m.room.history_visibility"),
			OWNER,
			json!({"history_visibility":"joined"}),
		)
		.await;
		let edit_root = send(
			router,
			base,
			OWNER,
			"edit-root",
			json!({"msgtype":"m.text","body":"edit visibility"}),
		)
		.await;
		let edit_reply = send(
			router,
			base,
			OWNER,
			"edit-reply",
			thread(&edit_root, "m.text", "visible original"),
		)
		.await;
		let visible_edit = send(router, base, OWNER, "visible-edit", json!({"msgtype":"m.text","body":"* visible revision","m.new_content":{"msgtype":"m.text","body":"visible revision"},"m.relates_to":{"rel_type":"m.replace","event_id":edit_reply}})).await;
		request(router, "POST", &format!("{base}/leave"), MEMBER, json!({})).await;
		send(router, base, OWNER, "hidden-title", title(root, "hidden topic")).await;
		send(router,base,OWNER,"hidden-edit",json!({"msgtype":"m.text","body":"* hidden","m.new_content":{"msgtype":"m.text","body":"hidden"},"m.relates_to":{"rel_type":"m.replace","event_id":edit_reply}})).await;
		request(router, "POST", &format!("{base}/join"), MEMBER, json!({})).await;
		send(router, base, OWNER, "edit-title", title(&edit_root, "visible title")).await;
		let served = event(router, base, MEMBER, &edit_root).await;
		let preview = &served["unsigned"]["m.relations"]["m.thread"]["io.mindroom.latest_reply"];
		assert_eq!(
			preview["event_id"], edit_reply,
			"a visible parent remains eligible when its edit is hidden"
		);
		assert_eq!(
			preview["unsigned"]["m.relations"]["m.replace"]["event_id"], visible_edit,
			"hidden latest edit must retain earlier visible edit"
		);
		let visible_reply =
			send(router, base, MEMBER, "visible-reply", thread(root, "m.text", "visible reply"))
				.await;
		let served = event(router, base, MEMBER, root).await;
		let summary = &served["unsigned"]["m.relations"]["m.thread"];
		assert_eq!(
			summary["latest_event"]["event_id"], visible_reply,
			"the standard latest event remains the visible new reply"
		);
		assert_eq!(
			summary["io.mindroom.title"]["event_id"], first_title,
			"hidden latest title must fall back"
		);
		assert_eq!(
			summary["io.mindroom.latest_reply"]["event_id"], visible_reply,
			"a history gap must not hide subsequent replies"
		);
	}

	async fn authorship(router: &Router, base: &str) {
		let forgery_root = send(
			router,
			base,
			OWNER,
			"forgery-root",
			json!({"msgtype":"m.text","body":"authorship"}),
		)
		.await;
		let root = forgery_root.as_str();
		send(router, base, MEMBER, "forgery", title(root, "unauthorized")).await;
		// A later promotion cannot authorize a title sent at power zero.
		request(
			router,
			"PUT",
			&format!("{base}/state/m.room.power_levels"),
			OWNER,
			json!({"users":{"@member:localhost":50}}),
		)
		.await;
		let served = event(router, base, OWNER, root).await;
		assert!(
			served["unsigned"]["m.relations"]["m.thread"]
				.get("io.mindroom.title")
				.is_none(),
			"promotion must not authorize an older forgery: {served}"
		);
		let approved_title =
			send(router, base, MEMBER, "approved-title", title(root, "approved topic")).await;
		let approved_edit = send(router, base, MEMBER, "approved-title-edit", json!({"msgtype":"m.notice","body":"* approved revision","m.new_content":{"msgtype":"m.notice","body":"approved revision","io.mindroom.thread_summary":{}},"m.relates_to":{"rel_type":"m.replace","event_id":approved_title}})).await;
		request(
			router,
			"PUT",
			&format!("{base}/state/m.room.power_levels"),
			OWNER,
			json!({"users":{"@member:localhost":0}}),
		)
		.await;
		let served = event(router, base, OWNER, root).await;
		assert_eq!(
			served["unsigned"]["m.relations"]["m.thread"]["io.mindroom.title"]["event_id"],
			approved_title,
			"later demotion does not rewrite event-time authorization"
		);
		send(router, base, MEMBER, "demoted-title-edit", json!({"msgtype":"m.notice","body":"* unauthorized edit","m.new_content":{"msgtype":"m.notice","body":"unauthorized edit","io.mindroom.thread_summary":{}},"m.relates_to":{"rel_type":"m.replace","event_id":approved_title}})).await;
		let served = event(router, base, OWNER, root).await;
		assert_eq!(
			served["unsigned"]["m.relations"]["m.thread"]["io.mindroom.title"]["unsigned"]
				["m.relations"]["m.replace"]["event_id"],
			approved_edit,
			"rejected post-demotion edit must retain earlier authorized edit"
		);
	}

	async fn backfilled_root(services: &Services, router: &Router, base: &str) -> Result {
		let root = send(
			router,
			base,
			OWNER,
			"backfilled-root",
			json!({"msgtype":"m.text","body":"historical root"}),
		)
		.await;
		relocate_to_backfill(services, &root, -1).await?;
		let reply = send(
			router,
			base,
			OWNER,
			"backfilled-root-reply",
			thread(&root, "m.text", "live answer"),
		)
		.await;
		let topic =
			send(router, base, OWNER, "backfilled-root-title", title(&root, "historical topic"))
				.await;
		for rebuild in [false, true] {
			if rebuild {
				services
					.pdu_metadata
					.rebuild_typed_relations()
					.await?;
			}
			let served = event(router, base, MEMBER, &root).await;
			let summary = &served["unsigned"]["m.relations"]["m.thread"];
			assert_eq!(summary["count"], 2, "backfilled root retains the standard count");
			assert_eq!(
				summary["latest_event"]["event_id"], topic,
				"backfilled root retains the standard latest event"
			);
			assert_eq!(
				summary["io.mindroom.title"]["event_id"], topic,
				"backfilled root needs a title, rebuild={rebuild}"
			);
			assert_eq!(
				summary["io.mindroom.latest_reply"]["event_id"], reply,
				"backfilled root needs a reply, rebuild={rebuild}"
			);
		}
		Ok(())
	}

	async fn large_room(services: &Services, router: &Router, base: &str) {
		let mut expected = BTreeMap::new();
		for index in 0..505 {
			let root = send(
				router,
				base,
				OWNER,
				&format!("scale-root-{index}"),
				json!({"msgtype":"m.text","body":"question"}),
			)
			.await;
			let reply = send(
				router,
				base,
				OWNER,
				&format!("scale-reply-{index}"),
				thread(&root, "m.text", "answer"),
			)
			.await;
			let topic =
				send(router, base, OWNER, &format!("scale-title-{index}"), title(&root, "topic"))
					.await;
			expected.insert(root, (reply, topic));
		}
		// Model writes received on an older binary: native relations exist,
		// while derived preview rows are absent at re-upgrade.
		services.db["relatesto_typed"].clear().await;
		let endpoint = format!("{}/threads?limit=100", base.replace("/v3/", "/v1/"));
		let mut path = endpoint.clone();
		let start = Instant::now();
		let mut pages = 0_usize;
		for _ in 0..6 {
			let response = request(router, "GET", &path, MEMBER, Value::Null).await;
			pages = pages.saturating_add(1);
			for root in response["chunk"].as_array().expect("thread list") {
				let id = root["event_id"].as_str().expect("thread root ID");
				if let Some((reply, topic)) = expected.remove(id) {
					let summary = &root["unsigned"]["m.relations"]["m.thread"];
					assert_eq!(
						summary["latest_event"]["event_id"], topic,
						"standard latest event on {id}"
					);
					assert_eq!(summary["count"], 2, "standard count on {id}");
					assert_eq!(
						summary["io.mindroom.title"]["event_id"], topic,
						"title without derived rows on {id}"
					);
					assert_eq!(
						summary["io.mindroom.latest_reply"]["event_id"], reply,
						"reply without derived rows on {id}"
					);
				}
			}
			if expected.is_empty() {
				break;
			}
			let next = response["next_batch"]
				.as_str()
				.expect("more threads remain");
			path = format!("{endpoint}&from={}", enc(next));
		}
		assert!(expected.is_empty(), "every one of 505 threads must carry previews");
		eprintln!(
			"PREVIEW_SCALE roots=505 pages={pages} elapsed_ms={} candidate_row_limit=50",
			start.elapsed().as_millis()
		);
	}

	/// Preserve the event and its authorization state at the storage position
	/// used by real backfill, without needing an external federation fixture.
	async fn relocate_to_backfill(
		services: &Services,
		event_id: &str,
		count: i64,
	) -> Result<RawPduId> {
		let event_id: &EventId = event_id.try_into()?;
		let old_id = services.timeline.get_pdu_id(event_id).await?;
		let old: PduId = old_id.into();
		let back_id: RawPduId = PduId {
			shortroomid: old.shortroomid,
			count: PduCount::Backfilled(count),
		}
		.into();
		let stored = services.db["pduid_pdu"]
			.get(old_id.as_bytes())
			.await?
			.to_vec();
		services.db["pduid_pdu"].insert(back_id.as_bytes(), &stored);
		services.db["eventid_pduid"].insert(event_id.as_bytes(), back_id.as_bytes());
		services.db["pduid_pdu"].remove(old_id.as_bytes());
		Ok(back_id)
	}

	async fn marker_and_encryption(router: &Router, base: &str, root: &str) {
		// The marker must be an object, and ordinary notices remain replies.
		let mut malformed = thread(root, "m.notice", "ordinary notice");
		malformed["io.mindroom.thread_summary"] = json!(true);
		let notice = send(router, base, OWNER, "malformed-marker", malformed).await;
		let served = event(router, base, OWNER, root).await;
		assert_eq!(
			served["unsigned"]["m.relations"]["m.thread"]["io.mindroom.latest_reply"]["event_id"],
			notice,
			"a non-object marker remains conversational"
		);

		let title_root = send(
			router,
			base,
			OWNER,
			"title-root",
			json!({"msgtype":"m.text","body":"title only"}),
		)
		.await;
		let only_title =
			send(router, base, OWNER, "only-title", title(&title_root, "only topic")).await;
		let served = event(router, base, OWNER, &title_root).await;
		let summary = &served["unsigned"]["m.relations"]["m.thread"];
		assert!(summary["io.mindroom.title"].is_object(), "title-only threads carry a title");
		assert!(
			summary.get("io.mindroom.latest_reply").is_none(),
			"title-only threads omit the conversational preview"
		);
		// Redacting a root retains its live children's standard thread summary.
		redact(router, base, &title_root, "redact-root").await;
		let served = event(router, base, OWNER, &title_root).await;
		let summary = &served["unsigned"]["m.relations"]["m.thread"];
		assert_eq!(
			summary["latest_event"]["event_id"], only_title,
			"root redaction must preserve the standard latest event"
		);
		assert_eq!(
			summary["io.mindroom.title"]["event_id"], only_title,
			"a redacted root must retain its live title"
		);

		// Encryption suppresses both fields, even for prior plaintext history.
		request(
			router,
			"PUT",
			&format!("{base}/state/m.room.encryption"),
			OWNER,
			json!({"algorithm":"m.megolm.v1.aes-sha2"}),
		)
		.await;
		let served = event(router, base, MEMBER, root).await;
		let summary = &served["unsigned"]["m.relations"]["m.thread"];
		assert!(
			summary.get("io.mindroom.title").is_none(),
			"encrypted rooms omit title metadata"
		);
		assert!(
			summary.get("io.mindroom.latest_reply").is_none(),
			"encrypted rooms omit conversational previews"
		);
	}

	fn assert_preview(summary: &Value, title: &str, reply: &str, edit: &str) {
		assert_eq!(summary["io.mindroom.title"]["event_id"], title, "{summary}");
		let reply_event = &summary["io.mindroom.latest_reply"];
		assert_eq!(reply_event["event_id"], reply, "{summary}");
		assert_eq!(
			reply_event["type"], "m.room.message",
			"a preview must include its client event type"
		);
		assert!(
			reply_event["origin_server_ts"].is_u64(),
			"a preview must include its client event timestamp"
		);
		assert_eq!(
			reply_event["unsigned"]["m.relations"]["m.replace"]["event_id"], edit,
			"{reply_event}"
		);
	}
	fn find_event<'a>(value: &'a Value, id: &str) -> Option<&'a Value> {
		match value {
			| Value::Object(map)
				if map.contains_key("type") && map.get("event_id").is_some_and(|v| v == id) =>
				Some(value),
			| Value::Object(map) => map.values().find_map(|v| find_event(v, id)),
			| Value::Array(items) => items.iter().find_map(|v| find_event(v, id)),
			| _ => None,
		}
	}
	async fn redact(router: &Router, base: &str, id: &str, txn: &str) {
		request(router, "PUT", &format!("{base}/redact/{}/{txn}", enc(id)), OWNER, json!({}))
			.await;
	}

	fn thread(root: &str, kind: &str, body: &str) -> Value {
		json!({"msgtype":kind,"body":body,"m.relates_to":{"rel_type":"m.thread","event_id":root}})
	}
	fn title(root: &str, body: &str) -> Value {
		let mut content = thread(root, "m.notice", body);
		content["io.mindroom.thread_summary"] = json!({"title":body});
		content
	}
	async fn send(router: &Router, base: &str, token: &str, txn: &str, content: Value) -> String {
		request(router, "PUT", &format!("{base}/send/m.room.message/{txn}"), token, content).await
			["event_id"]
			.as_str()
			.expect("valid test fixture")
			.to_owned()
	}
	async fn event(router: &Router, base: &str, token: &str, id: &str) -> Value {
		request(router, "GET", &format!("{base}/event/{}", enc(id)), token, Value::Null).await
	}
	async fn request(
		router: &Router,
		method: &str,
		uri: &str,
		token: &str,
		body: Value,
	) -> Value {
		let request = Request::builder()
			.method(method)
			.uri(uri)
			.header(header::AUTHORIZATION, format!("Bearer {token}"))
			.header(header::CONTENT_TYPE, "application/json")
			.header("X-Forwarded-For", "127.0.0.1")
			.body(if body.is_null() {
				Body::empty()
			} else {
				Body::from(body.to_string())
			})
			.expect("valid test fixture");
		let response = router
			.clone()
			.oneshot(request)
			.await
			.expect("valid test fixture");
		let status = response.status();
		let bytes = axum::body::to_bytes(response.into_body(), 8 << 20)
			.await
			.expect("valid test fixture");
		let body: Value = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
			panic!("{method} {uri}: {status}: {error}: {}", String::from_utf8_lossy(&bytes))
		});
		assert_eq!(status, StatusCode::OK, "{method} {uri}: {body}");
		body
	}
	fn enc(value: &str) -> String {
		url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
	}
}
