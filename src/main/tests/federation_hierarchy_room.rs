#![cfg(test)]

use std::{net::TcpListener, path::PathBuf};

use axum::{Json, Router, routing::any};
use axum_server::{from_tcp_rustls, tls_rustls::RustlsConfig};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::spawn;
use tuwunel_core::Result;
use tuwunel_service::Services;

use self::{client::register, fixture::boot};

#[expect(
	dead_code,
	reason = "Only registration and listener readiness are shared with the client API harness."
)]
mod client;

mod fixture;

const TOKEN: &str = "federation-hierarchy-room-access-token";

const CERTIFICATE: &str = "../../nix/pkgs/complement/certificate.crt";

const PRIVATE_KEY: &str = "../../nix/pkgs/complement/private_key.key";

/// A remote hierarchy answer is only used for the room it describes.
///
/// The peer answers every hierarchy request with the summary of its room
/// `!answered`. Asked about `!asked`, that answer is not served as the
/// hierarchy of `!asked`; asked about `!answered`, it is.
#[test]
fn remote_hierarchy_answer_must_describe_the_requested_room() -> Result {
	let options = ["ip_range_denylist=[]", "allow_invalid_tls_certificates=true"];

	boot("federation-hierarchy-room", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	register(services, "hierarchy", TOKEN).await?;

	let peer = TcpListener::bind(("127.0.0.1", 0))?;
	let peer_name = peer.local_addr()?;
	let asked = format!("!asked:{peer_name}");
	let answered = format!("!answered:{peer_name}");

	// tokio refuses to adopt a blocking socket
	peer.set_nonblocking(true)?;
	let peer = spawn(serve_peer(peer, answer(&answered)));

	let hierarchy = async |room_id: &str| {
		services
			.client
			.clients
			.default
			.get(format!("{base}/_matrix/client/v1/rooms/{room_id}/hierarchy"))
			.bearer_auth(TOKEN)
			.send()
			.await
	};

	let asked_status = hierarchy(&asked).await?.status();
	let answered_body: Value = hierarchy(&answered)
		.await?
		.error_for_status()?
		.json()
		.await?;

	peer.abort();

	assert_eq!(
		asked_status,
		StatusCode::NOT_FOUND,
		"the answer about another room was served as the asked room's hierarchy"
	);
	assert_eq!(
		answered_body["rooms"][0]["room_id"], answered,
		"the answer about the asked room was not served"
	);

	Ok(())
}

fn answer(room_id: &str) -> Value {
	json!({
		"room": {
			"room_id": room_id,
			"join_rule": "public",
			"guest_can_join": false,
			"world_readable": false,
			"num_joined_members": 1,
			"children_state": [],
		},
		"children": [],
		"inaccessible_children": [],
	})
}

async fn serve_peer(listener: TcpListener, answer: Value) -> Result {
	let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
	let config =
		RustlsConfig::from_pem_file(manifest.join(CERTIFICATE), manifest.join(PRIVATE_KEY))
			.await?;

	let app = Router::new().fallback(any(async move || Json(answer)));

	from_tcp_rustls(listener, config)?
		.serve(app.into_make_service())
		.await?;

	Ok(())
}
