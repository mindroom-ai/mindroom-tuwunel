use std::collections::HashSet;

use ruma::{
	CanonicalJsonObject, CanonicalJsonValue, EventId, MilliSecondsSinceUnixEpoch, OwnedEventId,
	RoomId, RoomVersionId, UserId,
	api::error::ErrorKind,
	events::room::{
		create::RoomCreateEventContent,
		join_rules::RoomJoinRulesEventContent,
		member::{MembershipState, RoomMemberEventContent},
	},
	room::JoinRule,
	room_id, user_id,
};
use serde_json::{
	Value, from_value, json, to_vec,
	value::{RawValue as RawJsonValue, to_raw_value},
};
use tuwunel_core::{Error, Result, config::Figment};
use tuwunel_matrix::{
	event::gen_event_id,
	pdu::{PduBuilder, into_outgoing_federation},
	room_version::rules as room_version_rules,
};

use super::merge_member_content;
use crate::{
	Services,
	test_utils::{fixture, pdu_id, with_field},
};

const MAX_BYTES: usize = 4096;

/// A send_join event is stored without its `unsigned` data, and redacted when
/// its content no longer matches its hash.
///
/// An event of another room in the same response is not stored.
#[tokio::test]
async fn an_altered_send_join_event_is_stored_redacted() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let room_id = room_id!("!join:localhost");
	let [(create_id, create), (join_id, join)] = room(services, room_id)?;
	let auth_events = [&create_id, &join_id];
	let topic = json!({"topic": "signed"});
	let name = json!({"name": "signed"});
	let (topic_id, topic_event) = state_event(
		services,
		&RoomVersionId::V11,
		room_id,
		"m.room.topic",
		"",
		&topic,
		&auth_events,
	)?;
	let (name_id, name_event) = state_event(
		services,
		&RoomVersionId::V11,
		room_id,
		"m.room.name",
		"",
		&name,
		&auth_events,
	)?;
	let (foreign_id, foreign) = state_event(
		services,
		&RoomVersionId::V11,
		room_id!("!other:localhost"),
		"m.room.topic",
		"",
		&topic,
		&auth_events,
	)?;

	let altered = with_field(topic_event, "content", json!({"topic": "altered"}));
	let annotated = with_field(name_event, "unsigned", json!({"age": 1}));

	ingest(services, room_id, &[create, join, altered, annotated, foreign]).await?;

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
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let room_id = room_id!("!join:localhost");
	let [(create_id, create), (join_id, join)] = room(services, room_id)?;
	let auth_events = [&create_id, &join_id];
	let signed = |kind, content| {
		state_event(services, &RoomVersionId::V11, room_id, kind, "", &content, &auth_events)
	};
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

	ingest(services, room_id, &[create, join, accepted, altered, genuine]).await?;

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

/// A stored profile avatar past its limit rides along a join with small extras.
///
/// The stored value predates the limit, so the join is not measured over it.
#[test]
fn stored_profile_fields_pass_beside_small_extras() -> Result {
	let avatar = oversized_avatar();
	let extra = extras(json!({ "com.example.key": "value" }));

	let merged = merge(member(Some("name"), Some(&avatar)), Some(&extra))?;

	assert_eq!(field(&merged, "avatar_url"), Some(avatar.as_str()));
	assert_eq!(field(&merged, "com.example.key"), Some("value"));

	Ok(())
}

#[test]
fn extras_past_the_cap_are_refused() {
	let extra = extras(json!({ "com.example.pad": "p".repeat(MAX_BYTES) }));

	assert!(refused(&extra));
}

#[test]
fn a_client_supplied_name_is_bounded_by_characters() -> Result {
	let long = extras(json!({ "displayname": "n".repeat(257) }));

	assert!(refused(&long));

	let fitting = extras(json!({ "displayname": "n".repeat(256) }));

	merge(member(None, None), Some(&fitting))?;

	Ok(())
}

/// A name the client supplies is bounded only while the profile has none.
///
/// Beside a stored name the client's name never reaches the merged event, so it
/// is not bounded.
#[test]
fn a_client_name_beside_a_stored_name_is_not_bounded() -> Result {
	let stored = oversized_name();
	let long = extras(json!({ "displayname": oversized_name() }));

	let merged = merge(member(Some(&stored), None), Some(&long))?;

	assert_eq!(field(&merged, "displayname"), Some(stored.as_str()));

	Ok(())
}

#[test]
fn a_stored_name_past_its_budget_passes_beside_extras() -> Result {
	let name = oversized_name();
	let extra = extras(json!({ "com.example.key": "value" }));

	let merged = merge(member(Some(&name), None), Some(&extra))?;

	assert_eq!(field(&merged, "displayname"), Some(name.as_str()));

	Ok(())
}

