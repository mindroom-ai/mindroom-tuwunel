#![cfg(test)]

use std::{net::TcpListener, path::PathBuf, time::Duration};

use axum::{
	Json, Router,
	extract::{Path, State},
	http::StatusCode,
	response::{IntoResponse, Response},
	routing::{get, post},
};
use axum_server::{from_tcp_rustls, tls_rustls::RustlsConfig};
use futures::future::join;
use serde_json::{Value, json};
use tokio::{
	spawn,
	sync::{mpsc, oneshot},
	time::timeout,
};
use tuwunel_core::{
	Err, Result, err,
	matrix::{Event, pdu::into_outgoing_federation},
	pdu::PduBuilder,
	ruma::{
		CanonicalJsonObject, CanonicalJsonValue, EventId, MilliSecondsSinceUnixEpoch,
		OwnedEventId, RoomId, ServerName, UserId,
		events::{StateEventType, room::message::RoomMessageEventContent},
	},
};
use tuwunel_service::Services;

use self::{
	client::{Client, register},
	fixture::boot,
};

mod client;
mod fixture;

const TOKEN: &str = "federation-prev-event-room-access-token";

const CERTIFICATE: &str = "../../nix/pkgs/complement/certificate.crt";

const PRIVATE_KEY: &str = "../../nix/pkgs/complement/private_key.key";

const TIMEOUT: Duration = Duration::from_secs(10);

