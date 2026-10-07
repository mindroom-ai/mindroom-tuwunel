#![cfg(test)]

use serde_json::{Value, json, value::to_raw_value};
use tuwunel_core::{Result, matrix::pdu::MAX_SERVED_PDU_BYTES, ruma::api::error::ErrorKind};
use tuwunel_service::Services;

use self::{
	client::{Client, register},
	fixture::boot,
};

mod client;
mod fixture;

const TOKEN: &str = "federation-inbound-parse-access-token";

/// Federation input is checked before it is parsed into JSON trees.
///
/// One server runs both checks, since a test binary boots only one.
#[test]
fn federation_input_is_checked_before_parsing() -> Result {
	boot("federation-inbound-parse", ["trusted_servers=[]"], exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	body_waits_for_the_origin_key(services, base).await?;
	oversized_pdu_is_refused_unparsed(services, base).await
}

/// A federation request's body is parsed only once its origin's key is known.
///
/// The origin is a loopback address the key fetch refuses, and the body is not
/// JSON. The request fails on the key, before anything reads the body.
async fn body_waits_for_the_origin_key(services: &Services, base: &str) -> Result {
	let authorization = format!(
		r#"X-Matrix origin="127.0.0.1:9",destination="{}",key="ed25519:a",sig="AAAA""#,
		services.globals.server_name(),
	);

	let response = services
		.client
		.clients
		.default
		.put(format!("{base}/_matrix/federation/v1/send/inbound-parse"))
		.header("Authorization", authorization)
		.body("not json")
		.send()
		.await?;

	let status = response.status().as_u16();
	let body: Value = response.json().await?;

	assert_eq!(status, 403, "{body}");
	assert_eq!(body["errcode"], "M_FORBIDDEN");

	Ok(())
}

/// An incoming PDU over the served PDU limit is refused before it is parsed.
async fn oversized_pdu_is_refused_unparsed(services: &Services, base: &str) -> Result {
	register(services, "inboundparse", TOKEN).await?;

	let client = Client { services, base, token: TOKEN };
	let room_id = client
		.create_room(&json!({ "preset": "private_chat" }))
		.await?;

	let pdu = to_raw_value(&json!({
		"room_id": room_id,
		"type": "m.room.message",
		"content": { "body": "x".repeat(MAX_SERVED_PDU_BYTES) },
	}))?;

	let parsed = services
		.event_handler
		.parse_incoming_pdu(&pdu)
		.await;

	assert!(
		parsed.is_err_and(|e| e.kind() == ErrorKind::TooLarge),
		"an oversized PDU was parsed",
	);

	Ok(())
}
