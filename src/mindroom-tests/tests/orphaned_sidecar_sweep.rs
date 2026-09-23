//! The `media delete-orphaned-long-text-sidecars` admin command, end to end:
//! real uploads in the media service and its file storage, retained events
//! sent through the client API, and the command's report and deletions.
//!
//! Sidecars are dated by the storage object's modification time, so the test
//! backdates the stored files of the "old" uploads before sweeping.

mod support;

#[cfg(test)]
mod tests {
	use std::{
		fs,
		sync::Arc,
		time::{Duration, SystemTime},
	};

	use axum::{Router, body::Body};
	use serde_json::{Value as JsonValue, json};
	use tower::ServiceExt;
	use tuwunel_core::{
		Result,
		http::{Request, StatusCode, header},
		ruma::{Mxc, UserId, user_id},
		utils::content_disposition::make_content_disposition,
	};
	use tuwunel_service::Services;

	use super::support::Harness;

	const BOT_TOKEN: &str = "mindroom-test-access-token-sweep-bot-0123456";
	const COMMAND: &str = "media delete-orphaned-long-text-sidecars";

	/// One harness per process (the tracing subscriber is global).
	#[test]
	fn orphaned_long_text_sidecar_sweep() -> Result {
		// The default `mindroom_edit_purge_min_age_secs` (one day) puts the
		// shortest accepted cutoff at two days.
		let harness = Harness::new("mindroom_orphaned_sidecar_sweep", [])?;

		harness.with_services(async |services| {
			tuwunel_admin::init(&services.admin);
			let result = sweep_orphaned_sidecars(&services).await;
			tuwunel_admin::fini(&services.admin);

			result
		})
	}

	async fn sweep_orphaned_sidecars(services: &Arc<Services>) -> Result {
		let bot = user_id!("@mindroom_bot:localhost");
		let alice = user_id!("@alice:localhost");
		services
			.users
			.create(bot, Some("password"), None)
			.await?;
		services
			.users
			.create_device(bot, None, (Some(BOT_TOKEN), None), None, None, None)
			.await?;
		services
			.users
			.create(alice, Some("password"), None)
			.await?;

		let (state, _guard) = tuwunel_api::router::state::create(services.clone());
		let router =
			tuwunel_api::router::build(Router::new(), &services.server).with_state(state);
		let created = request(
			&router,
			"POST",
			"/_matrix/client/v3/createRoom",
			Some(json!({"preset": "public_chat"})),
		)
		.await;
		let room_id = created["room_id"]
			.as_str()
			.expect("createRoom returns room_id")
			.to_owned();

		let sidecar = ("application/json", "message-content.json");
		let orphan = upload(services, "sweepOrphan", bot, sidecar).await?;
		let streaming = upload(services, "sweepStreaming", bot, sidecar).await?;
		let terminal = upload(services, "sweepTerminal", bot, sidecar).await?;
		let alice_orphan = upload(services, "sweepAliceOrphan", alice, sidecar).await?;
		let encrypted = upload(
			services,
			"sweepEncrypted",
			bot,
			("application/octet-stream", "message-content.json.enc"),
		)
		.await?;
		let other_json =
			upload(services, "sweepOtherJson", bot, ("application/json", "settings.json"))
				.await?;
		let not_json =
			upload(services, "sweepNotJson", bot, ("text/plain", "message-content.json")).await?;

		// A retained in-progress streaming preview and a retained terminal
		// `m.file` preview each reference their sidecar.
		let original = send(
			&router,
			&room_id,
			"sweep-original",
			json!({
				"msgtype": "m.notice",
				"body": "thinking",
			}),
		)
		.await;
		send(
			&router,
			&room_id,
			"sweep-streaming",
			json!({
				"msgtype": "m.notice",
				"body": "* partial answer",
				"m.new_content": {
					"msgtype": "m.notice",
					"body": "partial answer",
					"url": streaming,
					"io.mindroom.long_text": {"version": 2, "encoding": "matrix_event_content_json"},
				},
				"m.relates_to": {"rel_type": "m.replace", "event_id": original},
			}),
		)
		.await;
		send(
			&router,
			&room_id,
			"sweep-terminal",
			json!({
				"msgtype": "m.file",
				"body": "full answer",
				"filename": "message-content.json",
				"url": terminal,
				"io.mindroom.long_text": {"version": 2, "encoding": "matrix_event_content_json"},
			}),
		)
		.await;

		backdate_stored_media(services, Duration::from_hours(72))?;
		let fresh = upload(services, "sweepFresh", bot, sidecar).await?;

		let refused = admin(services, &format!("{COMMAND} --older-than 1d")).await;
		check_output(&refused, false, &["must be at least"]);

		let bot_sweep = format!("{COMMAND} --older-than 2d --uploader-prefix @mindroom_");
		let dry_run = admin(services, &bot_sweep).await;
		check_output(&dry_run, true, &[
			"dry run",
			"- Unencrypted sidecars examined: 4",
			"- Encrypted sidecars skipped: 1",
			"- Kept, referenced by a retained event: 2",
			"- Kept, newer than the cutoff: 1",
			"- Deletable: 1",
			&orphan,
		]);
		assert_media(services, &[
			(&orphan, true),
			(&streaming, true),
			(&terminal, true),
			(&alice_orphan, true),
			(&encrypted, true),
			(&other_json, true),
			(&not_json, true),
			(&fresh, true),
		])
		.await?;

		let executed = admin(services, &format!("{bot_sweep} --execute")).await;
		check_output(&executed, true, &["- Deleted: 1", "- Failed: 0"]);
		assert_media(services, &[
			(&orphan, false),
			(&streaming, true),
			(&terminal, true),
			(&alice_orphan, true),
			(&encrypted, true),
			(&other_json, true),
			(&not_json, true),
			(&fresh, true),
		])
		.await?;

		let alice_sweep = format!(
			"{COMMAND} --older-than 2d --uploader-regex ^@alice:localhost$ --limit 10 --execute"
		);
		let executed = admin(services, &alice_sweep).await;
		check_output(&executed, true, &["- Unencrypted sidecars examined: 1", "- Deleted: 1"]);
		assert_media(services, &[(&alice_orphan, false), (&streaming, true), (&terminal, true)])
			.await
	}

