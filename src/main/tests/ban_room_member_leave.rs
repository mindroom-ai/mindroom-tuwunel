#![cfg(test)]

use reqwest::StatusCode;
use serde_json::json;
use tuwunel_core::{
	Result,
	ruma::events::{
		StateEventType,
		room::member::{MembershipState, RoomMemberEventContent},
	},
};
use tuwunel_service::Services;

use self::{
	admin::accepted,
	client::{Client, register},
	fixture::boot,
};

#[expect(dead_code)] // This test expects no admin command to be refused.
mod admin;
mod client;
mod fixture;

const OWNER_TOKEN: &str = "ban-room-member-leave-owner-access-token";
const USER_TOKEN: &str = "ban-room-member-leave-user-access-token";

/// `ban-room` makes the room's local members leave it.
///
/// Clearing only the membership cache left the member's join in room state,
/// where it still authorized the messages they sent.
#[test]
fn ban_room_makes_local_members_leave() -> Result {
	let options: [&str; 0] = [];

	boot("ban-room-member-leave", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	register(services, "banroomowner", OWNER_TOKEN).await?;

	let user_id = register(services, "banroomuser", USER_TOKEN).await?;

	let owner = Client { services, base, token: OWNER_TOKEN };
	let user = Client { services, base, token: USER_TOKEN };

	let room_id = owner
		.create_room(&json!({ "preset": "public_chat" }))
		.await?;

	user.post(&format!("rooms/{room_id}/join"), &json!({}))
		.await?;

	accepted(services, &format!("rooms moderation ban-room {room_id}")).await?;

	let member = services
		.state_accessor
		.room_state_get_content::<RoomMemberEventContent>(
			&room_id,
			&StateEventType::RoomMember,
			user_id.as_str(),
		)
		.await?;

	assert_eq!(member.membership, MembershipState::Leave);

	let send = services
		.client
		.clients
		.default
		.put(user.url(&format!("rooms/{room_id}/send/m.room.message/after-ban")))
		.bearer_auth(USER_TOKEN)
		.json(&json!({ "msgtype": "m.text", "body": "after-ban" }))
		.send()
		.await?;

	assert_eq!(send.status(), StatusCode::FORBIDDEN);

	Ok(())
}
