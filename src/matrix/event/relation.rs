use ruma::{OwnedEventId, events::relation::RelationType};
use serde::Deserialize;

use super::Event;

/// Compares an event's relation type with a requested relation type.
///
/// Implementations inspect the `m.relates_to.rel_type` field in event content.
/// Missing or malformed relation content does not match.
pub trait RelationTypeEqual<E: Event> {
	/// Returns whether the event declares this relation type.
	///
	/// The comparison deserializes only the relation fields needed for the
	/// check. Content that cannot be deserialized returns false.
	fn relation_type_equal(&self, event: &E) -> bool;
}

/// Minimal relation metadata extracted from an event's content.
#[derive(Clone, Debug, Deserialize)]
pub struct ExtractRelatesToInfo {
	/// The event content's `m.relates_to` object.
	#[serde(rename = "m.relates_to")]
	pub relates_to: RelatesToInfo,
}

/// The relation type and target event needed to follow an edit chain.
#[derive(Clone, Debug, Deserialize)]
pub struct RelatesToInfo {
	/// The relation type declared by the event.
	pub rel_type: String,
	/// The event targeted by the relation.
	pub event_id: OwnedEventId,
}

#[derive(Clone, Debug, Deserialize)]
struct ExtractRelatesToEventId {
	#[serde(rename = "m.relates_to")]
	relates_to: ExtractRelType,
}

#[derive(Clone, Debug, Deserialize)]
struct ExtractRelType {
	rel_type: RelationType,
}

impl<E: Event> RelationTypeEqual<E> for RelationType {
	fn relation_type_equal(&self, event: &E) -> bool {
		event
			.get_content()
			.map(|c: ExtractRelatesToEventId| c.relates_to.rel_type)
			.is_ok_and(|r| r == *self)
	}
}