	async fn upload(
		services: &Services,
		media_id: &str,
		uploader: &UserId,
		(content_type, filename): (&str, &str),
	) -> Result<String> {
		let server_name = services.globals.server_name();
		let mxc = Mxc { server_name, media_id };
		let disposition = make_content_disposition(None, Some(content_type), Some(filename));
		services
			.media
			.create(
				&mxc,
				Some(uploader),
				Some(&disposition),
				Some(content_type),
				br#"{"body":"x"}"#,
			)
			.await?;

		Ok(mxc.to_string())
	}

	/// Set every stored media file's modification time `age` into the past.
	fn backdate_stored_media(services: &Services, age: Duration) -> Result {
		let modified = SystemTime::now()
			.checked_sub(age)
			.expect("representable time");
		for entry in fs::read_dir(services.media.get_media_dir())? {
			let path = entry?.path();
			if path.is_file() {
				fs::File::options()
					.write(true)
					.open(&path)?
					.set_modified(modified)?;
			}
		}

		Ok(())
	}

	async fn assert_media(services: &Services, expected: &[(&String, bool)]) -> Result {
		for &(mxc, present) in expected {
			let parsed = Mxc::try_from(mxc.as_str())?;
			assert_eq!(
				services
					.media
					.get_metadata(&parsed)
					.await
					.is_some(),
				present,
				"media {mxc} present",
			);
		}

		Ok(())
	}

	/// Output of an admin command, and whether it succeeded.
	async fn admin(services: &Services, command: &str) -> (bool, String) {
		match services
			.admin
			.command_in_place(command.to_owned(), None)
			.await
		{
			| Ok(Some(output)) => (true, output.as_str().to_owned()),
			| Ok(None) => (true, String::new()),
			| Err(output) => (false, output.as_str().to_owned()),
		}
	}

	fn check_output((ok, output): &(bool, String), success: bool, needles: &[&str]) {
		assert_eq!(*ok, success, "command success, with output:\n{output}");
		for needle in needles {
			assert!(output.contains(needle), "expected {needle:?} in output:\n{output}");
		}
	}

	async fn send(router: &Router, room_id: &str, txn_id: &str, content: JsonValue) -> String {
		let sent = request(
			router,
			"PUT",
			&format!(
				"/_matrix/client/v3/rooms/{}/send/m.room.message/{txn_id}",
				room_id.replace('!', "%21").replace(':', "%3A")
			),
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
			.header(header::AUTHORIZATION, format!("Bearer {BOT_TOKEN}"))
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
}
