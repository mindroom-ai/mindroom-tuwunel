#![cfg(test)]

use std::{net::TcpListener, path::PathBuf};

use axum::{
	Json, Router,
	extract::State,
	http::StatusCode,
	response::{IntoResponse, Response},
	routing::{get, put},
};
use axum_server::{from_tcp_rustls, tls_rustls::RustlsConfig};
use serde_json::{Value, json};
use tokio::{
	spawn,
	sync::{mpsc, watch},
};
use tuwunel_core::{
	Result, err,
	ruma::{OwnedUserId, RoomVersionId, ServerName, UserId},
};
use tuwunel_service::{Services, membership::Join};

use self::{
	client::{Client, register},
	fixture::boot,
};

mod client;
mod fixture;

const ALICE_TOKEN: &str = "restricted-join-fallback-alice-token";

const BOB_TOKEN: &str = "restricted-join-fallback-bob-token";

const CERTIFICATE: &str = "../../nix/pkgs/complement/certificate.crt";

const PRIVATE_KEY: &str = "../../nix/pkgs/complement/private_key.key";

/// A restricted join refused locally is not authorised by one of our users
/// through a remote template.
///
/// `@bob` is not in the allowed room, so the local join fails and the join
/// asks the `via` server for a template. A template naming `@alice`, who is
/// joined and may invite, would have our signature authorise the join, so it
/// is never sent. A template naming the remote server's own user is still
/// signed and sent.
#[test]
fn restricted_join_fallback_refuses_a_local_authoriser() -> Result {
	let options = ["ip_range_denylist=[]", "allow_invalid_tls_certificates=true"];

	boot("restricted-join-fallback", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let alice_id = register(services, "alice", ALICE_TOKEN).await?;
	let bob_id = register(services, "bob", BOB_TOKEN).await?;
	let alice = Client { services, base, token: ALICE_TOKEN };
	let allowed = alice
		.create_room(&json!({ "preset": "private_chat" }))
		.await?;

	let room_id = alice
		.create_room(&json!({
			"preset": "private_chat",
			"initial_state": [{
				"type": "m.room.join_rules",
				"state_key": "",
				"content": {
					"join_rule": "restricted",
					"allow": [{ "type": "m.room_membership", "room_id": allowed }],
				},
			}],
		}))
		.await?;

	let listener = TcpListener::bind(("127.0.0.1", 0))?;
	let peer_name = ServerName::parse(listener.local_addr()?.to_string())
		.map_err(|e| err!("peer server name: {e}"))?;

	// tokio refuses to adopt a blocking socket
	listener.set_nonblocking(true)?;

	let carol_id = UserId::parse_with_server_name("carol", &peer_name)?;
	let (authoriser, template_authoriser) = watch::channel(alice_id);
	let (sent, mut joins) = mpsc::unbounded_channel();
	let peer = Peer {
		room_version: services.state.get_room_version(&room_id).await?,
		authoriser: template_authoriser,
		sent,
	};

	let server = spawn(serve_peer(listener, peer));
	let servers = [peer_name];
	let join = || {
		services.membership.join(Join {
			sender_user: &bob_id,
			room_id: &room_id,
			orig_room_id: None,
			reason: None,
			servers: &servers,
			is_appservice: false,
			extra_content: None,
		})
	};

	let local = join().await;
	let local_sent = joins.try_recv().ok();

	authoriser.send_replace(carol_id.clone());

	let remote = join().await;
	let remote_sent = joins.try_recv().ok();

	server.abort();

	assert!(local.is_err(), "the join was not refused");
	assert_eq!(local_sent, None, "a join naming a local authoriser was sent");
	assert!(remote.is_err(), "the join succeeded though the peer refused it");
	assert_eq!(remote_sent, Some(json!(carol_id)), "no join named the remote authoriser");

	Ok(())
}

/// The `via` server, which answers `make_join` with a restricted join template
/// and refuses every join it is sent.
#[derive(Clone)]
struct Peer {
	room_version: RoomVersionId,

	/// The authoriser its template names.
	authoriser: watch::Receiver<OwnedUserId>,

	/// Hands the test the authoriser of each join it is sent.
	sent: mpsc::UnboundedSender<Value>,
}

async fn serve_peer(listener: TcpListener, peer: Peer) -> Result {
	let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
	let config =
		RustlsConfig::from_pem_file(manifest.join(CERTIFICATE), manifest.join(PRIVATE_KEY))
			.await?;

	let app = Router::new()
		.route("/_matrix/federation/v1/make_join/{room_id}/{user_id}", get(make_join))
		.route("/_matrix/federation/v2/send_join/{room_id}/{event_id}", put(send_join))
		.with_state(peer);

	from_tcp_rustls(listener, config)?
		.serve(app.into_make_service())
		.await?;

	Ok(())
}

async fn make_join(State(peer): State<Peer>) -> Json<Value> {
	let authoriser = peer.authoriser.borrow().clone();

	Json(json!({
		"room_version": peer.room_version,
		"event": {
			"type": "m.room.member",
			"content": { "membership": "join", "join_authorised_via_users_server": authoriser },
			"prev_events": [],
			"auth_events": [],
			"depth": 1,
		},
	}))
}

async fn send_join(State(peer): State<Peer>, Json(pdu): Json<Value>) -> Response {
	peer.sent
		.send(pdu["content"]["join_authorised_via_users_server"].clone())
		.ok();

	let error = json!({ "errcode": "M_FORBIDDEN", "error": "Join refused." });

	(StatusCode::FORBIDDEN, Json(error)).into_response()
}
