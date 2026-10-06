#![cfg(test)]

use serde_json::{Value, json, value::to_raw_value};
use tuwunel_core::{
	Result,
	ruma::{
		MilliSecondsSinceUnixEpoch, OwnedUserId, RoomId, TransactionId, UserId,
		api::{
			OutgoingRequestExt,
			federation::{
				authentication::ServerSignaturesInput,
				transactions::send_transaction_message::v1::Request as SendRequest,
			},
		},
		serde::Raw,
	},
	utils::ReadyExt,
};
use tuwunel_service::Services;

use self::{
	client::{Client, field, register},
	fixture::boot,
};

mod client;
mod fixture;

const MEMBER_TOKEN: &str = "federation-receipt-edu-member-token";
const OUTSIDER_TOKEN: &str = "federation-receipt-edu-outsider-token";

/// A federated read receipt is stored only for a user joined to the room, at
/// one of its timeline events, and for no thread other than one rooted at
/// such an event.
#[test]
fn federated_receipts_need_a_joined_user_and_a_room_event() -> Result {
	let options: [&str; 0] = [];

	boot("federation-receipt-edu", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let member = register(services, "receiptmember", MEMBER_TOKEN).await?;
	let outsider = register(services, "receiptoutsider", OUTSIDER_TOKEN).await?;
	let member_client = Client { services, base, token: MEMBER_TOKEN };
	let room = member_client.create_room(&json!({})).await?;
	let event = message(&member_client, &room).await?;
	let unknown = "$unknown:localhost";

	let edus = [
		receipt(&room, &outsider, &event, None),
		receipt(&room, &member, unknown, Some("main")),
		receipt(&room, &member, &event, Some(unknown)),
		receipt(&room, &member, &event, Some("custom")),
		receipt(&room, &member, &event, None),
	];

	assert_eq!(send_transaction(services, base, &edus).await?, 200);
	assert_eq!(receipt_users(services, &room).await, [member]);

	Ok(())
}

async fn message(client: &Client<'_>, room: &RoomId) -> Result<String> {
	let response: Value = client
		.services
		.client
		.clients
		.default
		.put(client.url(&format!("rooms/{room}/send/m.room.message/receipt-edu")))
		.bearer_auth(client.token)
		.json(&json!({"msgtype": "m.text", "body": "federated receipt check"}))
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(field(&response, "event_id")?.to_owned())
}

/// An `m.receipt` EDU carrying one user's `m.read` receipt for `event`.
fn receipt(room: &RoomId, user: &UserId, event: &str, thread: Option<&str>) -> Value {
	let data =
		thread.map_or_else(|| json!({"ts": 1}), |thread| json!({"ts": 1, "thread_id": thread}));

	json!({
		"edu_type": "m.receipt",
		"content": {
			room.as_str(): {
				"m.read": {
					user.as_str(): { "data": data, "event_ids": [event] },
				},
			},
		},
	})
}

/// Sends `edus` in one federation transaction signed by this server and
/// returns the response status.
async fn send_transaction(services: &Services, base: &str, edus: &[Value]) -> Result<u16> {
	let server_name = services.globals.server_name().to_owned();
	let auth = ServerSignaturesInput::new(
		server_name.clone(),
		server_name.clone(),
		services.server_keys.keypair(),
	);

	let mut request =
		SendRequest::new(TransactionId::new(), server_name, MilliSecondsSinceUnixEpoch::now());

	request.edus = edus
		.iter()
		.map(|edu| Ok(Raw::from_json(to_raw_value(edu)?)))
		.collect::<Result<_>>()?;

	let request = request.try_into_http_request::<Vec<u8>>(base, auth, ())?;
	let response = services
		.client
		.clients
		.default
		.execute(request.try_into()?)
		.await?;

	Ok(response.status().as_u16())
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
