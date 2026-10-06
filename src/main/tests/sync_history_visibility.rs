#![cfg(test)]

use serde_json::{Value, json};
use tuwunel_core::{
	Result, implement,
	ruma::{OwnedRoomId, RoomId, UserId},
};
use tuwunel_service::Services;

use self::{
	client::{Client, field, register},
	fixture::boot,
};

mod client;
mod fixture;

const OWNER_TOKEN: &str = "sync-history-visibility-owner-access-token";
const MEMBER_TOKEN: &str = "sync-history-visibility-member-access-token";
const INVITEE_TOKEN: &str = "sync-history-visibility-invitee-access-token";

/// Sync timelines carry only events the user's history visibility admits.
///
/// A member joining a `joined` room reads nothing sent before its join, in an
/// incremental sync covering the join, an initial sync, or a sliding sync. A
/// member who leaves and rejoins while another device is offline does not see
/// the topic change made meanwhile in the timeline, but gets it in the state.
/// An invitee rejecting an invite to a `shared` room sees its own leave but
/// none of the room's messages.
#[test]
fn sync_timelines_honour_history_visibility() -> Result {
	boot("sync-history-visibility", ["client_sync_timeout_min=0"], exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	register(services, "syncowner", OWNER_TOKEN).await?;
	register(services, "syncmember", MEMBER_TOKEN).await?;

	let invitee_id = register(services, "syncinvitee", INVITEE_TOKEN).await?;

	let owner = Client { services, base, token: OWNER_TOKEN };
	let member = Client { services, base, token: MEMBER_TOKEN };
	let invitee = Client { services, base, token: INVITEE_TOKEN };

	member_reads_nothing_before_its_join(&owner, &member).await?;
	rejoined_member_gets_the_state_changed_while_away(&owner, &member).await?;
	rejected_invite_shows_only_its_leave(&owner, &invitee, &invitee_id).await
}

async fn member_reads_nothing_before_its_join(owner: &Client<'_>, member: &Client<'_>) -> Result {
	let room_id = owner.create_joined_room().await?;

	let since = member.sync(None, &json!({})).await?;
	let since = field(&since, "next_batch")?;

	owner.send_text(&room_id, "before-join").await?;
	member.act(&room_id, "join").await?;
	owner.send_text(&room_id, "after-join").await?;

	let filter = json!({ "room": { "timeline": { "limit": 10 } } });
	let room = format!("/rooms/join/{room_id}/timeline/events");

	let incremental = member.sync(Some(since), &filter).await?;
	let initial = member.sync(None, &filter).await?;

	assert_eq!(bodies(&incremental, &room), ["after-join"], "incremental: {incremental}");
	assert_eq!(bodies(&initial, &room), ["after-join"], "initial: {initial}");

	let request = json!({
		"room_subscriptions": { room_id.as_str(): { "timeline_limit": 10 } },
	});

	let sliding = member.sliding_sync(&request).await?;
	let room = format!("/rooms/{room_id}/timeline");

	assert_eq!(bodies(&sliding, &room), ["after-join"], "sliding: {sliding}");

	Ok(())
}

async fn rejoined_member_gets_the_state_changed_while_away(
	owner: &Client<'_>,
	member: &Client<'_>,
) -> Result {
	let room_id = owner.create_joined_room().await?;

	member.act(&room_id, "join").await?;

	let since = member.sync(None, &json!({})).await?;
	let since = field(&since, "next_batch")?;

	member.act(&room_id, "leave").await?;
	owner
		.put(
			&format!("rooms/{room_id}/state/m.room.topic"),
			&json!({ "topic": "while-away" }),
		)
		.await?;
	member.act(&room_id, "join").await?;
	owner.send_text(&room_id, "after-rejoin").await?;

	let filter = json!({ "room": { "timeline": { "limit": 10 } } });
	let sync = member.sync(Some(since), &filter).await?;
	let room = format!("/rooms/join/{room_id}");

	let state_topic = sync
		.pointer(&format!("{room}/state/events"))
		.and_then(Value::as_array)
		.into_iter()
		.flatten()
		.any(|event| {
			event["type"] == "m.room.topic" && event["content"]["topic"] == "while-away"
		});

	assert_eq!(bodies(&sync, &format!("{room}/timeline/events")), ["after-rejoin"], "{sync}");
	assert!(state_topic, "topic change is missing from the state: {sync}");

	Ok(())
}

async fn rejected_invite_shows_only_its_leave(
	owner: &Client<'_>,
	invitee: &Client<'_>,
	invitee_id: &UserId,
) -> Result {
	let room_id = owner
		.create_room(&json!({ "preset": "private_chat" }))
		.await?;

	owner
		.post(&format!("rooms/{room_id}/invite"), &json!({ "user_id": invitee_id }))
		.await?;

	owner.send_text(&room_id, "while-invited").await?;
	invitee.act(&room_id, "leave").await?;

	let filter = json!({ "room": { "include_leave": true, "timeline": { "limit": 10 } } });
	let sync = invitee.sync(None, &filter).await?;
	let room = format!("/rooms/leave/{room_id}/timeline/events");

	let own_leave = sync
		.pointer(&room)
		.and_then(Value::as_array)
		.into_iter()
		.flatten()
		.any(|event| {
			event["state_key"] == invitee_id.as_str() && event["content"]["membership"] == "leave"
		});

	assert!(bodies(&sync, &room).is_empty(), "invitee reads the room: {sync}");
	assert!(own_leave, "invitee's leave is missing: {sync}");

	Ok(())
}

/// The message bodies in the timeline at a JSON pointer, oldest first.
fn bodies<'a>(response: &'a Value, timeline: &str) -> Vec<&'a str> {
	response
		.pointer(timeline)
		.and_then(Value::as_array)
		.into_iter()
		.flatten()
		.filter_map(|event| event.pointer("/content/body"))
		.filter_map(Value::as_str)
		.collect()
}

