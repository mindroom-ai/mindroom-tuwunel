use std::collections::HashSet;

use ruma::{CanonicalJsonObject, OwnedEventId, RoomId, RoomVersionId, room_id, user_id};
use serde_json::{
	Value, json,
	value::{RawValue as RawJsonValue, to_raw_value},
};
use tuwunel_core::{
	Result,
	config::Figment,
	matrix::{event::gen_event_id, room_version},
};

use crate::{
	Services,
	test_utils::{fixture, store_own_keys},
};

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
	let bob = user_id!("@bob:localhost");
	let room_id = room_id!("!join:localhost");
	let version = RoomVersionId::V11;
	let rules = room_version::rules(&version)?;
	let [(create_id, create), (join_id, join)] = room(services, room_id)?;
	let auth_events = [&create_id, &join_id];
	let (known_id, known) = topic(services, room_id, "known", &auth_events)?;
	let (unknown_id, unknown) = topic(services, room_id, "unknown", &auth_events)?;
	let (foreign_id, foreign) =
		topic(services, room_id!("!other:localhost"), "foreign", &auth_events)?;

	store_own_keys(services);

	let auth_chain = raw(&[&create, &join, &known])?;
	services
		.membership
		.ingest_send_join_events(room_id, bob, &version, &rules, &auth_chain, &[])
		.await?;

	let auth_chain = raw(&[&create, &join, &altered(&known), &altered(&unknown), &foreign])?;
	services
		.membership
		.ingest_send_join_events(room_id, bob, &version, &rules, &auth_chain, &[])
		.await?;

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

/// A send_join state event replaces the copy knock state stored for it.
///
/// Knock state stored unchecked by earlier versions is replaced by the joined
/// room's copy, whose content matches its hash.
#[tokio::test]
async fn send_join_state_replaces_knock_state() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let bob = user_id!("@bob:localhost");
	let room_id = room_id!("!join:localhost");
	let version = RoomVersionId::V11;
	let rules = room_version::rules(&version)?;
	let [(create_id, create), (join_id, join)] = room(services, room_id)?;
	let (event_id, event) = topic(services, room_id, "joined", &[&create_id, &join_id])?;

	store_own_keys(services);

	services
		.timeline
		.add_pdu_outlier(&event_id, &serde_json::from_value(altered(&event))?);

	let state = raw(&[&create, &join, &event])?;
	services
		.membership
		.ingest_send_join_events(room_id, bob, &version, &rules, &[], &state)
		.await?;

	let stored: Value = services.timeline.get_outlier(&event_id).await?;
	assert_eq!(stored["content"], json!({ "topic": "joined" }));

	Ok(())
}

/// A create event in a v12 send_join is stored only for its own room.
///
/// The joined room's create is stored under the joined room. A v11 create from
/// another room keeps that room's stored copy, and a v12 create of another room
/// is not stored.
#[tokio::test]
async fn send_join_create_keeps_its_own_room_id() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let bob = user_id!("@bob:localhost");
	let (v11, v12) = (RoomVersionId::V11, RoomVersionId::V12);
	let other = room_id!("!other:localhost");
	let (other_id, other_create) = create(services, &v11, Some(other), "@bob:localhost")?;
	let (joined_id, joined_create) = create(services, &v12, None, "@bob:localhost")?;
	let (foreign_id, foreign_create) = create(services, &v12, None, "@carol:localhost")?;
	let room_id = RoomId::new_v2(joined_id.localpart())?;

	store_own_keys(services);

	let rules = room_version::rules(&v11)?;
	let auth_chain = raw(&[&other_create])?;
	services
		.membership
		.ingest_send_join_events(other, bob, &v11, &rules, &auth_chain, &[])
		.await?;

	let rules = room_version::rules(&v12)?;
	let auth_chain = raw(&[&joined_create, &other_create, &foreign_create])?;
	services
		.membership
		.ingest_send_join_events(&room_id, bob, &v12, &rules, &auth_chain, &[])
		.await?;

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

