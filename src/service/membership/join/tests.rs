use ruma::{
	CanonicalJsonObject, MilliSecondsSinceUnixEpoch, OwnedEventId, RoomId, RoomVersionId,
	api::federation::discovery::ServerSigningKeys, room_id,
};
use serde_json::{Value, json, value::to_raw_value};
use tuwunel_core::{
	Result,
	config::Figment,
	matrix::{event::gen_event_id, room_version},
};
use tuwunel_database::Json;

use crate::{Services, test_utils::fixture};

/// Events in a send_join auth chain are checked before they are stored.
///
/// A copy whose content no longer matches its hash is stored redacted and
/// never replaces a copy this server already has, and an event from another
/// room is not stored.
#[tokio::test]
async fn send_join_auth_chain_is_checked_before_storing() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let room_id = room_id!("!join:localhost");
	let version = RoomVersionId::V11;
	let rules = room_version::rules(&version)?;
	let (known_id, known) = topic(services, room_id, "known")?;
	let (unknown_id, unknown) = topic(services, room_id, "unknown")?;
	let (foreign_id, foreign) = topic(services, room_id!("!other:localhost"), "foreign")?;

	// The send_join path verifies only with keys already in storage.
	let (key_id, verify_key) = services.server_keys.active_verify_key();
	let server_name = services.globals.server_name();
	let mut keys =
		ServerSigningKeys::new(server_name.to_owned(), MilliSecondsSinceUnixEpoch::now());
	keys.verify_keys
		.insert(key_id.to_owned(), verify_key.clone());

	services.db["server_signingkeys"].raw_put(server_name, Json(&keys));

	services
		.membership
		.ingest_send_join_auth_chain(room_id, &version, &rules, &[to_raw_value(&known)?])
		.await;

	let auth_chain = [
		to_raw_value(&altered(&known))?,
		to_raw_value(&altered(&unknown))?,
		to_raw_value(&foreign)?,
	];

	services
		.membership
		.ingest_send_join_auth_chain(room_id, &version, &rules, &auth_chain)
		.await;

	let stored = async |event_id| {
		services
			.timeline
			.get_outlier::<Value>(event_id)
			.await
			.map(|event| event["content"].clone())
	};

	assert_eq!(stored(&known_id).await?, json!({ "topic": "known" }));
	assert_eq!(stored(&unknown_id).await?, json!({}));
	assert!(!services.timeline.pdu_exists(&foreign_id).await);

	Ok(())
}

/// A topic event signed by this server, as another server would relay it.
fn topic(services: &Services, room_id: &RoomId, topic: &str) -> Result<(OwnedEventId, Value)> {
	let version = RoomVersionId::V11;
	let mut event: CanonicalJsonObject = serde_json::from_value(json!({
		"type": "m.room.topic",
		"state_key": "",
		"content": { "topic": topic },
		"room_id": room_id,
		"sender": "@bob:localhost",
		"origin_server_ts": 1,
		"depth": 1,
		"prev_events": [],
		"auth_events": [],
	}))?;

	services
		.server_keys
		.hash_and_sign_event(&mut event, &version)?;

	let event_id = gen_event_id(&event, &version)?;

	Ok((event_id, serde_json::to_value(event)?))
}

/// The same event with its topic changed, keeping its hashes and signatures.
fn altered(event: &Value) -> Value {
	let mut event = event.clone();
	event["content"]["topic"] = "altered".into();
	event
}
