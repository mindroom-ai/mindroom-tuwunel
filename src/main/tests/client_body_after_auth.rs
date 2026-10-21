#![cfg(test)]

use serde_json::{Map, Value, json};
use tuwunel_core::Result;
use tuwunel_service::Services;

use self::{
	client::{Client, register},
	fixture::boot,
};

#[expect(dead_code)] // Only posting and registration are used from the client harness.
mod client;
mod fixture;

const TOKEN: &str = "client-body-after-auth-access-token";

const MAS_SECRET: &str = "client-body-after-auth-mas-secret";

/// A client request's body is parsed only once the caller is authenticated,
/// and the body of a request without an account only when it holds few values.
#[test]
fn client_bodies_are_parsed_after_authentication() -> Result {
	let options = [format!("mas_secret=\"{MAS_SECRET}\"")];

	boot("client-body-after-auth", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let http = &services.client.clients.default;
	let server_name = services.globals.server_name();
	let user_id = register(services, "bodyafterauth", TOKEN).await?;

	// The body is not JSON, so a route that read it first would answer M_NOT_JSON.
	let response = http
		.put(format!("{base}/_matrix/client/v3/user/{user_id}/account_data/m.test"))
		.body("not json")
		.send()
		.await?;

	let status = response.status().as_u16();
	let body: Value = response.json().await?;

	assert_eq!(status, 401, "{body}");
	assert_eq!(body["errcode"], "M_MISSING_TOKEN");

	// A login without an account is refused before its 2,048 values are parsed.
	// The array is cut short, so a parse would answer M_NOT_JSON, and each value
	// is a string of an escaped quote, after which counting goes on.
	let login = format!("{base}/_matrix/client/v3/login");
	let response = http
		.post(&login)
		.body(format!("[{}", r#""\"","#.repeat(2048)))
		.send()
		.await?;

	let status = response.status().as_u16();
	let body: Value = response.json().await?;

	assert_eq!(status, 413, "{body}");
	assert_eq!(body["errcode"], "M_TOO_LARGE");

	// Commas and colons inside a string, after an escaped quote, are no values.
	let password = format!("\"{}", ",:".repeat(2048));
	let response = http
		.post(&login)
		.json(&json!({
			"type": "m.login.password",
			"identifier": { "type": "m.id.user", "user": "bodyafterauth" },
			"password": password,
		}))
		.send()
		.await?;

	let status = response.status().as_u16();
	let body: Value = response.json().await?;

	assert_eq!(status, 403, "{body}");
	assert_eq!(body["errcode"], "M_FORBIDDEN");

	// An authenticated key query for the members of a large room has no such limit.
	let device_keys: Map<String, Value> = (0..2048)
		.map(|n| (format!("@member{n}:{server_name}"), json!([])))
		.collect();

	let client = Client { services, base, token: TOKEN };
	client
		.post("keys/query", &json!({ "device_keys": device_keys }))
		.await?;

	// Nor has MAS, which syncs the devices of a user with many sessions.
	let devices: Vec<String> = (0..2048).map(|n| format!("DEVICE{n}")).collect();
	let mas = Client { services, base, token: MAS_SECRET };
	let response = mas
		.post_url(
			&format!("{base}/_synapse/mas/sync_devices"),
			&json!({ "localpart": user_id.localpart(), "devices": devices }),
		)
		.await?;

	let status = response.status().as_u16();
	let body: Value = response.json().await?;

	assert_eq!(status, 200, "{body}");

	Ok(())
}