/// send_join events are checked against their own auth events.
///
/// A join the creator sends for another user is rejected: it is not stored and
/// stays out of the room state, while the creator's own events are kept. So is
/// a topic whose power levels were stored before, as knock state is, when the
/// response leaves them out or they come after the topic and are rejected.
#[tokio::test]
async fn send_join_state_is_authorized() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let bob = user_id!("@bob:localhost");
	let room_id = room_id!("!join:localhost");
	let version = RoomVersionId::V11;
	let rules = room_version::rules(&version)?;
	let [(create_id, create), (join_id, join)] = room(services, room_id)?;
	let (alice_id, alice) = state_event(
		services,
		&version,
		room_id,
		"m.room.member",
		"@alice:localhost",
		&json!({ "membership": "join" }),
		&[&create_id, &join_id],
	)?;
	let (power_id, power) = state_event(
		services,
		&version,
		room_id,
		"m.room.power_levels",
		"",
		&json!({ "users": { "@bob:localhost": 100 } }),
		&[&create_id, &join_id, &alice_id],
	)?;
	let (powered_id, powered) =
		topic(services, room_id, "powered", &[&create_id, &join_id, &power_id])?;

	store_own_keys(services);

	let mut stored = power.clone();
	stored["event_id"] = power_id.as_str().into();
	services
		.timeline
		.add_pdu_outlier(&power_id, &serde_json::from_value(stored)?);

	let omitted = raw(&[&create, &join, &alice, &powered])?;
	let after = raw(&[&create, &join, &alice, &powered, &power])?;
	for state in [omitted, after] {
		let state: HashSet<_> = services
			.membership
			.ingest_send_join_events(room_id, bob, &version, &rules, &[], &state)
			.await?
			.into_values()
			.collect();

		assert_eq!(state, HashSet::from([create_id.clone(), join_id.clone()]));
		assert!(!services.timeline.pdu_exists(&alice_id).await);
		assert!(!services.timeline.pdu_exists(&powered_id).await);
	}

	Ok(())
}

/// The state of a send_join answer cannot make another local user joined.
///
/// Up to room version 10, a join for the creator a create event names passes
/// from any sender when that create is its only previous event, so a second
/// create naming a local user lets the answering server join them. Their join
/// is stored, as other events may name it, but the joined room's state leaves
/// it out, while the joining user's own join there still applies.
#[tokio::test]
async fn send_join_state_leaves_other_local_users_alone() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let room_id = room_id!("!join:localhost");
	let version = RoomVersionId::V10;
	let rules = room_version::rules(&version)?;
	let (alice, bob) = (user_id!("@alice:localhost"), user_id!("@bob:localhost"));
	let joined = json!({ "membership": "join" });
	let create_content = |creator| json!({ "creator": creator, "room_version": version });
	let event = |kind, state_key: &str, content: &Value, auth_events: &[&OwnedEventId]| {
		state_event(services, &version, room_id, kind, state_key, content, auth_events)
	};

	let (create_id, room_create) = event("m.room.create", "", &create_content(bob), &[])?;
	let (_, bob_join) = event("m.room.member", bob.as_str(), &joined, &[&create_id])?;
	let (alice_create_id, alice_create) =
		event("m.room.create", "", &create_content(alice), &[])?;
	let (alice_join_id, alice_join) =
		event("m.room.member", alice.as_str(), &joined, &[&alice_create_id])?;
	let state_lock = services.state.mutex.lock(room_id).await;

	store_own_keys(services);
	services
		.short
		.get_or_create_shortroomid(room_id)
		.await;

	let auth_chain = raw(&[&alice_create])?;
	let state = raw(&[&room_create, &bob_join, &alice_join])?;
	let state = services
		.membership
		.ingest_send_join_events(room_id, bob, &version, &rules, &auth_chain, &state)
		.await?;

	services
		.membership
		.apply_send_join_state(room_id, &state, &state_lock)
		.await?;

	assert!(services.timeline.pdu_exists(&alice_join_id).await);
	assert!(
		!services
			.state_cache
			.is_joined(alice, room_id)
			.await
	);
	assert!(services.state_cache.is_joined(bob, room_id).await);

	Ok(())
}