#[test]
fn without_extras_nothing_is_measured() -> Result {
	let name = oversized_name();
	let avatar = oversized_avatar();

	merge(member(Some(&name), Some(&avatar)), None)?;

	Ok(())
}

#[test]
fn a_client_avatar_past_its_budget_is_refused_without_a_profile_avatar() -> Result {
	let over = avatar_of_len(1001);
	let extra = extras(json!({ "avatar_url": over }));
	let error = merge(member(Some("name"), None), Some(&extra)).unwrap_err();

	assert!(bad_json(&error));

	let fitting = avatar_of_len(1000);
	let extra = extras(json!({ "avatar_url": fitting }));

	merge(member(Some("name"), None), Some(&extra))?;

	Ok(())
}

#[test]
fn a_stored_avatar_wins_over_the_clients_avatar() -> Result {
	let stored = oversized_avatar();
	let extra = extras(json!({ "avatar_url": avatar_of_len(2016) }));

	let merged = merge(member(Some("name"), Some(&stored)), Some(&extra))?;

	assert_eq!(field(&merged, "avatar_url"), Some(stored.as_str()));

	Ok(())
}

#[test]
fn reserved_join_keys_are_dropped_from_the_extras() -> Result {
	let extra = extras(json!({
		"reason": "client reason",
		"third_party_signed": { "sender": "@user:localhost" },
		"join_authorised_via_users_server": "@user:localhost",
		"com.example.key": "value",
	}));

	let merged = merge(member(Some("name"), None), Some(&extra))?;

	assert!(matches!(
		&merged,
		CanonicalJsonValue::Object(object)
			if !object.contains_key("reason")
				&& !object.contains_key("third_party_signed")
				&& !object.contains_key("join_authorised_via_users_server")
				&& object.contains_key("com.example.key")
	));

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

	let rules = room_version_rules(&v11)?;
	let auth_chain = raw(&[&other_create])?;
	services
		.membership
		.ingest_send_join_events(other, bob, &v11, &rules, &auth_chain, &[])
		.await?;

	let rules = room_version_rules(&v12)?;
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
/// a topic whose power levels were stored before, as knock state left by
/// earlier versions is, when the response leaves them out or they come after
/// the topic and are rejected.
#[tokio::test]
async fn send_join_state_is_authorized() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let bob = user_id!("@bob:localhost");
	let room_id = room_id!("!join:localhost");
	let version = RoomVersionId::V11;
	let rules = room_version_rules(&version)?;
	let [(create_id, create), (join_id, join)] = room(services, room_id)?;
	let (alice_id, alice) = state_event(
		services,
		&RoomVersionId::V11,
		room_id,
		"m.room.member",
		"@alice:localhost",
		&json!({ "membership": "join" }),
		&[&create_id, &join_id],
	)?;
	let (power_id, power) = state_event(
		services,
		&RoomVersionId::V11,
		room_id,
		"m.room.power_levels",
		"",
		&json!({ "users": { "@bob:localhost": 100 } }),
		&[&create_id, &join_id, &alice_id],
	)?;
	let (powered_id, powered) = state_event(
		services,
		&RoomVersionId::V11,
		room_id,
		"m.room.topic",
		"",
		&json!({ "topic": "powered" }),
		&[&create_id, &join_id, &power_id],
	)?;

	let mut stored = power.clone();
	stored["event_id"] = power_id.as_str().into();
	services
		.timeline
		.add_pdu_outlier(&power_id, &from_value(stored)?);

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
/// A join of a create's creator passes the authorization rules from any sender
/// when that create is its only previous event: the create's `creator` up to
/// room version 10, its sender since room version 11. Such a join of a local
/// user sent by anyone else is refused and not stored, unlike one of another
/// server's user. Their own join, such as an old one, is stored, as other
/// events may name it, but the joined room's state leaves it out, while the
/// joining user's own join there still applies.
#[tokio::test]
async fn send_join_state_leaves_other_local_users_alone() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let room_id = room_id!("!join:localhost");
	let version = RoomVersionId::V10;
	let rules = room_version_rules(&version)?;
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
	let (forced_id, forced) =
		event("m.room.member", alice.as_str(), &joined, &[&alice_create_id])?;
	let carol = user_id!("@carol:remote.test");
	let (carol_create_id, carol_create) =
		event("m.room.create", "", &create_content(carol), &[])?;
	let (carol_join_id, carol_join) =
		event("m.room.member", carol.as_str(), &joined, &[&carol_create_id])?;

	// The same join, sent by alice herself.
	let mut own: CanonicalJsonObject = from_value(forced.clone())?;
	own.remove("hashes");
	own.remove("signatures");
	own.insert("sender".into(), alice.as_str().into());
	services
		.server_keys
		.hash_and_sign_event(&mut own, &version)?;

	let alice_join_id = gen_event_id(&own, &version)?;
	let alice_join = serde_json::to_value(own)?;
	let state_lock = services.state.mutex.lock(room_id).await;

	services
		.short
		.get_or_create_shortroomid(room_id)
		.await;

	let auth_chain = raw(&[&alice_create, &forced, &carol_create, &carol_join])?;
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
	assert!(!services.timeline.pdu_exists(&forced_id).await);
	assert!(services.timeline.pdu_exists(&carol_join_id).await);
	assert!(
		!services
			.state_cache
			.is_joined(alice, room_id)
			.await
	);
	assert!(services.state_cache.is_joined(bob, room_id).await);

	Ok(())
}

