#![cfg(test)]

use reqwest::StatusCode;
use serde_json::json;
use tuwunel_core::{
	Result,
	ruma::{UInt, presence::PresenceState, user_id},
};
use tuwunel_service::Services;

use self::{
	client::{Client, register},
	fixture::boot,
};

#[expect(
	dead_code,
	reason = "Only registration and the URL helper are shared with the client API harness."
)]
mod client;

mod fixture;

const TOKEN: &str = "presence-status-size-test-access-token";

/// A presence status message is held to 4 KiB.
///
/// A longer one is refused from a local user with 413 and dropped from a
/// federated update, whose presence state still applies.
#[test]
fn presence_status_is_held_to_4_kib() -> Result {
	let options: [&str; 0] = [];

	boot("presence-status-size", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let user_id = register(services, "presencestatus", TOKEN).await?;
	let url = Client { services, base, token: TOKEN }.url(&format!("presence/{user_id}/status"));
	let set = async |len: usize| -> Result<StatusCode> {
		let body = json!({ "presence": "online", "status_msg": "s".repeat(len) });
		let response = services
			.client
			.clients
			.default
			.put(&url)
			.bearer_auth(TOKEN)
			.json(&body)
			.send()
			.await?;

		Ok(response.status())
	};

	assert_eq!(set(4_096).await?, StatusCode::OK);
	assert_eq!(set(4_097).await?, StatusCode::PAYLOAD_TOO_LARGE);

	let local = services.presence.get_presence(&user_id).await?;

	assert_eq!(local.content.status_msg.map(|msg| msg.len()), Some(4_096));

	let remote = user_id!("@presencestatus:remote.test");
	let federate = async |state: PresenceState, len: usize| {
		services
			.presence
			.set_presence_from_federation(remote, &state, false, UInt::MIN, Some("s".repeat(len)))
			.await?;

		services.presence.get_presence(remote).await
	};

	let kept = federate(PresenceState::Online, 4_096).await?;

	assert_eq!(kept.content.status_msg.map(|msg| msg.len()), Some(4_096));

	let dropped = federate(PresenceState::Unavailable, 4_097).await?;

	assert_eq!(dropped.content.presence, PresenceState::Unavailable);
	assert_eq!(dropped.content.status_msg, None);

	Ok(())
}
