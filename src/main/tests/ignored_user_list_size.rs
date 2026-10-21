#![cfg(test)]

use reqwest::StatusCode;
use serde_json::{Map, Value, json};
use tuwunel_core::Result;
use tuwunel_service::Services;

use self::{client::register, fixture::boot};

#[expect(dead_code)] // Only registration is used from the client harness.
mod client;
mod fixture;

const TOKEN: &str = "ignored-user-list-size-test-token";

/// An ignored user list past the size limit is refused.
///
/// A list of 50,000 ordinary user IDs, about 1.5 MiB, is answered with 413 and
/// not stored, while the same content under another type and a list of 100
/// are stored.
#[test]
fn an_oversized_ignored_user_list_is_refused() -> Result {
	boot("ignored-user-list-size", ["log_enable=false"], exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let user_id = register(services, "ignorer", TOKEN).await?;

	let put = async |kind: &str, count: usize| {
		let ignored_users: Map<_, _> = (0..count)
			.map(|index| (format!("@spammer{index}:example.com"), json!({})))
			.collect();

		services
			.client
			.clients
			.default
			.put(format!("{base}/_matrix/client/v3/user/{user_id}/account_data/{kind}"))
			.bearer_auth(TOKEN)
			.json(&json!({ "ignored_users": ignored_users }))
			.send()
			.await
	};

	let response = put("m.ignored_user_list", 50_000).await?;

	assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
	assert_eq!(response.json::<Value>().await?["errcode"], "M_TOO_LARGE");
	assert!(
		services
			.users
			.ignored_users(&user_id)
			.await
			.is_none()
	);

	let response = put("org.example.list", 50_000).await?;

	assert_eq!(response.status(), StatusCode::OK);

	let response = put("m.ignored_user_list", 100).await?;

	assert_eq!(response.status(), StatusCode::OK);

	let stored = services
		.users
		.ignored_users(&user_id)
		.await
		.expect("an ordinary list is stored");

	assert_eq!(stored.ignored_users.len(), 100);

	Ok(())
}