/// A local user who left a room stays out of it when another one joins.
///
/// `@alice:localhost` joined the room and left. The answer to the join of
/// `@carol:localhost` leaves her out, yet her leave stays in the room's state,
/// so an event whose only previous event is her old join does not join her
/// again.
#[tokio::test]
async fn send_join_state_keeps_other_local_members() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let room_id = room_id!("!join:localhost");
	let version = RoomVersionId::V11;
	let rules = room_version_rules(&version)?;
	let (alice, bob) = (user_id!("@alice:localhost"), user_id!("@bob:localhost"));
	let carol = user_id!("@carol:localhost");
	let create = RoomCreateEventContent {
		room_version: version.clone(),
		..RoomCreateEventContent::new_v11()
	};
	let join_rules = RoomJoinRulesEventContent::new(JoinRule::Public);
	let member = |user_id: &UserId, membership| {
		PduBuilder::state(user_id.as_str(), &RoomMemberEventContent::new(membership))
	};

	services
		.short
		.get_or_create_shortroomid(room_id)
		.await;

	let state_lock = services.state.mutex.lock(room_id).await;
	let append = async |sender, builder| {
		services
			.timeline
			.build_and_append_pdu(builder, sender, room_id, &state_lock)
			.await
	};

	let create_id = append(bob, PduBuilder::state(String::new(), &create)).await?;
	let bob_join_id = append(bob, member(bob, MembershipState::Join)).await?;
	let join_rules_id = append(bob, PduBuilder::state(String::new(), &join_rules)).await?;
	let alice_join_id = append(alice, member(alice, MembershipState::Join)).await?;
	append(alice, member(alice, MembershipState::Leave)).await?;

	let mut answer = Vec::new();
	for event_id in [&create_id, &bob_join_id, &join_rules_id] {
		let event = services.timeline.get_pdu_json(event_id).await?;
		answer.push(to_raw_value(&into_outgoing_federation(event, &version))?);
	}

	let state = services
		.membership
		.ingest_send_join_events(room_id, carol, &version, &rules, &[], &answer)
		.await?;

	services
		.membership
		.apply_send_join_state(room_id, &state, &state_lock)
		.await?;

	drop(state_lock);

	let mut update: CanonicalJsonObject = from_value(json!({
		"type": "m.room.member",
		"state_key": bob,
		"content": { "membership": "join", "displayname": "Bob" },
		"room_id": room_id,
		"sender": bob,
		"origin_server_ts": MilliSecondsSinceUnixEpoch::now(),
		"depth": 5,
		"prev_events": [alice_join_id],
		"auth_events": [create_id, bob_join_id, join_rules_id],
	}))?;

	services
		.server_keys
		.hash_and_sign_event(&mut update, &version)?;

	let update_id = gen_event_id(&update, &version)?;
	services
		.event_handler
		.handle_incoming_pdu(services.globals.server_name(), room_id, &update_id, update, true)
		.await?;

	services
		.timeline
		.non_outlier_pdu_exists(&update_id)
		.await?;

	assert!(
		!services
			.state_cache
			.is_joined(alice, room_id)
			.await
	);

	Ok(())
}

