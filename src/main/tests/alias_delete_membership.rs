#![cfg(test)]

use reqwest::StatusCode;
use serde_json::json;
use tuwunel_core::{
	Result, implement,
	ruma::{RoomAliasId, RoomId},
};
use tuwunel_service::Services;

use self::{
	client::{Client, register},
	fixture::boot,
};

mod client;
mod fixture;

const OWNER_TOKEN: &str = "alias-delete-membership-owner-access-token";
const MEMBER_TOKEN: &str = "alias-delete-membership-member-access-token";

/// Deleting someone else's alias by power level takes a joined member.
///
/// Every user holds the canonical alias level in this room, so the member
/// deletes one alias while joined, but not another after leaving.
#[test]
fn alias_deletion_requires_membership() -> Result {
	let options: [&str; 0] = [];

	boot("alias-delete-membership", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	register(services, "aliasowner", OWNER_TOKEN).await?;

	let member_id = register(services, "aliasmember", MEMBER_TOKEN).await?;

	let owner = Client { services, base, token: OWNER_TOKEN };
	let member = Client { services, base, token: MEMBER_TOKEN };

	let room_id = owner
		.create_room(&json!({
			"preset": "private_chat",
			"power_level_content_override": { "users_default": 50 },
		}))
		.await?;

	let server_name = services.globals.server_name();
	let joined = RoomAliasId::parse(format!("#joined:{server_name}"))?;
	let left = RoomAliasId::parse(format!("#left:{server_name}"))?;

	owner.put_alias(&joined, &room_id).await?;
	owner.put_alias(&left, &room_id).await?;
	owner
		.post(&format!("rooms/{room_id}/invite"), &json!({ "user_id": member_id }))
		.await?;

	member
		.post(&format!("rooms/{room_id}/join"), &json!({}))
		.await?;

	assert_eq!(member.delete_alias(&joined).await?, StatusCode::OK);

	member
		.post(&format!("rooms/{room_id}/leave"), &json!({}))
		.await?;

	assert_eq!(member.delete_alias(&left).await?, StatusCode::FORBIDDEN);
	assert_eq!(services.alias.resolve_local_alias(&left).await?, room_id);

	Ok(())
}

/// The client-API path of an alias in the room directory.
fn directory_path(alias: &RoomAliasId) -> String {
	format!("directory/room/{}", alias.as_str().replace('#', "%23"))
}

/// Point an alias at a room as this user.
#[implement(Client, params = "<'_>")]
async fn put_alias(&self, alias: &RoomAliasId, room_id: &RoomId) -> Result {
	self.services
		.client
		.clients
		.default
		.put(self.url(&directory_path(alias)))
		.bearer_auth(self.token)
		.json(&json!({ "room_id": room_id }))
		.send()
		.await?
		.error_for_status()?;

	Ok(())
}

/// Delete an alias as this user and return the status it answers with.
#[implement(Client, params = "<'_>")]
async fn delete_alias(&self, alias: &RoomAliasId) -> Result<StatusCode> {
	let response = self
		.services
		.client
		.clients
		.default
		.delete(self.url(&directory_path(alias)))
		.bearer_auth(self.token)
		.send()
		.await?;

	Ok(response.status())
}
