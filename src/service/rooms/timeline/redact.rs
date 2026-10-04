use ruma::{
	CanonicalJsonObject, EventId, OwnedEventId, RoomId,
	canonical_json::{RedactedBecause, redact_in_place},
	events::room::encrypted::Relation,
};
use tuwunel_core::{
	Err, Result, err, implement, matrix::event::Event, utils::result::NotFound, warn,
};

use super::ExtractRelatesTo;
use crate::rooms::{short::ShortRoomId, timeline::RoomMutexGuard};

/// Replace a PDU with the redacted form.
#[implement(super::Service)]
#[tracing::instrument(name = "redact", level = "debug", skip(self))]
pub async fn redact_pdu<Pdu: Event + Send + Sync>(
	&self,
	event_id: &EventId,
	reason: &Pdu,
	shortroomid: ShortRoomId,
	state_lock: &RoomMutexGuard,
) -> Result {
	let Ok(pdu_id) = self.get_pdu_id(event_id).await else {
		// If event does not exist, just noop
		// TODO this is actually wrong!
		return Ok(());
	};

	let mut pdu = self
		.get_pdu_json_from_id(&pdu_id)
		.await
		.map_err(|e| {
			err!(Database(error!(?pdu_id, ?event_id, ?e, "PDU ID points to invalid PDU.")))
		})?;

	self.services
		.retention
		.save_original_pdu(event_id, &pdu, state_lock)
		.await;

	let body = pdu["content"]
		.as_object()
		.and_then(|obj| obj.get("body"))
		.and_then(|body| body.as_str());

	if let Some(body) = body {
		self.services
			.search
			.deindex_pdu(shortroomid, &pdu_id, body);
	}

	let room_id: &RoomId = pdu.get("room_id").try_into()?;

	let room_version_id = self
		.services
		.state
		.get_room_version(room_id)
		.await?;

	let room_version_rules = room_version_id.rules().ok_or_else(|| {
		err!(Request(UnsupportedRoomVersion(
			"Cannot redact event for unknown room version {room_version_id:?}."
		)))
	})?;

	self.services
		.pdu_metadata
		.delete_typed_relation(&pdu_id, &pdu)
		.await;

	// Read before `redact_in_place` strips `m.relates_to`.
	let thread_root = thread_root(&pdu);

	redact_in_place(
		&mut pdu,
		&room_version_rules.redaction,
		Some(RedactedBecause::from_json(reason.to_canonical_object())),
	)
	.map_err(|err| err!("invalid event: {err}"))?;

	// The check `replace_pdu` makes, kept for the staged write.
	if self
		.db
		.pduid_pdu
		.get(&pdu_id)
		.await
		.is_not_found()
	{
		return Err!(Request(NotFound("PDU does not exist.")));
	}

	// The redacted reply and its root's thread count are written together.
	let mut txn = self.db.db.txn();

	if let Some(root_event_id) = thread_root
		&& let Err(error) = self
			.services
			.threads
			.stage_reply_redaction(&mut txn, &root_event_id, &pdu_id, state_lock)
			.await
	{
		warn!(%event_id, %root_event_id, %error, "Thread count not updated for redacted reply");
	}

	// Staged last, so the redacted form wins should the reply name itself as
	// its root.
	self.stage_replace_pdu(&mut txn, &pdu_id, &pdu);

	txn.execute();

	Ok(())
}

/// The thread root a reply names, read as `append_pdu_effects` reads it before
/// counting the reply. An already redacted reply names none.
fn thread_root(pdu: &CanonicalJsonObject) -> Option<OwnedEventId> {
	let content = pdu.get("content")?.clone();

	match serde_json::from_value::<ExtractRelatesTo>(content.into())
		.ok()?
		.relates_to
	{
		| Relation::Thread(thread) => Some(thread.event_id),
		| _ => None,
	}
}
