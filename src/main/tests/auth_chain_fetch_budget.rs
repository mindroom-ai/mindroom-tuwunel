#![cfg(test)]

use std::{collections::HashMap, net::TcpListener, path::PathBuf, sync::Arc};

use axum::{
	Json, Router,
	extract::{Path, State},
	http::StatusCode,
	response::{IntoResponse, Response},
	routing::{get, post},
};
use axum_server::{from_tcp_rustls, tls_rustls::RustlsConfig};
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::spawn;
use tuwunel_core::{
	Result, err,
	matrix::{Event, pdu::into_outgoing_federation},
	pdu::PduBuilder,
	ruma::{
		CanonicalJsonObject, CanonicalJsonValue, EventId, OwnedEventId, RoomId, ServerName,
		UserId, events::room::message::RoomMessageEventContent,
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

const OPTIONS: [&str; 4] = [
	"ip_range_denylist=[]",
	"allow_invalid_tls_certificates=true",
	"max_fetch_prev_events=2",
	"fetch_prev_wait_ms=0",
];

/// Events by event id.
type Served = HashMap<OwnedEventId, CanonicalJsonObject>;

/// The sending server's federation API, as far as these events need it.
#[derive(Default)]
struct Peer {
	/// The `/event` answers.
	events: Served,

	/// The `/get_missing_events` answer.
	missing: Vec<CanonicalJsonObject>,
}

/// The fetches an incoming event starts stay within `max_fetch_prev_events`.
#[test]
fn incoming_event_fetches_are_bounded() -> Result {
	boot("auth-chain-fetch-budget", OPTIONS, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let user_id = register(services, "authbudget", TOKEN).await?;
	let client = Client { services, base, token: TOKEN };

	auth_event_walks_share_one_size_limit(&client, &user_id).await?;
	prev_event_walks_share_one_size_limit(&client, &user_id).await?;
	prev_event_walk_counts_queued_fetches(&client, &user_id).await?;
	missing_events_beyond_the_limit_are_dropped(&client, &user_id).await
}

/// The walks fetching an incoming event's missing auth events share one limit
/// of `max_fetch_prev_events` times 64 KiB on the events they fetch, rather
/// than each walk having its own.
async fn auth_event_walks_share_one_size_limit(client: &Client<'_>, user_id: &UserId) -> Result {
	let services = client.services;
	let room_id = client.create_room(&json!({})).await?;

	// Five events of about 38 KB as the auth events of an incoming event: two
	// times 64 KiB holds three of them.
	let events = messages(services, user_id, &room_id, 5, 38_000).await?;
	let ids: Vec<_> = events.keys().cloned().collect();
	let (event_id, pdu) =
		sign_message(services, user_id, &room_id, "hello", Some(("auth_events", &ids))).await?;

	// Messages cannot be auth events, so the event fails its auth check.
	deliver(services, &room_id, &event_id, pdu, Peer { events, ..Peer::default() }).await?;

	let stored = count_stored(services, &ids).await;
	assert_eq!(stored, 3, "the auth event walks did not share one size limit");

	Ok(())
}

/// The walks fetching an incoming event's missing prev events, one auth chain
/// fetch for each, share that limit as well.
async fn prev_event_walks_share_one_size_limit(client: &Client<'_>, user_id: &UserId) -> Result {
	let services = client.services;
	let room_id = client.create_room(&json!({})).await?;

	// Five events of about 38 KB as the prev events of an incoming event.
	let events = messages(services, user_id, &room_id, 5, 38_000).await?;
	let ids: Vec<_> = events.keys().cloned().collect();
	let (event_id, pdu) =
		sign_message(services, user_id, &room_id, "hello", Some(("prev_events", &ids))).await?;

	deliver(services, &room_id, &event_id, pdu, Peer { events, ..Peer::default() }).await?;

	let stored = count_stored(services, &ids).await;
	assert_eq!(stored, 3, "the prev event walks did not share one size limit");

	Ok(())
}

/// The prev event walk counts the fetches it queues against
/// `max_fetch_prev_events`, as they all run at once, not only the events it
/// has already walked.
async fn prev_event_walk_counts_queued_fetches(client: &Client<'_>, user_id: &UserId) -> Result {
	let services = client.services;
	let room_id = client.create_room(&json!({})).await?;

	// The incoming event's prev event names twenty prev events of its own.
	let mut events = messages(services, user_id, &room_id, 20, 0).await?;
	let ids: Vec<_> = events.keys().cloned().collect();
	let (prev_id, prev) =
		sign_message(services, user_id, &room_id, "prev", Some(("prev_events", &ids))).await?;

	events.insert(prev_id.clone(), prev);

	let prev_ids = [prev_id];
	let (event_id, pdu) =
		sign_message(services, user_id, &room_id, "hello", Some(("prev_events", &prev_ids)))
			.await?;

	deliver(services, &room_id, &event_id, pdu, Peer { events, ..Peer::default() }).await?;

	// The walk queues the prev event and two of the twenty.
	let stored = count_stored(services, &ids).await;
	assert_eq!(stored, 2, "the prev event walk queued fetches past its limit");

	Ok(())
}

/// Only as many events of a `/get_missing_events` answer are stored as were
/// asked for.
async fn missing_events_beyond_the_limit_are_dropped(
	client: &Client<'_>,
	user_id: &UserId,
) -> Result {
	let services = client.services;
	let room_id = client.create_room(&json!({})).await?;

	// Fifteen events answer a request for ten.
	let events = messages(services, user_id, &room_id, 15, 0).await?;
	let ids: Vec<_> = events.keys().cloned().collect();
	let missing = events.into_values().collect();

	let unknown = [EventId::parse("$auth-chain-fetch-budget-unknown")?];
	let (event_id, pdu) =
		sign_message(services, user_id, &room_id, "hello", Some(("prev_events", &unknown)))
			.await?;

	deliver(services, &room_id, &event_id, pdu, Peer { missing, ..Peer::default() }).await?;

	let stored = count_stored(services, &ids).await;
	assert_eq!(stored, 10, "more missing events were stored than asked for");

	Ok(())
}

/// Sign `count` messages padded by `pad` bytes that only the peer serves.
async fn messages(
	services: &Services,
	user_id: &UserId,
	room_id: &RoomId,
	count: usize,
	pad: usize,
) -> Result<Served> {
	let mut events = Served::new();
	for i in 0..count {
		let body = format!("{i}{}", "x".repeat(pad));
		let (event_id, pdu) = sign_message(services, user_id, room_id, &body, None).await?;

		events.insert(event_id, pdu);
	}

	Ok(events)
}

/// Sign a message, naming `refs` as its auth or prev events when given.
async fn sign_message(
	services: &Services,
	user_id: &UserId,
	room_id: &RoomId,
	body: &str,
	refs: Option<(&str, &[OwnedEventId])>,
) -> Result<(OwnedEventId, CanonicalJsonObject)> {
	let room_version = services.state.get_room_version(room_id).await?;
	let builder = PduBuilder::timeline(&RoomMessageEventContent::text_plain(body));
	let (pdu, mut json) = {
		let state_lock = services.state.mutex.lock(room_id).await;

		services
			.timeline
			.create_hash_and_sign_event(builder, user_id, room_id, &state_lock)
			.await?
	};

	let Some((field, ids)) = refs else {
		return Ok((pdu.event_id().to_owned(), into_outgoing_federation(json, &room_version)));
	};

	let ids = ids
		.iter()
		.map(|id| CanonicalJsonValue::String(id.to_string()))
		.collect();

	json.insert(field.into(), CanonicalJsonValue::Array(ids));
	let event_id = services
		.server_keys
		.gen_id_hash_and_sign_event(&mut json, &room_version)?;

	Ok((event_id, into_outgoing_federation(json, &room_version)))
}

/// Hand `pdu` over as an incoming event from a server answering as `peer`.
async fn deliver(
	services: &Services,
	room_id: &RoomId,
	event_id: &EventId,
	pdu: CanonicalJsonObject,
	peer: Peer,
) -> Result {
	let listener = TcpListener::bind(("127.0.0.1", 0))?;
	let peer_name = ServerName::parse(listener.local_addr()?.to_string())
		.map_err(|e| err!("peer server name: {e}"))?;

	// tokio refuses to adopt a blocking socket
	listener.set_nonblocking(true)?;

	let peer = spawn(serve_peer(listener, peer));

	// Only the events stored along the way matter, not the incoming event.
	services
		.event_handler
		.handle_incoming_pdu(&peer_name, room_id, event_id, pdu, true)
		.await
		.ok();

	peer.abort();

	Ok(())
}

async fn count_stored(services: &Services, ids: &[OwnedEventId]) -> usize {
	ids.iter()
		.stream()
		.filter(|&event_id| services.timeline.pdu_exists(event_id))
		.count()
		.await
}

async fn serve_peer(listener: TcpListener, peer: Peer) -> Result {
	let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
	let config =
		RustlsConfig::from_pem_file(manifest.join(CERTIFICATE), manifest.join(PRIVATE_KEY))
			.await?;

	let app = Router::new()
		.route("/_matrix/federation/v1/event/{event_id}", get(event))
		.route("/_matrix/federation/v1/get_missing_events/{room_id}", post(missing_events))
		.with_state(Arc::new(peer));

	from_tcp_rustls(listener, config)?
		.serve(app.into_make_service())
		.await?;

	Ok(())
}

async fn event(State(peer): State<Arc<Peer>>, Path(event_id): Path<OwnedEventId>) -> Response {
	let Some(pdu) = peer.events.get(&event_id) else {
		let error = json!({"errcode": "M_NOT_FOUND", "error": "Event not found."});

		return (StatusCode::NOT_FOUND, Json(error)).into_response();
	};

	Json(json!({"origin": "peer.test", "origin_server_ts": 0, "pdus": [pdu]})).into_response()
}

async fn missing_events(State(peer): State<Arc<Peer>>) -> Json<Value> {
	Json(json!({"events": peer.missing}))
}
