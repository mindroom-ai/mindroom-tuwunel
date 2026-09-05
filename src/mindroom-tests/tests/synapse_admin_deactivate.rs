mod support;

#[cfg(test)]
mod tests {
	use axum::{
		Router,
		body::Body,
		http::{Request, StatusCode},
	};
	use serde_json::Value;
	use tower::ServiceExt;
	use tuwunel_core::{
		Result,
		ruma::{OwnedRoomId, UserId, user_id},
	};
	use tuwunel_service::{Services, users::PASSWORD_SENTINEL};

	use super::support::Harness;

	/// The Synapse admin deactivation endpoints are upstream code whose calls
	/// into `deactivate.full_deactivate` get re-adapted at every rebase to pass
	/// `DeactivationReason::Admin` (see the doc comment on
	/// `DeactivationReason`). Pin the observable contract at the route level
	/// for both endpoints: an account deactivated through them records the
	/// admin reason, leaves its joined rooms, and is not resurrected by SSO
	/// re-login. Passing the reason must not restore the old shallow route.
	#[test]
	fn synapse_admin_deactivation_records_admin_reason() -> Result {
		let harness = Harness::new("mindroom_rebase_synapse_admin_deactivate", [])?;

		harness.with_services(async |services| {
			// The server user is always an admin (`user_is_admin` short-circuit),
			// which avoids standing up the admin room in the test database.
			let admin = services.globals.server_user.clone();
			if !services.users.exists(&admin).await {
				services
					.users
					.create(&admin, Some(PASSWORD_SENTINEL), None)
					.await?;
			}

			let token = "mindroom-test-synapse-admin-token-0123456789";
			services
				.users
				.create_device(&admin, None, (Some(token), None), None, None, None)
				.await?;

			let (state, _guard) = tuwunel_api::router::state::create(services.clone());
			let router =
				tuwunel_api::router::build(Router::new(), &services.server).with_state(state);

			// `POST /_synapse/admin/v1/deactivate/{user_id}`
			let alice = user_id!("@sso-admin-deact-v1:localhost");
			services
				.users
				.create(alice, Some(PASSWORD_SENTINEL), Some("sso"))
				.await?;
			let alice_room =
				joined_room(&services, &router, alice, "sso-admin-v1-room-token-0123456789")
					.await?;

			let response = router
				.clone()
				.oneshot(
					Request::builder()
						.method("POST")
						.uri(format!("/_synapse/admin/v1/deactivate/{alice}"))
						.header("Authorization", format!("Bearer {token}"))
						.header("Content-Type", "application/json")
						.header("X-Forwarded-For", "127.0.0.1")
						.body(Body::from(r#"{"erase":false}"#))
						.expect("valid request"),
				)
				.await
				.expect("router response");
			assert_eq!(
				response.status(),
				StatusCode::OK,
				"v1 deactivate should succeed for the server admin",
			);

			assert!(
				services.users.is_deactivated(alice).await?,
				"v1 deactivate should deactivate the account",
			);
			assert_eq!(
				services
					.users
					.deactivation_reason(alice)
					.await
					.as_deref(),
				Some("admin"),
				"v1 deactivate must record the admin reason",
			);
			assert!(
				!services
					.users
					.maybe_reactivate_deactivated_sso(alice)
					.await?,
				"an account the v1 endpoint deactivated must not SSO-reactivate",
			);

			// `PUT /_synapse/admin/v2/users/{user_id}` with `deactivated: true`
			let bob = user_id!("@sso-admin-deact-v2:localhost");
			services
				.users
				.create(bob, Some(PASSWORD_SENTINEL), Some("sso"))
				.await?;
			let bob_room =
				joined_room(&services, &router, bob, "sso-admin-v2-room-token-0123456789")
					.await?;

			let response = router
				.clone()
				.oneshot(
					Request::builder()
						.method("PUT")
						.uri(format!("/_synapse/admin/v2/users/{bob}"))
						.header("Authorization", format!("Bearer {token}"))
						.header("Content-Type", "application/json")
						.header("X-Forwarded-For", "127.0.0.1")
						.body(Body::from(r#"{"deactivated":true}"#))
						.expect("valid request"),
				)
				.await
				.expect("router response");
			assert_eq!(
				response.status(),
				StatusCode::OK,
				"v2 create-or-modify of an existing user should succeed",
			);

			assert!(
				services.users.is_deactivated(bob).await?,
				"v2 deactivated flag should deactivate the account",
			);
			assert_eq!(
				services
					.users
					.deactivation_reason(bob)
					.await
					.as_deref(),
				Some("admin"),
				"v2 deactivated flag must record the admin reason",
			);
			assert!(
				!services
					.users
					.maybe_reactivate_deactivated_sso(bob)
					.await?,
				"an account the v2 endpoint deactivated must not SSO-reactivate",
			);

			assert_eq!(
				[
					services
						.state_cache
						.is_joined(alice, &alice_room)
						.await,
					services
						.state_cache
						.is_joined(bob, &bob_room)
						.await,
				],
				[false, false],
				"both admin routes must leave the deactivated user's rooms",
			);

			Ok(())
		})
	}

	async fn joined_room(
		services: &Services,
		router: &Router,
		user: &UserId,
		token: &str,
	) -> Result<OwnedRoomId> {
		services
			.users
			.create_device(user, None, (Some(token), None), None, None, None)
			.await?;
		let response = router
			.clone()
			.oneshot(
				Request::builder()
					.method("POST")
					.uri("/_matrix/client/v3/createRoom")
					.header("Authorization", format!("Bearer {token}"))
					.header("Content-Type", "application/json")
					.header("X-Forwarded-For", "127.0.0.1")
					.body(Body::from(r#"{"preset":"private_chat"}"#))
					.expect("valid room request"),
			)
			.await
			.expect("room response");
		let status = response.status();
		let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
			.await
			.expect("read room response");
		assert_eq!(status, StatusCode::OK, "create joined room: {bytes:?}");
		let body: Value = serde_json::from_slice(&bytes)?;
		let room: OwnedRoomId = body["room_id"]
			.as_str()
			.expect("created room id")
			.try_into()
			.expect("valid room id");
		assert!(services.state_cache.is_joined(user, &room).await);
		Ok(room)
	}
}
