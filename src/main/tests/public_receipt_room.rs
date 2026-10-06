#![cfg(test)]

mod client;

use std::net::TcpListener;

use futures::future::join;
use serde_json::{Value, json};
use tuwunel::{Args, Runtime, Server, async_run, async_start, async_stop};
use tuwunel_core::{
	Result,
	ruma::{EventId, OwnedEventId, OwnedUserId, RoomId, event_id},
	utils::ReadyExt,
};
use tuwunel_service::Services;

use self::client::{Client, field, register, wait_until_ready};

const MEMBER_TOKEN: &str = "public-receipt-room-member-token";
const OUTSIDER_TOKEN: &str = "public-receipt-room-outsider-token";

#[test]
fn public_receipts_require_membership_and_a_room_event() -> Result {
	let listener = TcpListener::bind(("127.0.0.1", 0))?;
	let port = listener.local_addr()?.port();
	let args = Args::default_test(&["fresh", "cleanup"])
		.with_option("address=[\"127.0.0.1\"]")
		.with_option(format!("port={port}"))
		.with_option("listening=true")
		.with_option("allow_local_presence=false")
		.with_option("allow_outgoing_presence=false");

	let runtime = Runtime::new(Some(&args))?;
	let server = Server::new(Some(&args), Some(&runtime))?;
	let result = runtime.block_on(async {
		let services = async_start(&server).await?;
		let base = format!("http://127.0.0.1:{port}");

		drop(listener);

		let exercise = async {
			let outcome = public_receipts(&services, &base).await;
			let shutdown = server.server.shutdown();

			outcome.and(shutdown)
		};

		let (run, outcome) = join(async_run(&server), exercise).await;

		drop(services);
		async_stop(&server).await?;
		run.and(outcome)
	});

	drop(runtime);
	result
}

async fn public_receipts(services: &Services, base: &str) -> Result {
	wait_until_ready(services, base).await?;
	let member = register(services, "receiptmember", MEMBER_TOKEN).await?;
	register(services, "receiptoutsider", OUTSIDER_TOKEN).await?;
	let member_client = Client { services, base, token: MEMBER_TOKEN };
	let outsider_client = Client { services, base, token: OUTSIDER_TOKEN };
	let room = member_client.create_room(&json!({})).await?;
	let event = message(&member_client, &room).await?;

	assert_receipt_status(&outsider_client, &room, &event, None, 403).await?;
	assert!(receipt_users(services, &room).await.is_empty());

	let unknown = event_id!("$unknown:localhost");
	assert_receipt_status(&member_client, &room, unknown, Some(unknown), 404).await?;
	assert_receipt_status(&member_client, &room, unknown, None, 404).await?;
	assert!(receipt_users(services, &room).await.is_empty());

	assert_receipt_status(&member_client, &room, &event, None, 200).await?;
	assert_eq!(receipt_users(services, &room).await, [member]);
	Ok(())
}

async fn message(client: &Client<'_>, room: &RoomId) -> Result<OwnedEventId> {
	let response: Value = client
		.services
		.client
		.clients
		.default
		.put(client.url(&format!("rooms/{room}/send/m.room.message/receipt-room")))
		.bearer_auth(client.token)
		.json(&json!({"msgtype": "m.text", "body": "public receipt room check"}))
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(field(&response, "event_id")?.try_into()?)
}

/// Posts an `m.read` receipt for `event` through both client endpoints that
/// publish one, asserting each answers with `status`.
async fn assert_receipt_status(
	client: &Client<'_>,
	room: &RoomId,
	event: &EventId,
	thread: Option<&EventId>,
	status: u16,
) -> Result {
	let body = thread.map_or_else(|| json!({}), |thread| json!({"thread_id": thread}));
	let receipt = client
		.services
		.client
		.clients
		.default
		.post(client.url(&format!("rooms/{room}/receipt/m.read/{event}")))
		.bearer_auth(client.token)
		.json(&body)
		.send()
		.await?;

	assert_eq!(receipt.status().as_u16(), status);

	if thread.is_some() {
		return Ok(());
	}

	let markers = client
		.services
		.client
		.clients
		.default
		.post(client.url(&format!("rooms/{room}/read_markers")))
		.bearer_auth(client.token)
		.json(&json!({"m.read": event}))
		.send()
		.await?;

	assert_eq!(markers.status().as_u16(), status);
	Ok(())
}

async fn receipt_users(services: &Services, room: &RoomId) -> Vec<OwnedUserId> {
	services
		.read_receipt
		.readreceipts_since(room, 0, None)
		.ready_fold(Vec::new(), |mut users, (user, ..)| {
			users.push(user.to_owned());
			users
		})
		.await
}
