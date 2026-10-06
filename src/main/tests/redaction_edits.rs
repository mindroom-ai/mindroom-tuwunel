#![cfg(test)]

use serde_json::{Value, json};
use tuwunel_core::{Result, implement};
use tuwunel_service::Services;

use self::{
	client::{Client, field, register},
	fixture::boot,
};

mod client;
mod fixture;

const OWNER_TOKEN: &str = "redaction-edits-owner-access-token";
const MEMBER_TOKEN: &str = "redaction-edits-member-access-token";

/// Redacting an edited message redacts its edits too.
///
/// An edit carries the message's new text in `m.new_content`, so another member
/// could otherwise still read it from the edit or from the message's relations.
#[test]
fn redaction_redacts_the_edits() -> Result {
	let options: [&str; 0] = [];

	boot("redaction-edits", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	register(services, "editowner", OWNER_TOKEN).await?;
	register(services, "editmember", MEMBER_TOKEN).await?;

	let owner = Client { services, base, token: OWNER_TOKEN };
	let member = Client { services, base, token: MEMBER_TOKEN };

	let room_id = owner
		.create_room(&json!({ "preset": "public_chat" }))
		.await?;

	member
		.post(&format!("rooms/{room_id}/join"), &json!({}))
		.await?;

	let send = format!("rooms/{room_id}/send/m.room.message");
	let message = json!({ "msgtype": "m.text", "body": "draft" });
	let message = owner
		.put(&format!("{send}/message"), &message)
		.await?;
	let message = field(&message, "event_id")?;

	let edit = json!({
		"msgtype": "m.text",
		"body": "* secret",
		"m.new_content": { "msgtype": "m.text", "body": "secret" },
		"m.relates_to": { "rel_type": "m.replace", "event_id": message },
	});
	let edit = owner.put(&format!("{send}/edit"), &edit).await?;
	let edit = field(&edit, "event_id")?;

	owner
		.put(&format!("rooms/{room_id}/redact/{message}/redaction"), &json!({}))
		.await?;

	let edit_view = member
		.get(&member.url(&format!("rooms/{room_id}/event/{edit}")))
		.await?;

	assert_eq!(edit_view["content"], json!({}), "edit kept its content: {edit_view}");

	let relations =
		format!("{base}/_matrix/client/v1/rooms/{room_id}/relations/{message}/m.replace");
	let relations = member.get(&relations).await?;

	assert_eq!(relations["chunk"], json!([]), "redacted message kept its edit: {relations}");

	Ok(())
}

/// Put a JSON body to one endpoint path as this user and parse the reply.
#[implement(Client, params = "<'_>")]
async fn put(&self, path: &str, body: &Value) -> Result<Value> {
	let response = self
		.services
		.client
		.clients
		.default
		.put(self.url(path))
		.bearer_auth(self.token)
		.json(body)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(response)
}

/// Get an absolute URL as this user and parse the reply.
#[implement(Client, params = "<'_>")]
async fn get(&self, url: &str) -> Result<Value> {
	let response = self
		.services
		.client
		.clients
		.default
		.get(url)
		.bearer_auth(self.token)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(response)
}