/// A send_join answer's later membership of another local user applies.
///
/// The room's state has the ban of `@alice:localhost`. The answer to the join
/// of `@carol:localhost` has her unbanned and invited again, and its state has
/// the invite, which names the unban rather than the ban.
#[tokio::test]
async fn send_join_state_follows_other_local_members() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let room_id = room_id!("!join:localhost");
	let version = RoomVersionId::V11;
	let rules = room_version_rules(&version)?;
	let (alice, bob) = (user_id!("@alice:localhost"), user_id!("@bob:localhost"));
	let carol = user_id!("@carol:localhost");
	let [(create_id, create), (join_id, join)] = room(services, room_id)?;
	let member = |membership, auth_events: &[&OwnedEventId]| {
		let content = json!({ "membership": membership });
		let kind = "m.room.member";
		state_event(services, &version, room_id, kind, alice.as_str(), &content, auth_events)
	};

	let (ban_id, ban) = member("ban", &[&create_id, &join_id])?;
	let (unban_id, unban) = member("leave", &[&create_id, &join_id, &ban_id])?;
	let (invite_id, invite) = member("invite", &[&create_id, &join_id, &unban_id])?;
	let state_lock = services.state.mutex.lock(room_id).await;

	services
		.short
		.get_or_create_shortroomid(room_id)
		.await;

	let state = raw(&[&create, &join, &ban])?;
	let state = services
		.membership
		.ingest_send_join_events(room_id, bob, &version, &rules, &[], &state)
		.await?;

	services
		.membership
		.apply_send_join_state(room_id, &state, &state_lock)
		.await?;

	let auth_chain = raw(&[&ban, &unban])?;
	let state = raw(&[&create, &join, &invite])?;
	let state: HashSet<_> = services
		.membership
		.ingest_send_join_events(room_id, carol, &version, &rules, &auth_chain, &state)
		.await?
		.into_values()
		.collect();

	assert_eq!(state, HashSet::from([create_id, join_id, invite_id]));

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
	let rules = room_version_rules(&version)?;
	let (v12_create_id, _) = create(services, &RoomVersionId::V12, None, "@bob:localhost")?;
	let v12_room_id = RoomId::new_v2(v12_create_id.localpart())?;
	let answers = [
		(&*v12_room_id, json!({ "room_version": "11" })),
		(room_id!("!join:localhost"), json!({ "room_version": "10" })),
	];

	for (room_id, content) in answers {
		let (create_id, create) = state_event(
			services,
			&RoomVersionId::V11,
			room_id,
			"m.room.create",
			"",
			&content,
			&[],
		)?;
		let (join_id, join) = state_event(
			services,
			&RoomVersionId::V11,
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

/// Ingests the events as the auth chain of a v11 send_join response.
async fn ingest(services: &Services, room_id: &RoomId, events: &[Value]) -> Result {
	let version = RoomVersionId::V11;
	let auth_chain = events
		.iter()
		.map(to_raw_value)
		.collect::<Result<Vec<_>, _>>()?;

	services
		.membership
		.ingest_send_join_events(
			room_id,
			user_id!("@bob:localhost"),
			&version,
			&room_version_rules(&version)?,
			&auth_chain,
			&[],
		)
		.await?;

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
	let mut event: CanonicalJsonObject = from_value(json!({
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

#[test]
fn a_non_string_client_name_or_avatar_is_refused() {
	assert!(refused(&extras(json!({ "displayname": 123 }))));
	assert!(refused(&extras(json!({ "avatar_url": { "url": "mxc://localhost/a" } }))));

	merge(member(None, None), Some(&extras(json!({ "avatar_url": null }))))
		.expect("a null avatar is not a value to check");
}

fn extras(value: Value) -> CanonicalJsonObject { from_value(value).expect("a canonical object") }

fn refused(extra: &CanonicalJsonObject) -> bool {
	merge(member(None, None), Some(extra)).is_err_and(|error| bad_json(&error))
}

fn merge(
	content: RoomMemberEventContent,
	extra: Option<&CanonicalJsonObject>,
) -> Result<CanonicalJsonValue> {
	merge_member_content(MAX_BYTES, content, extra)
}

fn member(displayname: Option<&str>, avatar_url: Option<&str>) -> RoomMemberEventContent {
	from_value(json!({
		"membership": "join",
		"displayname": displayname,
		"avatar_url": avatar_url,
	}))
	.expect("a join member content")
}

fn field<'a>(merged: &'a CanonicalJsonValue, key: &str) -> Option<&'a str> {
	merged
		.as_object()
		.and_then(|object| object.get(key))
		.and_then(CanonicalJsonValue::as_str)
}

fn bad_json(error: &Error) -> bool { matches!(error.kind(), ErrorKind::BadJson) }

fn oversized_name() -> String { "n".repeat(300) }

fn oversized_avatar() -> String { avatar_of_len(1516) }

fn avatar_of_len(len: usize) -> String {
	const PREFIX: &str = "mxc://localhost/";

	format!("{PREFIX}{}", "a".repeat(len.saturating_sub(PREFIX.len())))
}