/// A send_join create event that is rejected, or that names another room
/// version than the join's, fails the join before anything is stored.
///
/// A v11 create cannot belong to a room ID without a server name, as v12 rooms
/// have, so a v11 answer for a v12 room is refused.
#[tokio::test]
async fn send_join_refuses_a_rejected_create() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let bob = user_id!("@bob:localhost");
	let version = RoomVersionId::V11;
	let rules = room_version::rules(&version)?;
	let (v12_create_id, _) = create(services, &RoomVersionId::V12, None, "@bob:localhost")?;
	let v12_room_id = RoomId::new_v2(v12_create_id.localpart())?;
	let answers = [
		(&*v12_room_id, json!({ "room_version": "11" })),
		(room_id!("!join:localhost"), json!({ "room_version": "10" })),
	];

	store_own_keys(services);

	for (room_id, content) in answers {
		let (create_id, create) =
			state_event(services, &version, room_id, "m.room.create", "", &content, &[])?;
		let (join_id, join) = state_event(
			services,
			&version,
			room_id,
			"m.room.member",
			"@bob:localhost",
			&json!({ "membership": "join" }),
			&[&create_id],
		)?;

		let state = raw(&[&create, &join])?;
		let result = services
			.membership
			.ingest_send_join_events(room_id, bob, &version, &rules, &[], &state)
			.await;

		assert!(result.is_err(), "{room_id} create was accepted");
		assert!(!services.timeline.pdu_exists(&create_id).await);
		assert!(!services.timeline.pdu_exists(&join_id).await);
	}

	Ok(())
}

/// A topic event of `@bob:localhost`.
fn topic(
	services: &Services,
	room_id: &RoomId,
	topic: &str,
	auth_events: &[&OwnedEventId],
) -> Result<(OwnedEventId, Value)> {
	let content = json!({ "topic": topic });
	let version = RoomVersionId::V11;
	state_event(services, &version, room_id, "m.room.topic", "", &content, auth_events)
}

/// The same event with its topic changed, keeping its hashes and signatures.
fn altered(event: &Value) -> Value {
	let mut event = event.clone();
	event["content"]["topic"] = "altered".into();
	event
}

/// A create event signed by this server; up to v11 it carries its room ID.
fn create(
	services: &Services,
	version: &RoomVersionId,
	room_id: Option<&RoomId>,
	sender: &str,
) -> Result<(OwnedEventId, Value)> {
	let mut event: CanonicalJsonObject = serde_json::from_value(json!({
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

/// The create event of a v11 room and the join of its creator,
/// `@bob:localhost`.
fn room(services: &Services, room_id: &RoomId) -> Result<[(OwnedEventId, Value); 2]> {
	let create = create(services, &RoomVersionId::V11, Some(room_id), "@bob:localhost")?;
	let join = state_event(
		services,
		&RoomVersionId::V11,
		room_id,
		"m.room.member",
		"@bob:localhost",
		&json!({ "membership": "join" }),
		&[&create.0],
	)?;

	Ok([create, join])
}

/// A state event of `@bob:localhost` signed by this server, as another server
/// would relay it.
///
/// Its previous event is the last of its auth events, and its depth is one
/// more than their number.
fn state_event(
	services: &Services,
	version: &RoomVersionId,
	room_id: &RoomId,
	kind: &str,
	state_key: &str,
	content: &Value,
	auth_events: &[&OwnedEventId],
) -> Result<(OwnedEventId, Value)> {
	let prev_events: Vec<_> = auth_events.last().into_iter().collect();
	let mut event: CanonicalJsonObject = serde_json::from_value(json!({
		"type": kind,
		"state_key": state_key,
		"content": content,
		"room_id": room_id,
		"sender": "@bob:localhost",
		"origin_server_ts": 1,
		"depth": auth_events.len().saturating_add(1),
		"prev_events": prev_events,
		"auth_events": auth_events,
	}))?;

	services
		.server_keys
		.hash_and_sign_event(&mut event, version)?;

	let event_id = gen_event_id(&event, version)?;

	Ok((event_id, serde_json::to_value(event)?))
}

/// The events as a send_join response carries them.
fn raw(events: &[&Value]) -> Result<Vec<Box<RawJsonValue>>> {
	events
		.iter()
		.map(|event| Ok(to_raw_value(event)?))
		.collect()
}
