use ruma::{
	CanonicalJsonObject, CanonicalJsonValue, EventId, OwnedEventId, RoomVersionId,
	canonical_json::redact_in_place,
	signatures::{Verified, verify_event},
};
use serde_json::value::RawValue as RawJsonValue;
use tuwunel_core::{
	Err, Result, debug_info, err, implement,
	matrix::{event::gen_event_id_canonical_json, room_version},
};

#[implement(super::Service)]
pub async fn validate_and_add_event_id(
	&self,
	pdu: &RawJsonValue,
	room_version_id: &RoomVersionId,
) -> Result<(OwnedEventId, CanonicalJsonObject)> {
	let (event_id, mut value) = gen_event_id_canonical_json(pdu, room_version_id)?;

	self.verify_received_event(&event_id, &mut value, room_version_id)
		.await?;

	// For v3+ rooms we add the event_id, but for v1/v2 rooms it's already present.
	if !room_version::rules(room_version_id)?
		.event_format
		.require_event_id
	{
		value.insert("event_id".into(), CanonicalJsonValue::String(event_id.as_str().into()));
	}

	Ok((event_id, value))
}

#[implement(super::Service)]
pub async fn validate_and_add_event_id_no_fetch(
	&self,
	pdu: &RawJsonValue,
	room_version_id: &RoomVersionId,
) -> Result<(OwnedEventId, CanonicalJsonObject)> {
	let (event_id, mut value) = gen_event_id_canonical_json(pdu, room_version_id)?;
	let room_version_rules = room_version::rules(room_version_id)?;

	if !self
		.required_keys_exist(&value, &room_version_rules)
		.await
	{
		return Err!(BadServerResponse(debug_warn!(
			"Event {event_id} cannot be verified: missing keys."
		)));
	}

	self.verify_received_event(&event_id, &mut value, room_version_id)
		.await?;

	// For v3+ rooms we add the event_id, but for v1/v2 rooms it's already present.
	if !room_version_rules.event_format.require_event_id {
		value.insert("event_id".into(), CanonicalJsonValue::String(event_id.as_str().into()));
	}

	Ok((event_id, value))
}

/// Verifies an event received from another server in place.
///
/// As for any other received event, the sender's `unsigned` data is dropped
/// and an event whose content does not match its content hash is redacted.
#[implement(super::Service)]
async fn verify_received_event(
	&self,
	event_id: &EventId,
	value: &mut CanonicalJsonObject,
	room_version_id: &RoomVersionId,
) -> Result {
	value.remove("unsigned");

	match self
		.verify_event(value, Some(room_version_id))
		.await
	{
		| Ok(Verified::All) => Ok(()),
		| Ok(Verified::Signatures) => {
			debug_info!("Calculated hash does not match (redaction): {event_id}");
			let rules = room_version::rules(room_version_id)?;
			redact_in_place(value, &rules.redaction, None).map_err(|e| {
				err!(BadServerResponse("Event {event_id} could not be redacted: {e}"))
			})
		},
		| Err(e) =>
			Err!(BadServerResponse(debug_error!("Event {event_id} failed verification: {e:?}"))),
	}
}

#[implement(super::Service)]
pub async fn verify_event(
	&self,
	event: &CanonicalJsonObject,
	room_version_id: Option<&RoomVersionId>,
) -> Result<Verified> {
	let room_version_id = room_version_id.unwrap_or(&RoomVersionId::V11);
	let room_version_rules = room_version::rules(room_version_id)?;

	let event_keys = self
		.get_event_keys(event, &room_version_rules)
		.await?;

	verify_event(&event_keys, event, &room_version_rules).map_err(Into::into)
}

#[implement(super::Service)]
pub async fn verify_json(
	&self,
	event: &CanonicalJsonObject,
	room_version_id: Option<&RoomVersionId>,
) -> Result {
	let room_version_id = room_version_id.unwrap_or(&RoomVersionId::V11);
	let room_version_rules = room_version::rules(room_version_id)?;

	let event_keys = self
		.get_event_keys(event, &room_version_rules)
		.await?;

	ruma::signatures::verify_json(&event_keys, event).map_err(Into::into)
}
