use std::collections::HashMap;

use ruma::{
	CanonicalJsonObject, OwnedEventId, RoomId, RoomVersionId,
	api::federation::membership::{
		RawStrippedState, create_knock_event::v1::Response as SendKnockResponse,
	},
	events::StateEventType,
	room_id, user_id,
};
use serde_json::{Value, json, value::to_raw_value};
use tuwunel_core::{Result, config::Figment, matrix::event::gen_event_id};

use crate::{
	Services,
	test_utils::{fixture, store_own_keys},
};

/// The knock responder's state cannot set a local user's membership.
///
/// Knock state is the answering server's pick of the room's state, yet it
/// becomes the room's state here. A join it claims for a local user is dropped,
/// while its other state still installs.
#[tokio::test]
async fn knock_state_does_not_set_local_memberships() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let room_id = room_id!("!knock:remote.invalid");
	let (alice, carol) = (user_id!("@alice:localhost"), user_id!("@carol:localhost"));
	let (_, name) = pdu(services, room_id, "m.room.name", "", &json!({ "name": "Knock" }))?;
	let (_, join) = pdu(
		services,
		room_id,
		"m.room.member",
		alice.as_str(),
		&json!({ "membership": "join" }),
	)?;
	let response = SendKnockResponse::new(vec![knock_state(&name)?, knock_state(&join)?]);
	let state_lock = services.state.mutex.lock(room_id).await;

	store_own_keys(services);
	services
		.short
		.get_or_create_shortroomid(room_id)
		.await;

	let state_map = services
		.membership
		.ingest_send_knock_state(room_id, carol, &response, &RoomVersionId::V11)
		.await?;

	services
		.membership
		.apply_send_knock_state(room_id, &state_map, &state_lock)
		.await?;

	services
		.state_accessor
		.room_state_get(room_id, &StateEventType::RoomName, "")
		.await?;

	assert!(
		!services
			.state_cache
			.is_joined(alice, room_id)
			.await
	);

	Ok(())
}

/// Knock state keeps the member events of this server's other users.
///
/// The room's state has the leave of `@alice:localhost`. The knock state of
/// `@carol:localhost` leaves member events out, yet her leave stays in the
/// room's state.
#[tokio::test]
async fn knock_state_keeps_other_local_members() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let room_id = room_id!("!knock:remote.invalid");
	let (alice, carol) = (user_id!("@alice:localhost"), user_id!("@carol:localhost"));
	let content = json!({ "membership": "leave" });
	let (leave_id, leave) = pdu(services, room_id, "m.room.member", alice.as_str(), &content)?;
	let (_, name) = pdu(services, room_id, "m.room.name", "", &json!({ "name": "Knock" }))?;
	let response = SendKnockResponse::new(vec![knock_state(&name)?]);
	let state_lock = services.state.mutex.lock(room_id).await;

	store_own_keys(services);
	services
		.short
		.get_or_create_shortroomid(room_id)
		.await;

	services
		.timeline
		.add_pdu_outlier(&leave_id, &serde_json::from_value(leave)?);

	let shortstatekey = services
		.short
		.get_or_create_shortstatekey(&StateEventType::RoomMember, alice.as_str())
		.await;

	let state_map = HashMap::from([(shortstatekey, leave_id.clone())]);
	services
		.membership
		.apply_send_knock_state(room_id, &state_map, &state_lock)
		.await?;

	let state_map = services
		.membership
		.ingest_send_knock_state(room_id, carol, &response, &RoomVersionId::V11)
		.await?;

	services
		.membership
		.apply_send_knock_state(room_id, &state_map, &state_lock)
		.await?;

	let member_id = services
		.state_accessor
		.room_state_get_id(room_id, &StateEventType::RoomMember, alice.as_str())
		.await?;

	assert_eq!(member_id, leave_id);

	Ok(())
}

/// Knock state events are checked before they are stored.
///
/// A copy whose content no longer matches its hash leaves the stored event
/// alone and stays out of the room's state, and an event from another room is
/// not stored.
#[tokio::test]
async fn knock_state_is_checked_before_storing() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let room_id = room_id!("!knock:remote.invalid");
	let carol = user_id!("@carol:localhost");
	let other_room_id = room_id!("!other:remote.invalid");
	let content = json!({ "topic": "known" });
	let (topic_id, topic) = pdu(services, room_id, "m.room.topic", "", &content)?;
	let (foreign_id, foreign) = pdu(services, other_room_id, "m.room.topic", "", &content)?;
	let mut altered = topic.clone();
	altered["content"]["topic"] = "altered".into();

	store_own_keys(services);
	services
		.timeline
		.add_pdu_outlier(&topic_id, &serde_json::from_value(topic)?);

	let response = SendKnockResponse::new(vec![knock_state(&altered)?, knock_state(&foreign)?]);
	let state_map = services
		.membership
		.ingest_send_knock_state(room_id, carol, &response, &RoomVersionId::V11)
		.await?;

	let stored: Value = services.timeline.get_outlier(&topic_id).await?;
	assert_eq!(stored["content"], content);
	assert!(state_map.is_empty());
	assert!(!services.timeline.pdu_exists(&foreign_id).await);

	Ok(())
}

/// A state event signed by this server, as the answering server would relay
/// it.
fn pdu(
	services: &Services,
	room_id: &RoomId,
	kind: &str,
	state_key: &str,
	content: &Value,
) -> Result<(OwnedEventId, Value)> {
	let version = RoomVersionId::V11;
	let mut event: CanonicalJsonObject = serde_json::from_value(json!({
		"type": kind,
		"state_key": state_key,
		"content": content,
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

/// The event as a full PDU entry of a knock response.
fn knock_state(event: &Value) -> Result<RawStrippedState> {
	Ok(RawStrippedState::Pdu(to_raw_value(event)?))
}
