#![cfg(test)]

use futures::StreamExt;
use reqwest::StatusCode;
use serde_json::json;
use tuwunel_core::{Result, ruma::OwnedDeviceId};
use tuwunel_service::{Services, sending::MAX_EDU_CONTENT_BYTES};

use self::{
	client::{Client, register},
	fixture::boot,
};

#[expect(dead_code)] // Only raw posts and registration are used from the client harness.
mod client;
mod fixture;

const TOKEN: &str = "edu-content-size-test-access-token";

/// Keys too large to send in an EDU are refused.
///
/// Device keys, a cross-signing key and the device keys of a dehydrated device,
/// each padded past `MAX_EDU_CONTENT_BYTES`, are answered with 413.
#[test]
fn keys_too_large_for_an_edu_are_refused() -> Result {
	let options: [&str; 0] = [];

	boot("edu-content-size", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let user_id = register(services, "edusize", TOKEN).await?;
	let device_ids: Vec<OwnedDeviceId> = services
		.users
		.all_device_ids(&user_id)
		.map(ToOwned::to_owned)
		.collect()
		.await;

	let client = Client { services, base, token: TOKEN };
	let pad = "x".repeat(MAX_EDU_CONTENT_BYTES);
	let device_keys = json!({ "device_keys": {
		"user_id": user_id,
		"device_id": device_ids[0],
		"algorithms": [],
		"keys": {},
		"signatures": {},
		"pad": pad,
	}});

	let dehydrated_device = json!({
		"device_id": "DEHYDRATED",
		"device_data": { "algorithm": "m.dehydration.v1.olm" },
		"device_keys": device_keys["device_keys"],
	});

	let master_key = json!({ "master_key": {
		"user_id": user_id,
		"usage": ["master"],
		"keys": { "ed25519:master": "master" },
		"pad": pad,
	}});

	for (path, body) in [("keys/upload", device_keys), ("keys/device_signing/upload", master_key)]
	{
		let response = client.post_url(&client.url(path), &body).await?;

		assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE, "{path}");
	}

	let dehydrated_device_url =
		format!("{base}/_matrix/client/unstable/org.matrix.msc3814.v1/dehydrated_device");

	let response = services
		.client
		.clients
		.default
		.put(&dehydrated_device_url)
		.bearer_auth(TOKEN)
		.json(&dehydrated_device)
		.send()
		.await?;

	assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE, "dehydrated_device");

	Ok(())
}
