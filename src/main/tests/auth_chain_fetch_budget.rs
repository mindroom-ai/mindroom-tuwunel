#![cfg(test)]

use std::{
	collections::HashSet,
	net::TcpListener,
	path::PathBuf,
	sync::{Arc, Mutex},
};

use axum::{
	Json, Router,
	extract::{Path, State},
	http::StatusCode,
	response::IntoResponse,
	routing::get,
};
use axum_server::{from_tcp_rustls, tls_rustls::RustlsConfig};
use serde_json::json;
use tokio::spawn;
use tuwunel_core::{
	Result, err,
	matrix::pdu::into_outgoing_federation,
	pdu::PduBuilder,
	ruma::{
		CanonicalJsonValue, OwnedEventId, ServerName,
		events::room::message::RoomMessageEventContent,
	},
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

/// The event ids the peer was asked for.
type Requested = Arc<Mutex<HashSet<OwnedEventId>>>;

/// The walks fetching an incoming event's missing auth events share one budget
/// of `max_fetch_prev_events` fetches, rather than each walk having its own.
#[test]
fn auth_event_walks_share_one_fetch_budget() -> Result {
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

	let builder = PduBuilder::timeline(&RoomMessageEventContent::text_plain("hello"));
	let (_, mut pdu) = {
		let state_lock = services.state.mutex.lock(&room_id).await;

		services
			.timeline
			.create_hash_and_sign_event(builder, &user_id, &room_id, &state_lock)
			.await?
	};

	let auth_events = (0..5)
		.map(|i| CanonicalJsonValue::String(format!("$auth-chain-fetch-budget-{i}")))
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

	let requested = Requested::default();
	let peer = spawn(serve_peer(listener, requested.clone()));

	// Without its auth events the event fails its auth check.
	services
		.event_handler
		.handle_incoming_pdu(&peer_name, &room_id, &event_id, pdu, true)
		.await
		.ok();

	peer.abort();

	let requested = requested.lock().expect("not poisoned").len();

	assert_eq!(requested, 2, "each auth event walk fetched on its own budget");

	Ok(())
}

/// A peer that has none of the events it is asked for.
async fn serve_peer(listener: TcpListener, requested: Requested) -> Result {
	let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
	let config =
		RustlsConfig::from_pem_file(manifest.join(CERTIFICATE), manifest.join(PRIVATE_KEY))
			.await?;

	let app = Router::new()
		.route("/_matrix/federation/v1/event/{event_id}", get(event))
		.with_state(requested);

	from_tcp_rustls(listener, config)?
		.serve(app.into_make_service())
		.await?;

	Ok(())
}

async fn event(
	State(requested): State<Requested>,
	Path(event_id): Path<OwnedEventId>,
) -> impl IntoResponse {
	requested
		.lock()
		.expect("not poisoned")
		.insert(event_id);

	let error = json!({"errcode": "M_NOT_FOUND", "error": "Event not found."});

	(StatusCode::NOT_FOUND, Json(error))
}
