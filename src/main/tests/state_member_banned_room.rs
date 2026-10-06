#![cfg(test)]

use reqwest::StatusCode;
use serde_json::{Value, json};
use tuwunel_core::{
	Result, implement,
	ruma::{RoomId, UserId},
};
use tuwunel_service::Services;

use self::{
	client::{Client, register},
	fixture::boot,
};

mod client;
mod fixture;

const OWNER_TOKEN: &str = "state-member-banned-room-owner-access-token";
const USER_TOKEN: &str = "state-member-banned-room-user-access-token";

/// A join sent as a member state event is refused in a banned room.
///
/// `/join` refuses a room the server banned, so the same membership sent
/// through the state endpoint must not let a non-admin in either.
#[test]
fn state_member_join_refused_in_banned_room() -> Result {
	let options: [&str; 0] = [];

	boot("state-member-banned-room", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	register(services, "bannedroomowner", OWNER_TOKEN).await?;

	let user_id = register(services, "bannedroomuser", USER_TOKEN).await?;

	assert!(!services.admin.user_is_admin(&user_id).await, "{user_id} is a server admin");

	let owner = Client { services, base, token: OWNER_TOKEN };
	let user = Client { services, base, token: USER_TOKEN };

	let room_id = owner
		.create_room(&json!({ "preset": "public_chat" }))
		.await?;

	services.metadata.ban_room(&room_id);

	let (status, body) = user
		.put_membership(&room_id, &user_id, &json!({ "membership": "join" }))
		.await?;

	assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
	assert_eq!(body["errcode"], "M_FORBIDDEN", "{body}");
	assert!(
		!services
			.state_cache
			.is_joined(&user_id, &room_id)
			.await,
		"{user_id} joined the banned room"
	);

	Ok(())
}

/// Put a member state event for `target` as this user.
#[implement(Client, params = "<'_>")]
async fn put_membership(
	&self,
	room_id: &RoomId,
	target: &UserId,
	content: &Value,
) -> Result<(StatusCode, Value)> {
	let response = self
		.services
		.client
		.clients
		.default
		.put(self.url(&format!("rooms/{room_id}/state/m.room.member/{target}")))
		.bearer_auth(self.token)
		.json(content)
		.send()
		.await?;

	let status = response.status();
	let body = response.json().await?;

	Ok((status, body))
}