/// An incoming event cannot name another room's event as a prev event, nor
/// take its state from another room's events.
///
/// The other room's event is already in our timeline, so it is not fetched
/// again; it is still checked for its room, as a fetched prev event is.
/// Otherwise the state before the incoming event would be the other room's.
/// The same holds when a stored prev event as old as the room's first event
/// names the other room's event.
///
/// A peer then sends events it serves no prev event for. For one, it answers
/// `/state_ids` with this room's create and member events and the other room's
/// power levels. For another, whose prev event is the other room's next event,
/// it holds its `/event` answer until that event is in our timeline, then has
/// none: the prev event's room was not checked while it was missing, so it is
/// checked where its state is used.
#[test]
fn events_from_another_room_are_rejected() -> Result {
	let options = ["ip_range_denylist=[]", "allow_invalid_tls_certificates=true"];

	boot("federation-prev-event-room", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let user_id = register(services, "prevevents", TOKEN).await?;
	let client = Client { services, base, token: TOKEN };
	let room_id = client.create_room(&json!({})).await?;
	let other_room_id = client.create_room(&json!({})).await?;
	let other_event_id = services
		.state_accessor
		.room_state_get_id(&other_room_id, &StateEventType::RoomMember, user_id.as_str())
		.await?;

	let (event_id, pdu) =
		sign_message(services, &user_id, &room_id, &other_event_id, None).await?;

	assert_rejected(services, services.globals.server_name(), &room_id, &event_id, pdu).await?;

	let first_ts = services
		.timeline
		.first_pdu_in_room(&room_id)
		.await?
		.origin_server_ts();

	let (prev_id, prev) =
		sign_message(services, &user_id, &room_id, &other_event_id, Some(first_ts)).await?;

	services
		.event_handler
		.handle_incoming_pdu(services.globals.server_name(), &room_id, &prev_id, prev, false)
		.await?;

	let (event_id, pdu) = sign_message(services, &user_id, &room_id, &prev_id, None).await?;

	assert_rejected(services, services.globals.server_name(), &room_id, &event_id, pdu).await?;
	assert!(
		services
			.timeline
			.non_outlier_pdu_exists(&prev_id)
			.await
			.is_err(),
		"the stored prev event reached the timeline"
	);

	from_peer(services, &user_id, &room_id, &other_room_id).await
}

/// Hand over events from a peer that serves none of their prev events.
async fn from_peer(
	services: &Services,
	user_id: &UserId,
	room_id: &RoomId,
	other_room_id: &RoomId,
) -> Result {
	let state_ids = vec![
		state_id(services, room_id, StateEventType::RoomCreate, "").await?,
		state_id(services, room_id, StateEventType::RoomMember, user_id.as_str()).await?,
		state_id(services, other_room_id, StateEventType::RoomPowerLevels, "").await?,
	];

	let (held, held_json) = {
		let builder = PduBuilder::timeline(&RoomMessageEventContent::text_plain("held"));
		let state_lock = services.state.mutex.lock(other_room_id).await;

		services
			.timeline
			.create_hash_and_sign_event(builder, user_id, other_room_id, &state_lock)
			.await?
	};

	let listener = TcpListener::bind(("127.0.0.1", 0))?;
	let peer_name = ServerName::parse(listener.local_addr()?.to_string())
		.map_err(|e| err!("peer server name: {e}"))?;

	// tokio refuses to adopt a blocking socket
	listener.set_nonblocking(true)?;

	let (asked, mut asks) = mpsc::channel(1);
	let peer = Peer {
		held: held.event_id().to_owned(),
		asked,
		state_ids,
	};

	let peer = spawn(serve_peer(listener, peer));

	let unknown = EventId::parse("$federation-prev-event-room-unknown")?;
	let (event_id, pdu) = sign_message(services, user_id, room_id, &unknown, None).await?;

	assert_rejected(services, &peer_name, room_id, &event_id, pdu).await?;

	let (event_id, pdu) = sign_message(services, user_id, room_id, held.event_id(), None).await?;

	let rejected = assert_rejected(services, &peer_name, room_id, &event_id, pdu);
	let store_held = async {
		let release = timeout(TIMEOUT, asks.recv())
			.await
			.map_err(|_| err!("the held event was not fetched"))?
			.ok_or_else(|| err!("the peer stopped"))?;

		let room_version = services
			.state
			.get_room_version(other_room_id)
			.await?;
		let held_json = into_outgoing_federation(held_json, &room_version);

		services
			.event_handler
			.handle_incoming_pdu(
				services.globals.server_name(),
				other_room_id,
				held.event_id(),
				held_json,
				true,
			)
			.await?;

		drop(asks);
		release
			.send(())
			.map_err(|()| err!("the held fetch was dropped"))
	};

	let (rejected, stored) = join(rejected, store_held).await;

	peer.abort();
	stored?;
	rejected?;

	assert!(
		services
			.state
			.pdu_shortstatehash(held.event_id())
			.await
			.is_ok(),
		"the held event has no state in the other room"
	);

	Ok(())
}

async fn state_id(
	services: &Services,
	room_id: &RoomId,
	event_type: StateEventType,
	state_key: &str,
) -> Result<OwnedEventId> {
	services
		.state_accessor
		.room_state_get_id(room_id, &event_type, state_key)
		.await
}

/// Sign a message for `room_id` whose only prev event is `prev_event_id`.
async fn sign_message(
	services: &Services,
	user_id: &UserId,
	room_id: &RoomId,
	prev_event_id: &EventId,
	timestamp: Option<MilliSecondsSinceUnixEpoch>,
) -> Result<(OwnedEventId, CanonicalJsonObject)> {
	let room_version = services.state.get_room_version(room_id).await?;
	let builder = PduBuilder {
		timestamp,
		..PduBuilder::timeline(&RoomMessageEventContent::text_plain("hello"))
	};

	let (_, mut pdu) = {
		let state_lock = services.state.mutex.lock(room_id).await;

		services
			.timeline
			.create_hash_and_sign_event(builder, user_id, room_id, &state_lock)
			.await?
	};

	let prev_events = vec![CanonicalJsonValue::String(prev_event_id.into())];

	pdu.insert("prev_events".into(), CanonicalJsonValue::Array(prev_events));

	let event_id = services
		.server_keys
		.gen_id_hash_and_sign_event(&mut pdu, &room_version)?;

	Ok((event_id, into_outgoing_federation(pdu, &room_version)))
}

/// Hand `pdu` over from `origin` as an incoming timeline event, which must be
/// rejected for another room's event and kept out of the timeline.
async fn assert_rejected(
	services: &Services,
	origin: &ServerName,
	room_id: &RoomId,
	event_id: &EventId,
	pdu: CanonicalJsonObject,
) -> Result {
	let result = services
		.event_handler
		.handle_incoming_pdu(origin, room_id, event_id, pdu, true)
		.await;

	let Err(error) = result else {
		return Err!("an event resting on another room's event was accepted");
	};

	assert!(error.to_string().contains("wrong room"), "rejected for another reason: {error}");
	assert!(
		services
			.timeline
			.non_outlier_pdu_exists(event_id)
			.await
			.is_err(),
		"the rejected event reached the timeline"
	);

	Ok(())
}

/// The sending server's federation API, as far as these events need it.
#[derive(Clone)]
struct Peer {
	/// Answered only once the test has stored it, and then as unknown.
	held: OwnedEventId,

	/// Hands the test the release for the held answer.
	asked: mpsc::Sender<oneshot::Sender<()>>,

	/// The `/state_ids` answer for any event.
	state_ids: Vec<OwnedEventId>,
}

async fn serve_peer(listener: TcpListener, peer: Peer) -> Result {
	let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
	let config =
		RustlsConfig::from_pem_file(manifest.join(CERTIFICATE), manifest.join(PRIVATE_KEY))
			.await?;

	let app = Router::new()
		.route("/_matrix/federation/v1/event/{event_id}", get(event))
		.route("/_matrix/federation/v1/get_missing_events/{room_id}", post(missing_events))
		.route("/_matrix/federation/v1/state_ids/{room_id}", get(state_ids))
		.with_state(peer);

	from_tcp_rustls(listener, config)?
		.serve(app.into_make_service())
		.await?;

	Ok(())
}

async fn event(State(peer): State<Peer>, Path(event_id): Path<OwnedEventId>) -> Response {
	if event_id == peer.held {
		let (release, released) = oneshot::channel();

		if peer.asked.send(release).await.is_ok() {
			released.await.ok();
		}
	}

	let error = json!({"errcode": "M_NOT_FOUND", "error": "Event not found."});

	(StatusCode::NOT_FOUND, Json(error)).into_response()
}

async fn missing_events() -> Json<Value> { Json(json!({"events": []})) }

async fn state_ids(State(peer): State<Peer>) -> Json<Value> {
	Json(json!({"auth_chain_ids": [], "pdu_ids": peer.state_ids}))
}
