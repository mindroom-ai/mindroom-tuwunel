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

/// Keys and remote to-device messages too large to send in an EDU are refused.
///
/// Device keys, a cross-signing key and a to-device message for a remote user,
/// each padded past `MAX_EDU_CONTENT_BYTES`, are answered with 413.
#[test]
fn payloads_too_large_for_an_edu_are_refused() -> Result {
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

	let response = services
		.client
		.clients
		.default
		.put(client.url("sendToDevice/m.test/edu-content-size"))
		.bearer_auth(TOKEN)
		.json(&json!({ "messages": { "@peer:remote.example": { "PEER": { "pad": pad } } } }))
		.send()
		.await?;

	assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

	Ok(())
}
