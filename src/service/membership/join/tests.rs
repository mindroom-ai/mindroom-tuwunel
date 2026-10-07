use ruma::{CanonicalJsonObject, EventId, OwnedEventId, RoomId, RoomVersionId, room_id};
use serde_json::{Value, from_value, json, to_vec, value::to_raw_value};
use tuwunel_core::{Result, config::Figment};
use tuwunel_matrix::{event::gen_event_id, room_version::rules as room_version_rules};

use crate::{
	Services,
	test_utils::{keyed_fixture, pdu_id, signed_state, with_field},
};

/// A send_join event is stored without its `unsigned` data, and redacted when
/// its content no longer matches its hash.
///
/// An event of another room in the same response is not stored.
#[tokio::test]
async fn an_altered_send_join_event_is_stored_redacted() -> Result {
	let Some(fixture) = keyed_fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let room_id = room_id!("!join:localhost");
	let topic = json!({"topic": "signed"});
	let name = json!({"name": "signed"});
	let (topic_id, topic_event) = signed_state(services, room_id, "m.room.topic", "", &topic)?;
	let (name_id, name_event) = signed_state(services, room_id, "m.room.name", "", &name)?;
	let (foreign_id, foreign) =
		signed_state(services, room_id!("!other:localhost"), "m.room.topic", "", &topic)?;

	let altered = with_field(topic_event, "content", json!({"topic": "altered"}));
	let annotated = with_field(name_event, "unsigned", json!({"age": 1}));

	ingest(services, room_id, &[altered, annotated, foreign]).await?;

	let altered: Value = services.timeline.get_outlier(&topic_id).await?;
	let annotated: Value = services.timeline.get_outlier(&name_id).await?;

	assert_eq!(altered["content"], json!({}), "an altered event is stored redacted");
	assert_eq!(annotated["content"], name);
	assert!(annotated.get("unsigned").is_none(), "remote unsigned data is dropped");
	assert!(!services.timeline.pdu_exists(&foreign_id).await);

	Ok(())
}

/// A send_join event leaves this server's copy alone unless that copy is an
/// outlier and the new one matches its content hash.
///
/// An accepted event gains no outlier beside it. An altered copy leaves a
/// stored outlier as it was. A verified copy replaces an outlier stored with
/// altered content.
#[tokio::test]
async fn a_send_join_event_replaces_only_an_altered_outlier() -> Result {
	let Some(fixture) = keyed_fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let room_id = room_id!("!join:localhost");
	let signed = |kind, content| signed_state(services, room_id, kind, "", &content);
	let known_topic = json!({"topic": "known"});
	let genuine_avatar = json!({"url": "mxc://localhost/genuine"});
	let (accepted_id, accepted) = signed("m.room.name", json!({"name": "accepted"}))?;
	let (known_id, known) = signed("m.room.topic", known_topic.clone())?;
	let (forged_id, genuine) = signed("m.room.avatar", genuine_avatar.clone())?;
	let forged = with_field(genuine.clone(), "content", json!({"url": "mxc://localhost/forged"}));

	put_accepted(services, &accepted_id, &accepted)?;

	for (event_id, event) in [(&known_id, known.clone()), (&forged_id, forged)] {
		services
			.timeline
			.add_pdu_outlier(event_id, &from_value(event)?);
	}

	let altered = with_field(known, "content", json!({"topic": "altered"}));

	ingest(services, room_id, &[accepted, altered, genuine]).await?;

	let outlier_content = async |event_id| {
		services
			.timeline
			.get_outlier(event_id)
			.await
			.map(|event: Value| event["content"].clone())
	};

	let accepted_outlier = services
		.timeline
		.outlier_pdu_exists(&accepted_id)
		.await;

	assert!(accepted_outlier.is_err(), "an accepted event gains no outlier");
	assert_eq!(outlier_content(&known_id).await?, known_topic);
	assert_eq!(outlier_content(&forged_id).await?, genuine_avatar);

	Ok(())
}

/// A create event in a v12 send_join is stored only for its own room.
///
/// The joined room's create is stored under the joined room. A v11 create from
/// another room keeps that room's stored copy, and a v12 create of another room
/// is not stored.
#[tokio::test]
async fn send_join_create_keeps_its_own_room_id() -> Result {
	let Some(fixture) = keyed_fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let (v11, v12) = (RoomVersionId::V11, RoomVersionId::V12);
	let other = room_id!("!other:localhost");
	let (other_id, other_create) = create(services, &v11, Some(other), "@bob:localhost")?;
	let (joined_id, joined_create) = create(services, &v12, None, "@bob:localhost")?;
	let (foreign_id, foreign_create) = create(services, &v12, None, "@carol:localhost")?;
	let room_id = RoomId::new_v2(joined_id.localpart())?;

	let rules = room_version_rules(&v11)?;
	let auth_chain = [to_raw_value(&other_create)?];
	services
		.membership
		.ingest_send_join_auth_chain(other, &v11, &rules, &auth_chain)
		.await;

	let rules = room_version_rules(&v12)?;
	let auth_chain = [
		to_raw_value(&joined_create)?,
		to_raw_value(&other_create)?,
		to_raw_value(&foreign_create)?,
	];
	services
		.membership
		.ingest_send_join_auth_chain(&room_id, &v12, &rules, &auth_chain)
		.await;

	let stored_room = async |event_id| {
		services
			.timeline
			.get_outlier::<Value>(event_id)
			.await
			.map(|event| event["room_id"].clone())
	};

	assert_eq!(stored_room(&joined_id).await?, json!(room_id));
	assert_eq!(stored_room(&other_id).await?, json!(other));
	assert!(!services.timeline.pdu_exists(&foreign_id).await);

	Ok(())
}

async fn ingest(services: &Services, room_id: &RoomId, events: &[Value]) -> Result {
	let version = RoomVersionId::V11;
	let auth_chain = events
		.iter()
		.map(to_raw_value)
		.collect::<Result<Vec<_>, _>>()?;

	services
		.membership
		.ingest_send_join_auth_chain(
			room_id,
			&version,
			&room_version_rules(&version)?,
			&auth_chain,
		)
		.await;

	Ok(())
}

/// Writes an accepted copy of an event straight to storage.
///
/// The event's ID maps to a timeline row holding it, so it reads as accepted,
/// and no outlier row is written.
fn put_accepted(services: &Services, event_id: &EventId, event: &Value) -> Result {
	let pdu_id = pdu_id(1);

	services.db["eventid_pduid"].insert(event_id.as_bytes(), pdu_id.as_bytes());
	services.db["pduid_pdu"].insert(pdu_id.as_bytes(), to_vec(event)?);

	Ok(())
}

/// A create event signed by this server; up to v11 it carries its room ID.
fn create(
	services: &Services,
	version: &RoomVersionId,
	room_id: Option<&RoomId>,
	sender: &str,
) -> Result<(OwnedEventId, Value)> {
	let mut event: CanonicalJsonObject = from_value(json!({
		"type": "m.room.create",
		"state_key": "",
		"content": { "room_version": version },
		"sender": sender,
		"origin_server_ts": 1,
		"depth": 1,
		"prev_events": [],
		"auth_events": [],
	}))?;

	if let Some(room_id) = room_id {
		event.insert("room_id".into(), room_id.as_str().into());
	}

	services
		.server_keys
		.hash_and_sign_event(&mut event, version)?;

	let event_id = gen_event_id(&event, version)?;

	Ok((event_id, serde_json::to_value(event)?))
}
