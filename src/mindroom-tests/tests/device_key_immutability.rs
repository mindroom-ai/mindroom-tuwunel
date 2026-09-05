mod support;

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use axum::{Router, body::Body};
	use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD as b64};
	use serde_json::{Map, Value, json};
	use tokio::sync::Barrier;
	use tower::ServiceExt;
	use tuwunel_core::{
		Result,
		http::{Request, StatusCode, header},
		ruma::{OneTimeKeyAlgorithm, device_id, encryption::DeviceKeys, serde::Raw, user_id},
	};

	use super::support::Harness;

	const USER: &str = "@alice:localhost";
	const DEVICE: &str = "IMMUTABLE";
	const TOKEN: &str = "device-key-immutability-token-0123456789";
	const CONCURRENT_DEVICE: &str = "CONCURRENT";
	const CONCURRENT_TOKEN: &str = "device-key-concurrency-token-0123456789ab";
	const REQUESTS_PER_IDENTITY: usize = 8;
	const CORRUPT_DEVICE: &str = "CORRUPT";
	const CORRUPT_TOKEN: &str = "device-key-corruption-token-0123456789abcdef";

	#[test]
	fn identity_keys_are_immutable_and_removed_with_the_device() -> Result {
		let harness = Harness::new("device_key_immutability", [])?;

		harness.with_services(async |services| {
			let user_id = user_id!("@alice:localhost");
			let device_id = device_id!(DEVICE);
			services
				.users
				.create(user_id, Some("password"), None)
				.await?;
			services
				.users
				.create_device(user_id, Some(device_id), (Some(TOKEN), None), None, None, None)
				.await?;

			let (state, _guard) = tuwunel_api::router::state::create(services.clone());
			let router =
				tuwunel_api::router::build(Router::new(), &services.server).with_state(state);

			let original = device_keys(DEVICE, 1, 2, 3);
			let mut first_body = json!({
				"device_keys": original,
				"one_time_keys": {
					"signed_curve25519:OTK": signed_key(4, false),
				},
				"fallback_keys": {
					"signed_curve25519:FALLBACK": signed_key(5, true),
				},
			});
			let first = upload(&router, TOKEN, first_body.take()).await;
			assert_eq!(first.0, StatusCode::OK, "first identity upload: {}", first.1);
			assert_eq!(
				first.1["one_time_key_counts"]["signed_curve25519"], 1,
				"the setup upload must persist its one-time key",
			);

			// The key material is identical but the signature differs. The upload
			// must be a no-op so server-added or previously-uploaded signatures
			// cannot be erased by a client retry.
			let exact_with_new_signature = device_keys(DEVICE, 1, 2, 9);
			let exact =
				upload(&router, TOKEN, json!({"device_keys": exact_with_new_signature})).await;
			assert_eq!(exact.0, StatusCode::OK, "exact-copy retry: {}", exact.1);

			let stored = services
				.users
				.get_device_keys(user_id, device_id)
				.await?;
			let stored: Value = serde_json::from_str(stored.json().get())?;
			let signature_id = format!("ed25519:{DEVICE}");
			assert_eq!(
				stored["signatures"][USER][signature_id.as_str()],
				encoded(3, 64),
				"an exact-key retry must preserve the stored signatures",
			);

			let rotated = device_keys(DEVICE, 6, 7, 8);
			let rejected = upload(&router, TOKEN, json!({"device_keys": rotated.clone()})).await;
			assert_eq!(rejected.0, StatusCode::FORBIDDEN, "rotated upload: {}", rejected.1);
			assert_eq!(rejected.1["errcode"], "M_FORBIDDEN");

			assert_eq!(
				services
					.users
					.count_one_time_keys(user_id, device_id)
					.await
					.get(&OneTimeKeyAlgorithm::SignedCurve25519)
					.copied(),
				Some(1_u32.into()),
			);
			assert!(
				services
					.users
					.take_fallback_key(user_id, device_id, &OneTimeKeyAlgorithm::SignedCurve25519,)
					.await
					.is_ok(),
				"the setup fallback key must exist before removal",
			);

			services
				.users
				.remove_device(user_id, device_id)
				.await;
			assert!(
				services
					.users
					.get_device_keys(user_id, device_id)
					.await
					.is_err(),
				"device removal must delete identity keys",
			);
			assert_eq!(
				services
					.users
					.count_one_time_keys(user_id, device_id)
					.await
					.get(&OneTimeKeyAlgorithm::SignedCurve25519)
					.copied(),
				Some(0_u32.into()),
				"device removal must delete one-time keys",
			);
			assert!(
				services
					.users
					.take_fallback_key(user_id, device_id, &OneTimeKeyAlgorithm::SignedCurve25519,)
					.await
					.is_err(),
				"device removal must delete fallback keys",
			);

			services
				.users
				.create_device(user_id, Some(device_id), (Some(TOKEN), None), None, None, None)
				.await?;
			let reused = upload(&router, TOKEN, json!({"device_keys": rotated})).await;
			assert_eq!(
				reused.0,
				StatusCode::OK,
				"a recreated device may install a fresh identity: {}",
				reused.1,
			);

			concurrent_first_uploads_choose_one_identity(&services, &router).await?;
			corrupt_stored_identity_is_not_treated_as_absent(&services, &router).await?;

			Ok(())
		})
	}

	async fn concurrent_first_uploads_choose_one_identity(
		services: &Arc<tuwunel_service::Services>,
		router: &Router,
	) -> Result {
		let user_id = user_id!("@alice:localhost");
		let device_id = device_id!(CONCURRENT_DEVICE);
		services
			.users
			.create_device(
				user_id,
				Some(device_id),
				(Some(CONCURRENT_TOKEN), None),
				None,
				None,
				None,
			)
			.await?;

		let barrier = Arc::new(Barrier::new(REQUESTS_PER_IDENTITY * 2 + 1));
		let mut tasks = Vec::with_capacity(REQUESTS_PER_IDENTITY * 2);
		for identity_seed in [10, 20] {
			for _ in 0..REQUESTS_PER_IDENTITY {
				let router = router.clone();
				let barrier = barrier.clone();
				let keys = device_keys(
					CONCURRENT_DEVICE,
					identity_seed,
					identity_seed
						.checked_add(1)
						.expect("bounded signing seed"),
					identity_seed
						.checked_add(2)
						.expect("bounded signature seed"),
				);
				tasks.push(tokio::spawn(async move {
					barrier.wait().await;
					(
						identity_seed,
						upload(&router, CONCURRENT_TOKEN, json!({"device_keys": keys}))
							.await
							.0,
					)
				}));
			}
		}

		barrier.wait().await;
		let mut first_successes = 0_usize;
		let mut second_successes = 0_usize;
		for task in tasks {
			let (identity_seed, status) = task.await.expect("upload task should join");
			assert!(
				matches!(status, StatusCode::OK | StatusCode::FORBIDDEN),
				"concurrent upload returned unexpected status {status}",
			);
			if status == StatusCode::OK {
				if identity_seed == 10 {
					first_successes = first_successes
						.checked_add(1)
						.expect("bounded first-identity success count");
				} else {
					second_successes = second_successes
						.checked_add(1)
						.expect("bounded second-identity success count");
				}
			}
		}

		assert!(
			matches!(
				(first_successes, second_successes),
				(REQUESTS_PER_IDENTITY, 0) | (0, REQUESTS_PER_IDENTITY)
			),
			"exactly one identity must win; successes were ({first_successes}, \
			 {second_successes})",
		);

		Ok(())
	}

	async fn corrupt_stored_identity_is_not_treated_as_absent(
		services: &Arc<tuwunel_service::Services>,
		router: &Router,
	) -> Result {
		let user_id = user_id!("@alice:localhost");
		let device_id = device_id!(CORRUPT_DEVICE);
		services
			.users
			.create_device(
				user_id,
				Some(device_id),
				(Some(CORRUPT_TOKEN), None),
				None,
				None,
				None,
			)
			.await?;

		let malformed = Raw::<DeviceKeys>::from_json_string("{}".to_owned())?;
		services
			.users
			.add_device_keys(user_id, device_id, &malformed)
			.await?;

		let response = upload(
			router,
			CORRUPT_TOKEN,
			json!({"device_keys": device_keys(CORRUPT_DEVICE, 30, 31, 32)}),
		)
		.await;
		assert_eq!(
			response.0,
			StatusCode::INTERNAL_SERVER_ERROR,
			"malformed stored keys must not be overwritten as if absent: {}",
			response.1,
		);
		assert_eq!(
			services
				.users
				.get_device_keys(user_id, device_id)
				.await?
				.json()
				.get(),
			"{}",
			"the failed upload must preserve the stored record for diagnosis",
		);

		Ok(())
	}

	async fn upload(router: &Router, token: &str, body: Value) -> (StatusCode, Value) {
		let request = Request::builder()
			.method("POST")
			.uri("/_matrix/client/v3/keys/upload")
			.header(header::AUTHORIZATION, format!("Bearer {token}"))
			.header(header::CONTENT_TYPE, "application/json")
			.header("X-Forwarded-For", "127.0.0.1")
			.body(Body::from(body.to_string()))
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

		(status, body)
	}

	fn device_keys(device: &str, curve_seed: u8, signing_seed: u8, signature_seed: u8) -> Value {
		let curve_id = format!("curve25519:{device}");
		let signing_id = format!("ed25519:{device}");
		let mut keys = Map::new();
		keys.insert(curve_id, Value::String(encoded(curve_seed, 32)));
		keys.insert(signing_id.clone(), Value::String(encoded(signing_seed, 32)));

		let mut user_signatures = Map::new();
		user_signatures.insert(signing_id, Value::String(encoded(signature_seed, 64)));
		let mut signatures = Map::new();
		signatures.insert(USER.to_owned(), Value::Object(user_signatures));

		json!({
			"user_id": USER,
			"device_id": device,
			"algorithms": [
				"m.olm.v1.curve25519-aes-sha2",
				"m.megolm.v1.aes-sha2",
			],
			"keys": keys,
			"signatures": signatures,
		})
	}

	fn signed_key(seed: u8, fallback: bool) -> Value {
		let mut signature = Map::new();
		signature.insert(
			format!("ed25519:{DEVICE}"),
			Value::String(encoded(
				seed.checked_add(1)
					.expect("bounded signature seed"),
				64,
			)),
		);
		let mut signatures = Map::new();
		signatures.insert(USER.to_owned(), Value::Object(signature));

		json!({
			"key": encoded(seed, 32),
			"fallback": fallback,
			"signatures": signatures,
		})
	}

	fn encoded(seed: u8, len: usize) -> String { b64.encode(vec![seed; len]) }
}
