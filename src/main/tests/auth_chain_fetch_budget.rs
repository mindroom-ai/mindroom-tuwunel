#![cfg(test)]

use std::{collections::HashMap, net::TcpListener, path::PathBuf, sync::Arc};

use axum::{
	Json, Router,
	extract::{Path, State},
	http::StatusCode,
	response::{IntoResponse, Response},
	routing::get,
};
use axum_server::{from_tcp_rustls, tls_rustls::RustlsConfig};
use futures::StreamExt;
use serde_json::json;
use tokio::spawn;
use tuwunel_core::{
	Result, err,
	matrix::{Event, pdu::into_outgoing_federation},
	pdu::PduBuilder,
	ruma::{
		CanonicalJsonObject, CanonicalJsonValue, OwnedEventId, ServerName,
		events::room::message::RoomMessageEventContent,
	},
	utils::stream::IterStream,
};
use tuwunel_service::Services;

use self::{
	client::{Client, register},
	fixture::boot,
};

mod client;
mod fixture;

const TOKEN: &str = "auth-chain-fetch-budget-access-token";

const CERTIFICATE: &str = "../../nix/pkgs/complement/certificate.crt";

const PRIVATE_KEY: &str = "../../nix/pkgs/complement/private_key.key";

/// The events the peer serves, by event id.
type Served = Arc<HashMap<OwnedEventId, CanonicalJsonObject>>;

/// The walks fetching an incoming event's missing auth events share one limit
/// of `max_fetch_prev_events` times 64 KiB on the events they fetch, rather
/// than each walk having its own.
#[test]
fn auth_event_walks_share_one_size_limit() -> Result {
	let options = [
		"ip_range_denylist=[]",
		"allow_invalid_tls_certificates=true",
		"max_fetch_prev_events=2",
	];

	boot("auth-chain-fetch-budget", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let user_id = register(services, "authbudget", TOKEN).await?;
	let client = Client { services, base, token: TOKEN };
	let room_id = client.create_room(&json!({})).await?;
	let room_version = services.state.get_room_version(&room_id).await?;
	let state_lock = services.state.mutex.lock(&room_id).await;

	// Five events of about 38 KB as the auth events of an incoming event: two
	// times 64 KiB holds three of them.
	let mut served = HashMap::new();
	for i in 0..5 {
		let body = format!("{i}{}", "x".repeat(38_000));
		let builder = PduBuilder::timeline(&RoomMessageEventContent::text_plain(body));
		let (pdu, json) = services
			.timeline
			.create_hash_and_sign_event(builder, &user_id, &room_id, &state_lock)
			.await?;

		served.insert(pdu.event_id().to_owned(), into_outgoing_federation(json, &room_version));
	}

	let builder = PduBuilder::timeline(&RoomMessageEventContent::text_plain("hello"));
	let (_, mut pdu) = services
		.timeline
		.create_hash_and_sign_event(builder, &user_id, &room_id, &state_lock)
		.await?;

	drop(state_lock);

	let auth_events = served
		.keys()
		.map(|event_id| CanonicalJsonValue::String(event_id.to_string()))
		.collect();

	pdu.insert("auth_events".into(), CanonicalJsonValue::Array(auth_events));

	let event_id = services
		.server_keys
		.gen_id_hash_and_sign_event(&mut pdu, &room_version)?;
	let pdu = into_outgoing_federation(pdu, &room_version);

	let listener = TcpListener::bind(("127.0.0.1", 0))?;
	let peer_name = ServerName::parse(listener.local_addr()?.to_string())
		.map_err(|e| err!("peer server name: {e}"))?;

	// tokio refuses to adopt a blocking socket
	listener.set_nonblocking(true)?;

	let served = Served::new(served);
	let peer = spawn(serve_peer(listener, served.clone()));

	// Messages cannot be auth events, so the event fails its auth check.
	services
		.event_handler
		.handle_incoming_pdu(&peer_name, &room_id, &event_id, pdu, true)
		.await
		.ok();

	peer.abort();

	let stored = served
		.keys()
		.stream()
		.filter(|&event_id| services.timeline.pdu_exists(event_id))
		.count()
		.await;

	assert_eq!(stored, 3, "the auth event walks did not share one size limit");

	Ok(())
}

async fn serve_peer(listener: TcpListener, served: Served) -> Result {
	let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
	let config =
		RustlsConfig::from_pem_file(manifest.join(CERTIFICATE), manifest.join(PRIVATE_KEY))
			.await?;

	let app = Router::new()
		.route("/_matrix/federation/v1/event/{event_id}", get(event))
		.with_state(served);

	from_tcp_rustls(listener, config)?
		.serve(app.into_make_service())
		.await?;

	Ok(())
}

async fn event(State(served): State<Served>, Path(event_id): Path<OwnedEventId>) -> Response {
	let Some(pdu) = served.get(&event_id) else {
		let error = json!({"errcode": "M_NOT_FOUND", "error": "Event not found."});

		return (StatusCode::NOT_FOUND, Json(error)).into_response();
	};

	Json(json!({"origin": "peer.test", "origin_server_ts": 0, "pdus": [pdu]})).into_response()
}
