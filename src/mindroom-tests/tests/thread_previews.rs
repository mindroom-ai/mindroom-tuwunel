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
		redactions(&router, &base, &case, &outer).await;
		ignored_users(&router, &base).await;
		bounded_seeks(&router, &base).await;
		history_visibility(&router, &base, &case).await;
		title_rule(&router).await;
		backfilled_root(&services, &router, &base).await?;
		large_room(&router, &base).await;
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

		let edit = send(router, base, OWNER, "edit", json!({"msgtype":"m.notice","body":"* final answer","m.new_content":{"msgtype":"m.notice","body":"final answer","io.mindroom.thread_summary":{"title":"must not promote this reply"}},"m.relates_to":{"rel_type":"m.replace","event_id":reply}})).await;
		// A member who never posted in the thread renames it, as both clients
		// write a manual title.
		let mut rename = title(&root, "renamed topic");
		rename["io.mindroom.thread_summary"] =
			json!({"version":1,"summary":"renamed topic","model":"manual","pinned":true});
		let newest_title = send(router, base, MEMBER, "rename", rename).await;
		let served = event(router, base, MEMBER, &root).await;
		let summary = &served["unsigned"]["m.relations"]["m.thread"];
		assert_eq!(
			summary["latest_event"]["event_id"], newest_title,
			"the rename remains the standard latest event"
		);
		assert_eq!(summary["count"], 3, "edits do not increment the direct thread count");
		assert_eq!(
			summary["current_user_participated"], true,
			"metadata sends still count as participation"
		);
		assert_preview(summary, &newest_title, &reply, &edit);
		assert!(
			summary["io.mindroom.latest_reply"]["unsigned"]["m.relations"]["m.replace"]
				["content"]["m.new_content"]["io.mindroom.thread_summary"]
				.is_object(),
			"a reply edit retains its marker as payload without becoming a title"
		);
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

	async fn redactions(router: &Router, base: &str, case: &ThreadCase, outer: &str) {
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
		send(router, base, OWNER, "bounded-title", title(&root, "old topic")).await;
		let mut newest = String::new();
		for index in 0..50 {
			newest = send(
				router,
				base,
				MEMBER,
				&format!("bounded-reply-{index}"),
				thread(&root, "m.text", "more"),
			)
			.await;
		}
		let start = Instant::now();
		let served = event(router, base, OWNER, &root).await;
		let summary = &served["unsigned"]["m.relations"]["m.thread"];
		assert!(
			summary.get("io.mindroom.title").is_none(),
			"a title outside the fifty-row window is omitted"
		);
		assert_eq!(
			summary["io.mindroom.latest_reply"]["event_id"], newest,
			"the newest reply is served"
		);
		eprintln!("PREVIEW_BOUND rows=50 elapsed_ms={}", start.elapsed().as_millis());
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
		let visible_title_edit = send(router, base, OWNER, "visible-title-edit", json!({"msgtype":"m.notice","body":"* visible topic","m.new_content":{"msgtype":"m.notice","body":"visible topic","io.mindroom.thread_summary":{}},"m.relates_to":{"rel_type":"m.replace","event_id":first_title}})).await;
		request(router, "POST", &format!("{base}/leave"), MEMBER, json!({})).await;
		send(router, base, OWNER, "hidden-title", title(root, "hidden topic")).await;
		send(router,base,OWNER,"hidden-edit",json!({"msgtype":"m.text","body":"* hidden","m.new_content":{"msgtype":"m.text","body":"hidden"},"m.relates_to":{"rel_type":"m.replace","event_id":edit_reply}})).await;
		send(router, base, OWNER, "hidden-title-edit", json!({"msgtype":"m.notice","body":"* hidden topic","m.new_content":{"msgtype":"m.notice","body":"hidden topic","io.mindroom.thread_summary":{}},"m.relates_to":{"rel_type":"m.replace","event_id":first_title}})).await;
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
			send(router, base, OWNER, "visible-reply", thread(root, "m.text", "visible reply"))
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
			summary["io.mindroom.title"]["unsigned"]["m.relations"]["m.replace"]["event_id"],
			visible_title_edit,
			"a hidden newest title edit must retain the earlier visible edit"
		);
		assert_eq!(
			summary["io.mindroom.latest_reply"]["event_id"], visible_reply,
			"a history gap must not hide subsequent replies"
		);
	}

	async fn title_rule(router: &Router) {
		let room = request(
			router,
			"POST",
			"/_matrix/client/v3/createRoom",
			OWNER,
			json!({"preset":"private_chat","is_direct":true,"invite":["@member:localhost"]}),
		)
		.await;
		let base = format!(
			"/_matrix/client/v3/rooms/{}",
			enc(room["room_id"].as_str().expect("DM room"))
		);
		let base = base.as_str();
		request(router, "POST", &format!("{base}/join"), MEMBER, json!({})).await;
		let root =
			send(router, base, OWNER, "rule-root", json!({"msgtype":"m.text","body":"question"}))
				.await;
		send(router, base, OWNER, "rule-reply", thread(&root, "m.text", "answer")).await;
		send(router, base, OWNER, "rule-title", title(&root, "first topic")).await;
		// An agent retitling a thread it never replied in, at power level zero.
		let retitle = send(router, base, MEMBER, "rule-retitle", title(&root, "renamed")).await;
		let served = event(router, base, OWNER, &root).await;
		assert_eq!(
			served["unsigned"]["m.relations"]["m.thread"]["io.mindroom.title"]["event_id"],
			retitle,
			"any sender's marked notice is the newest title"
		);
		// Only an `m.notice` is a title, as the clients read it.
		let mut text = thread(&root, "m.text", "marked text");
		text["io.mindroom.thread_summary"] = json!({"summary":"not a title"});
		let text = send(router, base, MEMBER, "rule-marked-text", text).await;
		let edit = send(router, base, MEMBER, "rule-retitle-edit", json!({"msgtype":"m.notice","body":"* revised","m.new_content":{"msgtype":"m.notice","body":"revised","io.mindroom.thread_summary":{}},"m.relates_to":{"rel_type":"m.replace","event_id":retitle}})).await;
		let served = event(router, base, OWNER, &root).await;
		let summary = &served["unsigned"]["m.relations"]["m.thread"];
		assert_eq!(summary["io.mindroom.title"]["event_id"], retitle, "{summary}");
		assert_eq!(
			summary["io.mindroom.latest_reply"]["event_id"], text,
			"a marked m.text is a conversational reply"
		);
		assert_eq!(
			summary["io.mindroom.title"]["unsigned"]["m.relations"]["m.replace"]["event_id"],
			edit,
			"a title's sender can edit it"
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
		let served = event(router, base, MEMBER, &root).await;
		let summary = &served["unsigned"]["m.relations"]["m.thread"];
		assert_eq!(summary["count"], 2, "backfilled root retains the standard count");
		assert_eq!(
			summary["latest_event"]["event_id"], topic,
			"backfilled root retains the standard latest event"
		);
		assert_eq!(summary["io.mindroom.title"]["event_id"], topic, "backfilled root title");
		assert_eq!(
			summary["io.mindroom.latest_reply"]["event_id"], reply,
			"backfilled root reply"
		);
		Ok(())
	}

	async fn large_room(router: &Router, base: &str) {
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
					assert_eq!(summary["io.mindroom.title"]["event_id"], topic, "title on {id}");
					assert_eq!(
						summary["io.mindroom.latest_reply"]["event_id"], reply,
						"reply on {id}"
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
		// The clients take a notice with any truthy marker, such as the older
		// `true`, as a title, so it never becomes the conversational preview.
		let reply = send(router, base, OWNER, "marker-reply", thread(root, "m.text", "answer")).await;
		for (txn, marker) in [
			("true-marker", json!(true)),
			("number-marker", json!(1)),
			("string-marker", json!("topic")),
			("array-marker", json!([])),
		] {
			let mut notice = thread(root, "m.notice", "topic");
			notice["io.mindroom.thread_summary"] = marker;
			let title = send(router, base, OWNER, txn, notice).await;
			let served = event(router, base, OWNER, root).await;
			let summary = &served["unsigned"]["m.relations"]["m.thread"];
			assert_eq!(summary["io.mindroom.title"]["event_id"], title, "{txn}: {summary}");
			assert_eq!(summary["io.mindroom.latest_reply"]["event_id"], reply, "{txn}: {summary}");
		}
		// A falsy marker leaves an ordinary notice conversational.
		for (txn, marker) in [
			("false-marker", json!(false)),
			("zero-marker", json!(0)),
			("empty-marker", json!("")),
			("null-marker", Value::Null),
		] {
			let mut notice = thread(root, "m.notice", "ordinary notice");
			notice["io.mindroom.thread_summary"] = marker;
			let notice = send(router, base, OWNER, txn, notice).await;
			let served = event(router, base, OWNER, root).await;
			let summary = &served["unsigned"]["m.relations"]["m.thread"];
			assert_eq!(summary["io.mindroom.latest_reply"]["event_id"], notice, "{txn}: {summary}");
		}

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
		assert_eq!(
			summary["io.mindroom.title"]["event_id"], only_title,
			"a title needs no reply"
		);
		assert!(
			summary.get("io.mindroom.latest_reply").is_none(),
			"title-only threads omit the conversational preview"
		);
		send(
			router,
			base,
			OWNER,
			"title-root-reply",
			thread(&title_root, "m.text", "later reply"),
		)
		.await;
		let later_title =
			send(router, base, OWNER, "later-title", title(&title_root, "later topic")).await;
		// Redacting a root retains its live children's standard thread summary.
		redact(router, base, &title_root, "redact-root").await;
		let served = event(router, base, OWNER, &title_root).await;
		let summary = &served["unsigned"]["m.relations"]["m.thread"];
		assert_eq!(
			summary["latest_event"]["event_id"], later_title,
			"root redaction must preserve the standard latest event"
		);
		assert_eq!(
			summary["io.mindroom.title"]["event_id"], later_title,
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
