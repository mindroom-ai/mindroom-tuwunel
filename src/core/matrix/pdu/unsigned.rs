use std::collections::BTreeMap;

use ruma::{
	MilliSecondsSinceUnixEpoch,
	events::{AnyTimelineEvent, room::member::MembershipState},
	serde::Raw,
};
use serde::Serialize;
use serde_json::value::{RawValue as RawJsonValue, Value as JsonValue, to_raw_value};

use super::{Event, Pdu, Unsigned};
use crate::{Result, err, implement};

#[implement(Pdu)]
pub fn remove_transaction_id(&mut self) -> Result {
	use BTreeMap as Map;

	let Some(unsigned) = &self.unsigned else {
		return Ok(());
	};

	let mut unsigned: Map<&str, Raw<JsonValue>> = serde_json::from_str(unsigned.json().get())
		.map_err(|e| err!(Database("Invalid unsigned in pdu event: {e}")))?;

	unsigned.remove("transaction_id");
	self.unsigned = to_raw_value(&unsigned)
		.map(Into::into)
		.map(Some)
		.expect("unsigned is valid");

	Ok(())
}

#[implement(Pdu)]
pub fn add_age(&mut self) -> Result {
	use BTreeMap as Map;

	let mut unsigned: Map<&str, Raw<JsonValue>> = self
		.unsigned
		.as_ref()
		.map(Unsigned::json)
		.map(RawJsonValue::get)
		.map_or_else(|| Ok(Map::new()), serde_json::from_str)
		.map_err(|e| err!(Database("Invalid unsigned in pdu event: {e}")))?;

	// deliberately allowing for the possibility of negative age
	let now: i128 = MilliSecondsSinceUnixEpoch::now().get().into();
	let then: i128 = self.origin_server_ts.into();
	let this_age = now.saturating_sub(then);

	unsigned.insert("age", raw_of(&this_age)?);
	self.unsigned = Some(to_raw_value(&unsigned)?.into());

	Ok(())
}

/// MSC4115: annotate the served event with the requesting user's room
/// membership at the time of the event.
#[implement(Pdu)]
pub fn add_membership(&mut self, membership: &MembershipState) -> Result {
	use BTreeMap as Map;

	let mut unsigned: Map<&str, Raw<JsonValue>> = self
		.unsigned
		.as_ref()
		.map(Unsigned::json)
		.map(RawJsonValue::get)
		.map_or_else(|| Ok(Map::new()), serde_json::from_str)
		.map_err(|e| err!(Database("Invalid unsigned in pdu event: {e}")))?;

	unsigned.insert("membership", raw_of(membership)?);
	self.unsigned = Some(to_raw_value(&unsigned)?.into());

	Ok(())
}

/// MSC2675: attach a related event as a bundled aggregation at
/// `unsigned.m.relations.<name>`. The bundle is the full related event in
/// client format — including `room_id` and `origin_server_ts` — because
/// clients (notably the MindRoom Cinny fork) hydrate replacements straight
/// from this object and ignore bundles missing `origin_server_ts`.
#[implement(Pdu)]
pub fn add_relation(&mut self, name: &str, related: &Pdu) -> Result {
	use serde_json::Map;

	let mut unsigned: Map<String, JsonValue> = self
		.unsigned
		.as_ref()
		.map(Unsigned::json)
		.map(RawJsonValue::get)
		.map_or_else(|| Ok(Map::new()), serde_json::from_str)
		.map_err(|e| err!(Database("Invalid unsigned in pdu event: {e}")))?;

	let related: Raw<AnyTimelineEvent> = related.to_format();
	let related: JsonValue = serde_json::from_str(related.json().get())
		.map_err(|e| err!(Database("Invalid related event for bundled aggregation: {e}")))?;

	unsigned
		.entry("m.relations")
		.or_insert(JsonValue::Object(Map::new()))
		.as_object_mut()
		.map(|object| object.insert(name.to_owned(), related));

	self.unsigned = Some(to_raw_value(&unsigned)?.into());

	Ok(())
}

/// MSC3816: overwrite `unsigned.m.relations.m.thread.current_user_participated`
/// with a per-requester value. No-op when the event carries no thread bundle.
#[implement(Pdu)]
pub fn set_thread_participated(&mut self, participated: bool) -> Result {
	use serde_json::Map;

	let Some(unsigned) = self.unsigned.as_ref() else {
		return Ok(());
	};

	let mut unsigned: Map<String, JsonValue> = serde_json::from_str(unsigned.json().get())
		.map_err(|e| err!(Database("Invalid unsigned in pdu event: {e}")))?;

	let updated = unsigned
		.get_mut("m.relations")
		.and_then(JsonValue::as_object_mut)
		.and_then(|relations| relations.get_mut("m.thread"))
		.and_then(JsonValue::as_object_mut)
		.map(|thread| {
			thread.insert("current_user_participated".to_owned(), participated.into());
		})
		.is_some();

	if updated {
		self.unsigned = Some(to_raw_value(&unsigned)?.into());
	}

	Ok(())
}

#[inline]
fn raw_of<T: Serialize>(value: &T) -> Result<Raw<JsonValue>> {
	Ok(Raw::from_raw_value(&to_raw_value(value)?))
}