/// Create a public room whose history is visible only to joined members.
#[implement(Client, params = "<'_>")]
async fn create_joined_room(&self) -> Result<OwnedRoomId> {
	self.create_room(&json!({
		"preset": "public_chat",
		"initial_state": [{
			"type": "m.room.history_visibility",
			"state_key": "",
			"content": { "history_visibility": "joined" },
		}],
	}))
	.await
}

/// Run one legacy sync as this user with a filter and an optional `since`.
#[implement(Client, params = "<'_>")]
async fn sync(&self, since: Option<&str>, filter: &Value) -> Result<Value> {
	let filter = filter.to_string();
	let mut query = vec![("filter", filter.as_str()), ("timeout", "0")];

	query.extend(since.map(|since| ("since", since)));

	let response = self
		.services
		.client
		.clients
		.default
		.get(self.url("sync"))
		.bearer_auth(self.token)
		.query(&query)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(response)
}

/// Post one simplified sliding sync request as this user and parse the reply.
#[implement(Client, params = "<'_>")]
async fn sliding_sync(&self, body: &Value) -> Result<Value> {
	let url = format!(
		"{}/_matrix/client/unstable/org.matrix.simplified_msc3575/sync?timeout=0",
		self.base
	);

	self.post_url(&url, body).await
}

/// Apply a membership action to this user, such as a join or a leave.
#[implement(Client, params = "<'_>")]
async fn act(&self, room_id: &RoomId, action: &str) -> Result {
	self.post(&format!("rooms/{room_id}/{action}"), &json!({}))
		.await
		.map(drop)
}

/// Send a text message, using its body as the transaction id.
#[implement(Client, params = "<'_>")]
async fn send_text(&self, room_id: &RoomId, body: &str) -> Result {
	self.put(
		&format!("rooms/{room_id}/send/m.room.message/{body}"),
		&json!({ "msgtype": "m.text", "body": body }),
	)
	.await
}

/// Put a JSON body to one endpoint path as this user.
#[implement(Client, params = "<'_>")]
async fn put(&self, path: &str, body: &Value) -> Result {
	self.services
		.client
		.clients
		.default
		.put(self.url(path))
		.bearer_auth(self.token)
		.json(body)
		.send()
		.await?
		.error_for_status()?;

	Ok(())
}

/// Post a JSON body to one endpoint path as this user and parse the reply.
#[implement(Client, params = "<'_>")]
async fn post(&self, path: &str, body: &Value) -> Result<Value> {
	self.post_url(&self.url(path), body).await
}

/// Post a JSON body to a full URL as this user and parse the reply.
///
/// A non-success status is the error, so a caller only ever sees the body of
/// an accepted request.
#[implement(Client, params = "<'_>")]
async fn post_url(&self, url: &str, body: &Value) -> Result<Value> {
	let response = self
		.services
		.client
		.clients
		.default
		.post(url)
		.bearer_auth(self.token)
		.json(body)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(response)
}
