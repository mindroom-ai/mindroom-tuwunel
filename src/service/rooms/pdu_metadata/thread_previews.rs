//! Requester-specific presentation fields, separate from standard thread semantics.

use std::collections::BTreeSet;

use futures::{StreamExt, pin_mut};
use ruma::{
	OwnedUserId, UserId,
	events::{
		AnySyncMessageLikeEvent, StateEventType, TimelineEventType,
		room::power_levels::{RoomPowerLevels, RoomPowerLevelsEventContent},
	},
	int,
	serde::Raw,
};
use serde_json::{Value, value::to_raw_value};
use tuwunel_core::{
	PduId, Result, implement,
	matrix::{Event, Pdu, PduCount, RawPduId, room_version},
	utils::{
		stream::{ReadyExt, TryIgnore},
		u64_from_u8,
	},
};

use super::{
	ExtractRelatesTo, Service,
	typed_relations::{CHILD_COUNT_OFFSET, KEY_LEN, Tag, prefix},
};

const TITLE: &str = "io.mindroom.title";
const REPLY: &str = "io.mindroom.latest_reply";

/// Marked events are metadata even when the author cannot publish a title.
/// In particular, an unauthorized marker cannot displace a real reply.
pub(super) fn preview_tag(event: &impl Event) -> Option<Tag> {
	if event.state_key().is_some() || *event.kind() == TimelineEventType::RoomEncrypted {
		return None;
	}

	let content: Value = event.get_content().ok()?;
	Some(if content["io.mindroom.thread_summary"].is_object() {
		Tag::ThreadTitle
	} else {
		Tag::ThreadReply
	})
}

#[implement(Service)]
pub(super) async fn bundle_thread_previews(&self, user: &UserId, root: &mut Pdu) -> Result {
	let Some(unsigned) = &root.unsigned else { return Ok(()) };
	let mut unsigned: Value = serde_json::from_str(unsigned.json().get())?;
	let Some(thread) = unsigned
		.get_mut("m.relations")
		.and_then(|relations| relations.get_mut("m.thread"))
		.and_then(Value::as_object_mut)
	else {
		return Ok(());
	};

	// Never trust persisted/federated copies of these per-recipient fields.
	thread.remove(TITLE);
	thread.remove(REPLY);

	let access = &self.services.state_accessor;
	if !access.is_encrypted_room(root.room_id()).await
		&& access.user_can_see_event(user, root).await
		&& let Ok(root_id) = self
			.services
			.timeline
			.get_pdu_id(root.event_id())
			.await
	{
		let ignored = self
			.services
			.users
			.ignored_users(user)
			.await
			.map(|list| list.ignored_users.into_keys().collect())
			.unwrap_or_default();
		for (field, tag) in [(TITLE, Tag::ThreadTitle), (REPLY, Tag::ThreadReply)] {
			if let Some(child) = self
				.preview_child(user, &ignored, root, root_id.into(), tag)
				.await
			{
				let event: Raw<AnySyncMessageLikeEvent> = child.into_format();
				thread.insert(field.to_owned(), serde_json::to_value(event)?);
			}
		}
	}

	root.unsigned = Some(to_raw_value(&unsigned)?.into());
	Ok(())
}

/// Two separate indexes avoid scanning a thread's replies to find its title.
/// Filtered and missing rows consume the same bounded budget as redaction repair.
#[implement(Service)]
async fn preview_child(
	&self,
	user: &UserId,
	ignored: &BTreeSet<OwnedUserId>,
	root: &Pdu,
	root_id: PduId,
	tag: Tag,
) -> Option<Pdu> {
	let prefix = prefix(root_id.shortroomid, root_id.count, tag);
	let mut seek = super::typed_relations::Key::new();
	seek.extend(prefix.iter().copied());
	seek.extend([u8::MAX; size_of::<u64>() * 2]);
	let rows = self
		.db
		.relatesto_typed
		.rev_raw_keys_from(seek.as_slice())
		.ignore_err()
		.ready_take_while(move |key| key.starts_with(&prefix))
		.take(
			self.services
				.server
				.config
				.thread_latest_reply_search_limit,
		)
		.ready_filter(|key| key.len() == KEY_LEN)
		.map(|key| PduCount::from_unsigned(u64_from_u8(&key[CHILD_COUNT_OFFSET..KEY_LEN])));
	pin_mut!(rows);
	while let Some(count) = rows.next().await {
		let child_id: RawPduId = PduId { shortroomid: root_id.shortroomid, count }.into();
		let Ok(mut child) = self
			.services
			.timeline
			.get_pdu_from_id(&child_id)
			.await
		else {
			continue;
		};
		if child.room_id() != root.room_id()
			|| child.is_redacted()
			|| preview_tag(&child) != Some(tag)
			|| !child.get_content::<ExtractRelatesTo>().is_ok_and(|content| {
				matches!(content.relates_to, ruma::events::room::encrypted::Relation::Thread(t) if t.event_id == root.event_id())
			})
			|| ignored.contains(child.sender())
			|| self.services.config.is_forbidden_remote_server_name(child.sender().server_name())
			|| !self.services.state_accessor.user_can_see_event(user, &child).await
			|| self.services.state_accessor.erased_for(user, &child).await
			|| tag == Tag::ThreadTitle && !self.title_authorized(&child).await
		{
			continue;
		}
		child
			.remove_transaction_id_unless_sender(Some(user))
			.ok()?;
		// Match latest_event's edit fold without recursively embedding nested
		// threads or carrying unvalidated stored relation bundles.
		if let Some(unsigned) = &child.unsigned {
			let mut unsigned: Value = serde_json::from_str(unsigned.json().get()).ok()?;
			unsigned.as_object_mut()?.remove("m.relations");
			child.unsigned = Some(to_raw_value(&unsigned).ok()?.into());
		}
		if self.services.server.config.bundle_edit_relations
			&& let Some(mut edit) = self.newest_replacement(&child).await
			&& (tag != Tag::ThreadTitle || self.title_authorized(&edit).await)
			&& self
				.services
				.state_accessor
				.user_can_see_event(user, &edit)
				.await
			&& !self
				.services
				.state_accessor
				.erased_for(user, &edit)
				.await
		{
			edit.remove_transaction_id_unless_sender(Some(user))
				.ok()?;
			child
				.set_replacement_bundle(&edit.into_format())
				.ok()?;
		}
		if self
			.services
			.server
			.config
			.bundle_reference_relations
		{
			let references = self.references(&child).await;
			if !references.is_empty() {
				child.set_reference_bundle(&references).ok()?;
			}
		}
		return Some(child);
	}
	None
}

/// Only a moderator (power >= 50) at the event can publish title metadata.
/// Later grants cannot legitimize a forged title. Missing state fails closed.
#[implement(Service)]
async fn title_authorized(&self, title: &Pdu) -> bool {
	let Ok(state) = self
		.services
		.state
		.pdu_shortstatehash(title.event_id())
		.await
	else {
		return false;
	};
	let access = &self.services.state_accessor;
	let Ok(levels) = access
		.state_get_content::<RoomPowerLevelsEventContent>(
			state,
			&StateEventType::RoomPowerLevels,
			"",
		)
		.await
	else {
		return false;
	};
	let Ok(create) = access.get_create(title.room_id()).await else { return false };
	let Ok(version) = create.room_version() else { return false };
	let Ok(rules) = room_version::rules(&version) else { return false };
	let Ok(creators) = create.creators(&rules.authorization) else { return false };
	RoomPowerLevels::new(levels.into(), &rules.authorization, creators).for_user(title.sender())
		>= int!(50)
}
