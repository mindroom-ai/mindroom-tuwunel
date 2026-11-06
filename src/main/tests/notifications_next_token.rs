#![cfg(test)]

use std::time::Duration;

use serde_json::{Value, json};
use tuwunel_core::{Result, err, implement, ruma::RoomId, utils::BoolExt};
use tuwunel_service::Services;

use self::{
	client::{Client, field, poll_until, register},
	fixture::boot,
};

mod client;
mod fixture;

const READER_TOKEN: &str = "notifications-next-token-reader-token";
const SENDER_TOKEN: &str = "notifications-next-token-sender-token";
const NOTIFIED_DEADLINE: Duration = Duration::from_secs(5);

/// `/notifications` gives a `next_token` only while more notifications follow.
///
/// Three messages in a room of two notify the other member three times. Read
/// three at a time they are one page without a `next_token`. Read two at a
/// time, the first page has a `next_token` and the page after it, which
/// reaches the end, has none.
#[test]
fn notifications_next_token_only_before_the_end() -> Result {
	boot("notifications-next-token", ["log_enable=false"], exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	register(services, "notifiedreader", READER_TOKEN).await?;
	register(services, "notifyingsender", SENDER_TOKEN).await?;

	let reader = Client { services, base, token: READER_TOKEN };
	let sender = Client { services, base, token: SENDER_TOKEN };
	let room_id = sender
		.create_room(&json!({ "preset": "public_chat" }))
		.await?;

	reader
		.post(&format!("rooms/{room_id}/join"), &json!({}))
		.await?;

	for body in ["one", "two", "three"] {
		sender.message(&room_id, body).await?;
	}

	let notified = poll_until(NOTIFIED_DEADLINE, async || {
		reader
			.notifications("100", None)
			.await
			.is_ok_and(|(count, _)| count == 3)
	})
	.await;

	notified
		.into_option()
		.ok_or_else(|| err!("the reader was not notified of the three messages"))?;

	assert_eq!(reader.notifications("3", None).await?, (3, None), "exact-size page");

	let (count, next_token) = reader.notifications("2", None).await?;

	assert_eq!(count, 2, "first page");
	assert!(next_token.is_some(), "first page ends early");
	assert_eq!(
		reader
			.notifications("2", next_token.as_deref())
			.await?,
		(1, None),
		"last page"
	);

	Ok(())
}

/// Read one page of notifications: its size and its `next_token`.
#[implement(Client, params = "<'_>")]
async fn notifications(
	&self,
	limit: &str,
	from: Option<&str>,
) -> Result<(usize, Option<String>)> {
	let from = from.map(|from| ("from", from));
	let response: Value = self
		.services
		.client
		.clients
		.default
		.get(self.url("notifications"))
		.bearer_auth(self.token)
		.query(&[("limit", limit)])
		.query(&from.as_slice())
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	let count = response["notifications"]
		.as_array()
		.map(Vec::len)
		.ok_or_else(|| err!("notifications response without a list"))?;

	let next_token = response
		.get("next_token")
		.and_then(Value::as_str)
		.map(ToOwned::to_owned);

	Ok((count, next_token))
}

#[implement(Client, params = "<'_>")]
async fn message(&self, room_id: &RoomId, body: &str) -> Result {
	let response: Value = self
		.services
		.client
		.clients
		.default
		.put(self.url(&format!("rooms/{room_id}/send/m.room.message/{body}")))
		.bearer_auth(self.token)
		.json(&json!({ "msgtype": "m.text", "body": body }))
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	field(&response, "event_id").map(drop)
}
