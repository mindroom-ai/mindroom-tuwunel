#![cfg(test)]

use serde_json::{Value, json};
use tuwunel_core::{
	Result, implement,
	ruma::{
		EventId, RoomId,
		api::{
			OutgoingRequestExt,
			federation::{
				authentication::ServerSignaturesInput,
				event::get_room_state_ids::v1::Request as StateIdsRequest,
			},
		},
		events::StateEventType,
	},
};
use tuwunel_service::Services;

use self::{
	client::{Client, register},
	fixture::boot,
};

mod client;
mod fixture;

const OWNER_TOKEN: &str = "federation-knock-access-owner-access-token";
const KNOCKER_TOKEN: &str = "federation-knock-access-knocker-access-token";

/// A pending knock does not admit a server with no joined member.
///
/// The owner leaves a knock room while another user's knock is pending, so
/// this server, the requesting origin, has no joined member left. A knock is
/// only a request to join, so the room's state stays closed to it.
#[test]
fn pending_knock_does_not_open_room_to_servers() -> Result {
	let options: [&str; 0] = [];

	boot("federation-knock-access", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	register(services, "knockowner", OWNER_TOKEN).await?;

	let knocker_id = register(services, "knocker", KNOCKER_TOKEN).await?;
	let owner = Client { services, base, token: OWNER_TOKEN };
	let knocker = Client { services, base, token: KNOCKER_TOKEN };
	let join_rule = json!({
		"type": "m.room.join_rules",
		"state_key": "",
		"content": { "join_rule": "knock" },
	});

	let room_id = owner
		.create_room(&json!({ "initial_state": [join_rule] }))
		.await?;

	knocker
		.post(&format!("knock/{room_id}"), &json!({}))
		.await?;

	let knock = services
		.state_accessor
		.room_state_get_id(&room_id, &StateEventType::RoomMember, knocker_id.as_str())
		.await?;

	owner
		.post(&format!("rooms/{room_id}/leave"), &json!({}))
		.await?;

	let (status, body) = state_ids(services, base, &room_id, &knock).await?;

	assert_eq!(status, 403, "pending knock: {body}");

	let body: Value = serde_json::from_str(&body)?;

	assert_eq!(body["error"], "M_FORBIDDEN: Server is not in room.");

	Ok(())
}

async fn state_ids(
	services: &Services,
	base: &str,
	room_id: &RoomId,
	event_id: &EventId,
) -> Result<(u16, String)> {
	let server_name = services.globals.server_name().to_owned();
	let auth = ServerSignaturesInput::new(
		server_name.clone(),
		server_name,
		services.server_keys.keypair(),
	);

	let request = StateIdsRequest::new(event_id.to_owned(), room_id.to_owned())
		.try_into_http_request::<Vec<u8>>(base, auth, ())?;

	let response = services
		.client
		.clients
		.default
		.execute(request.try_into()?)
		.await?;

	let status = response.status().as_u16();
	let body = response.text().await?;

	Ok((status, body))
}

/// Post a JSON body to one endpoint path as this user and parse the reply.
///
/// A non-success status is the error, so a caller only ever sees the body of
/// an accepted request.
#[implement(Client, params = "<'_>")]
async fn post(&self, path: &str, body: &Value) -> Result<Value> {
	let response = self
		.services
		.client
		.clients
		.default
		.post(self.url(path))
		.bearer_auth(self.token)
		.json(body)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(response)
}
