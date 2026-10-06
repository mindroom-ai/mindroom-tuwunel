#![cfg(test)]

use serde_json::{Value, json};
use tuwunel_core::{
	Result, implement,
	ruma::{EventId, OwnedEventId, RoomId},
};
use tuwunel_service::Services;

use self::{
	client::{Client, field, register},
	fixture::boot,
};

mod client;
mod fixture;

const OWNER_TOKEN: &str = "bundled-relations-after-leave-owner-access-token";
const MEMBER_TOKEN: &str = "bundled-relations-after-leave-member-access-token";

/// A user who left a room gets no bundled reply or edit sent after they left.
///
/// The thread root itself stays visible to them, but its stored thread summary
/// names a later reply and its newest edit came later too, so the summary is
/// omitted and the original body is served. This holds for `/event`, the
/// room's `/initialSync` and `/notifications`. A current member still gets
/// both.
#[test]
fn bundled_relations_withhold_events_after_leave() -> Result {
	boot("bundled-relations-after-leave", ["bundle_edit_relations=true"], exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	register(services, "bundleowner", OWNER_TOKEN).await?;
	register(services, "bundlemember", MEMBER_TOKEN).await?;

	let owner = Client { services, base, token: OWNER_TOKEN };
	let member = Client { services, base, token: MEMBER_TOKEN };

	let room_id = owner
		.create_room(&json!({ "preset": "public_chat" }))
		.await?;

	member.act(&room_id, "join").await?;

	let root = owner
		.send(&room_id, "root", &json!({ "msgtype": "m.text", "body": "root" }))
		.await?;

	owner
		.reply(&room_id, &root, "before-leave")
		.await?;

	member.act(&room_id, "leave").await?;

	owner
		.reply(&room_id, &root, "after-leave")
		.await?;

	let edit = json!({
		"msgtype": "m.text",
		"body": "* edited-after-leave",
		"m.new_content": { "msgtype": "m.text", "body": "edited-after-leave" },
		"m.relates_to": { "rel_type": "m.replace", "event_id": root },
	});

	owner.send(&room_id, "edit", &edit).await?;

	let path = format!("rooms/{room_id}/event/{root}");
	let member_view = member.get(&path).await?;
	let owner_view = owner.get(&path).await?;

	let member_text = member_view.to_string();

	assert!(
		!member_text.contains("after-leave"),
		"departed member reads events sent after leaving: {member_view}"
	);
	assert!(
		member_view
			.pointer("/unsigned/m.relations/m.thread")
			.is_none(),
		"departed member keeps a thread summary: {member_view}"
	);
	assert_eq!(member_view["content"]["body"], "root", "departed member lost the root body");

	let initial_sync = member
		.get(&format!("rooms/{room_id}/initialSync"))
		.await?;

	let notifications = member.get("notifications").await?;
	for (view, items, event) in [
		(&initial_sync, "/messages/chunk", ""),
		(&notifications, "/notifications", "/event"),
	] {
		let served_root = view
			.pointer(items)
			.and_then(Value::as_array)
			.into_iter()
			.flatten()
			.filter_map(|item| item.pointer(event))
			.any(|event| event["event_id"] == root.as_str());

		assert!(served_root, "departed member lost the root: {view}");
		assert!(
			!view.to_string().contains("after-leave"),
			"departed member reads events sent after leaving: {view}"
		);
	}

	assert_eq!(
		owner_view.pointer("/unsigned/m.relations/m.thread/latest_event/content/body"),
		Some(&json!("after-leave")),
		"owner lost the thread's latest reply: {owner_view}"
	);
	assert_eq!(
		owner_view.pointer("/unsigned/m.relations/m.replace/content/m.new_content/body"),
		Some(&json!("edited-after-leave")),
		"owner lost the root's edit: {owner_view}"
	);

	Ok(())
}

/// Apply a membership action to this user, such as a join or a leave.
#[implement(Client, params = "<'_>")]
async fn act(&self, room_id: &RoomId, action: &str) -> Result {
	self.services
		.client
		.clients
		.default
		.post(self.url(&format!("rooms/{room_id}/{action}")))
		.bearer_auth(self.token)
		.json(&json!({}))
		.send()
		.await?
		.error_for_status()?;

	Ok(())
}

/// Send a thread reply to `root`, using its body as the transaction id.
#[implement(Client, params = "<'_>")]
async fn reply(&self, room_id: &RoomId, root: &EventId, body: &str) -> Result {
	let content = json!({
		"msgtype": "m.text",
		"body": body,
		"m.relates_to": {
			"rel_type": "m.thread",
			"event_id": root,
			"is_falling_back": true,
			"m.in_reply_to": { "event_id": root },
		},
	});

	self.send(room_id, body, &content).await.map(drop)
}

/// Send a message event and return its id.
#[implement(Client, params = "<'_>")]
async fn send(&self, room_id: &RoomId, txn: &str, content: &Value) -> Result<OwnedEventId> {
	let response: Value = self
		.services
		.client
		.clients
		.default
		.put(self.url(&format!("rooms/{room_id}/send/m.room.message/{txn}")))
		.bearer_auth(self.token)
		.json(content)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(field(&response, "event_id")?.try_into()?)
}

/// Get one endpoint path as this user and parse the reply.
#[implement(Client, params = "<'_>")]
async fn get(&self, path: &str) -> Result<Value> {
	let response = self
		.services
		.client
		.clients
		.default
		.get(self.url(path))
		.bearer_auth(self.token)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(response)
}
