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

const PASSWORD: &str = "edu-content-size-test-password";

/// Keys and remote to-device messages too large to send in an EDU are refused.
///
/// Device keys, a cross-signing key, a to-device message for a remote user (by
/// its content or its device ID) and the device keys of a dehydrated device,
/// each padded past `MAX_EDU_CONTENT_BYTES`, are answered with 413. A login
/// choosing a device ID over 512 bytes, which device list updates carry, is
/// answered with 400.
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

	let to_device = json!({ "messages": { "@peer:remote.example": { "PEER": { "pad": pad } } } });
	let to_device_id = json!({ "messages": { "@peer:remote.example": { &pad: {} } } });
	let dehydrated_device_url =
		format!("{base}/_matrix/client/unstable/org.matrix.msc3814.v1/dehydrated_device");

	for (url, body) in [
		(client.url("sendToDevice/m.test/edu-content-size"), to_device),
		(client.url("sendToDevice/m.test/edu-device-id-size"), to_device_id),
		(dehydrated_device_url, dehydrated_device),
	] {
		let response = services
			.client
			.clients
			.default
			.put(&url)
			.bearer_auth(TOKEN)
			.json(&body)
			.send()
			.await?;

		assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE, "{url}");
	}

	services
		.users
		.set_password(&user_id, Some(PASSWORD))
		.await?;

	let login = json!({
		"type": "m.login.password",
		"identifier": { "type": "m.id.user", "user": user_id },
		"password": PASSWORD,
		"device_id": "x".repeat(513),
	});

	let response = client
		.post_url(&client.url("login"), &login)
		.await?;

	assert_eq!(response.status(), StatusCode::BAD_REQUEST, "login");

	Ok(())
}
