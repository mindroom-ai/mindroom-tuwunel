use futures::{Stream, StreamExt, TryFutureExt, pin_mut};
use ruma::{
	EventId, OwnedEventId, UserId,
	events::{
		AnySyncMessageLikeEvent, TimelineEventType,
		ignored_user_list::IgnoredUserListEventContent,
	},
	serde::Raw,
};
use serde_json::{Value, value::to_raw_value};
use tuwunel_core::{
	Result,
	arrayvec::ArrayVec,
	implement,
	result::LogErr,
	utils::{
		BoolExt,
		stream::{ReadyExt, TryIgnore},
		u64_from_u8,
	},
};
use tuwunel_matrix::{Event, Pdu, PduCount, PduId, RawPduId};

use super::{
	Service, is_ignored_event,
	typed_relations::{CHILD_COUNT_OFFSET, KEY_LEN, Tag, prefix},
};
use crate::users::is_ignored;

type Seek = ArrayVec<u8, KEY_LEN>;

/// Fold read-time bundled aggregations into a served event's `unsigned`,
/// per-requester.
///
/// MSC3440: the stored `m.thread` bundle is rebuilt for a
/// `sender_user` who ignores a participant or may not see its latest reply.
/// MSC3816: the bundle's shared `current_user_participated` is recomputed for
/// `sender_user`. MSC3925:
/// when `bundle_edit_relations` is enabled, the newest `m.replace` edit is
/// folded in as the full replacement event, and the bundled thread
/// `latest_event` carries its own newest edit (MSC3856). MSC3267: when
/// `bundle_reference_relations` is enabled, the `m.reference` children are
/// folded in as a `{ chunk: [{ event_id }] }` summary. The thread presence gate
/// keeps the common no-bundle case to a substring scan; the edit and reference
/// folds are skipped unless enabled.
#[implement(Service)]
#[tracing::instrument(skip_all, level = "trace")]
pub async fn bundle_aggregations(&self, sender_user: &UserId, mut pdu: Pdu) -> Pdu {
	// MSC4025: an erased sender's event serves as the pruned clone, and a
	// pruned event carries no aggregations.
	if let Some(pruned) = self
		.services
		.state_accessor
		.erased_view(sender_user, &pdu)
		.await
	{
		return pruned;
	}

	let has_thread = pdu.has_thread_bundle().log_err().unwrap_or(true);

	self.bundle_thread_for(sender_user, &mut pdu)
		.await;

	if has_thread {
		self.bundle_thread_previews(sender_user, &mut pdu)
			.await
			.log_err()
			.ok();
	}

	let replacement = self
		.services
		.server
		.config
		.rooms
		.relations
		.bundle_edits
		.then_async(|| self.newest_replacement(sender_user, &pdu))
		.await
		.flatten();

	if let Some(replacement) = replacement {
		let replacement = replacement.served_unfolded_to(Some(sender_user));

		pdu.set_replacement_bundle(&replacement.into_format())
			.log_err()
			.ok();
	}

	let references = self
		.services
		.server
		.config
		.rooms
		.relations
		.bundle_references
		.then_async(|| self.references(&pdu))
		.await
		.unwrap_or_default();

	if !references.is_empty() {
		pdu.set_reference_bundle(&references)
			.log_err()
			.ok();
	}

	pdu
}

/// Readies an event for a requester on a path that folds no aggregations.
///
/// MSC4025: when this requester must receive an erased sender's event pruned,
/// the pruned clone is served, as in [`Service::bundle_aggregations`], or the
/// pruning error is returned rather than the event served intact. Any other
/// event loses its stored aggregations, which copy replies the requester may
/// not see, and another sender's private fields.
#[implement(Service)]
pub async fn unbundled(&self, sender_user: &UserId, pdu: Pdu) -> Result<Pdu> {
	self.services
		.state_accessor
		.try_erased_view(sender_user, &pdu)
		.await
		.map(|pruned| pruned.unwrap_or_else(|| pdu.served_unfolded_to(Some(sender_user))))
}

#[implement(Service)]
#[tracing::instrument(skip_all, level = "trace")]
async fn bundle_thread_for(&self, sender_user: &UserId, pdu: &mut Pdu) {
	if pdu
		.has_thread_bundle()
		.log_err()
		.unwrap_or(true)
		.is_false()
	{
		return;
	}

	if pdu
		.thread_latest_event()
		.log_err()
		.ok()
		.flatten()
		.is_none()
		|| pdu
			.remove_thread_latest_transaction_id_unless_sender(sender_user)
			.log_err()
			.is_err()
	{
		drop_thread_bundle(pdu);
		return;
	}

	let view = self.thread_view(sender_user, pdu).await;

	if apply_thread_view(pdu, view).is_none() {
		return;
	}

	let own_latest = self.own_thread_latest(sender_user, pdu).await;

	if apply_own_thread_latest(pdu, own_latest).is_none() {
		return;
	}

	let participated = self
		.services
		.threads
		.user_participated(pdu.event_id(), sender_user)
		.await;

	let participation = pdu
		.set_thread_participated(participated)
		.log_err();

	if thread_result_or_drop(pdu, participation).is_none() {
		return;
	}

	self.erase_thread_latest(sender_user, pdu).await;

	let _: Option<()> = self
		.services
		.server
		.config
		.rooms
		.relations
		.bundle_edits
		.then_async(|| self.bundle_thread_latest_edit(sender_user, pdu))
		.await;
}

/// The requester's view of the stored thread summary (MSC3440, MSC2675).
///
/// The stored `latest_event` and `count` cover every reply. A requester who
/// ignores a participant, or who may not see the latest reply, gets them
/// rebuilt from a bounded walk of the newest replies: the result is how many
/// replies the walk hid, and the newest reply they may see when the stored
/// latest is hidden. `None` drops the summary: no reply they may see is in
/// reach, or the summary or its root cannot be read.
#[implement(Service)]
// Unspanned: span frames on this path overflow the debug-build worker stack.
async fn thread_view(&self, sender_user: &UserId, pdu: &Pdu) -> Option<(usize, Option<Pdu>)> {
	let (latest_id, _) = pdu
		.thread_latest_event()
		.log_err()
		.ok()
		.flatten()?;

	let ignored = self
		.services
		.users
		.ignored_users(sender_user)
		.await;

	let ignored = ignored.as_ref();

	let latest_shown = match self.services.timeline.get_pdu(&latest_id).await {
		| Err(_) => false,
		| Ok(latest) => self.shown_to(sender_user, ignored, &latest).await,
	};

	let ignores_nobody = ignored.is_none_or(|list| list.ignored_users.is_empty());

	if latest_shown && ignores_nobody {
		return Some((0, None));
	}

	let root_id = self
		.services
		.timeline
		.get_pdu_id(pdu.event_id())
		.await
		.ok()?;

	if latest_shown && !self.ignores_participant(ignored, &root_id).await {
		return Some((0, None));
	}

	let (hidden, newest_shown) = self
		.walk_replies(sender_user, ignored, root_id)
		.await;

	match latest_shown {
		| true => Some((hidden, None)),
		| false => newest_shown.map(|reply| (hidden, Some(reply))),
	}
}

/// Applies a requester's view to the served thread summary.
///
/// An unchanged view serves the stored summary as is. Otherwise the swapped-in
/// reply, without the aggregations stored on it, becomes the latest event and
/// the count loses the hidden replies. The summary is dropped, never served
/// with a count alone, for no view, a missing or used-up count, or a failed
/// rewrite; `None` then tells the caller to stop.
fn apply_thread_view(pdu: &mut Pdu, view: Option<(usize, Option<Pdu>)>) -> Option<()> {
	let Some((hidden, latest)) = view else {
		drop_thread_bundle(pdu);
		return None;
	};

	if hidden == 0 && latest.is_none() {
		return Some(());
	}

	if let Some(latest) = latest {
		replace_thread_latest(pdu, latest.without_aggregations())?;
	}

	let count = pdu.thread_count().log_err();
	let count = thread_result_or_drop(pdu, count)?
		.map(|count| count.saturating_sub(hidden))
		.filter(|&count| count > 0);

	let Some(count) = count else {
		drop_thread_bundle(pdu);
		return None;
	};

	let counted = pdu.set_thread_count(count).log_err();

	thread_result_or_drop(pdu, counted)
}

/// The requester's own latest reply, reloaded with its private fields.
///
/// The stored latest event carries none of its sender's private fields (the
/// transaction ID, MSC4140's delay ID), nor does a reply the requester's view
/// swaps in. A requester who sent the latest reply is served it unfolded: those
/// fields kept, the aggregations stored on it dropped. `None` for anyone else
/// keeps the copy in place. An unreadable summary or a reply that fails to load
/// is an error, since its sender may not be served the copy without its delay
/// ID (MSC4140).
#[implement(Service)]
// Unspanned: span frames on this path overflow the debug-build worker stack.
async fn own_thread_latest(&self, sender_user: &UserId, pdu: &Pdu) -> Result<Option<Pdu>> {
	let Some((event_id, _)) = pdu
		.thread_latest_event()
		.log_err()?
		.filter(|(_, sender)| sender.eq(sender_user))
	else {
		return Ok(None);
	};

	self.services
		.timeline
		.get_pdu(&event_id)
		.map_ok(|latest| latest.served_unfolded_to(Some(sender_user)))
		.map_ok(Some)
		.await
		.log_err()
}

/// Applies the requester's own latest reply to the served thread summary.
///
/// No reply of theirs serves the summary as is. An error or a failed rewrite
/// drops it, never serving the sender a copy without their private fields, and
/// `None` then tells the caller to stop.
fn apply_own_thread_latest(pdu: &mut Pdu, latest: Result<Option<Pdu>>) -> Option<()> {
	thread_result_or_drop(pdu, latest)?
		.map_or(Some(()), |latest| replace_thread_latest(pdu, latest))
}

/// Serves `latest` as the thread summary's latest event.
///
/// A failed rewrite drops the summary, never serving the copy it replaced, and
/// `None` then tells the caller to stop.
fn replace_thread_latest(pdu: &mut Pdu, latest: Pdu) -> Option<()> {
	let replaced = pdu
		.set_thread_latest_event(&latest.into_format())
		.log_err();

	thread_result_or_drop(pdu, replaced)
}

/// Whether one requester may be shown `pdu` inside an aggregation.
///
/// An event the requester's ignore list hides never is (MSC2675); otherwise
/// the room's history visibility decides. The ignore test runs first because
/// it reads nothing.
#[implement(Service)]
// Unspanned: span frames on this path overflow the debug-build worker stack.
async fn shown_to(
	&self,
	sender_user: &UserId,
	ignored: Option<&IgnoredUserListEventContent>,
	pdu: &Pdu,
) -> bool {
	!is_ignored_event(ignored, pdu)
		&& self
			.services
			.state_accessor
			.user_can_see_event(sender_user, pdu)
			.await
}

/// Whether the ignore list names any participant of a thread.
///
/// Unreadable participants count as ignored, so the summary is rebuilt rather
/// than served unfiltered.
#[implement(Service)]
#[tracing::instrument(skip_all, level = "trace")]
async fn ignores_participant(
	&self,
	ignored: Option<&IgnoredUserListEventContent>,
	root_id: &RawPduId,
) -> bool {
	self.services
		.threads
		.get_participants(root_id)
		.await
		.ok()
		.is_none_or(|participants| {
			participants
				.iter()
				.any(|user| is_ignored(ignored, user))
		})
}

/// Walks a thread's newest replies within the summary search limit.
///
/// Returns how many the requester may not be shown, and the newest one they
/// may. A reply that fails to load is skipped. The walk is sequential because
/// callers already serve events concurrently.
#[implement(Service)]
// Unspanned: span frames on this path overflow the debug-build worker stack.
// size firewall
#[cfg_attr(debug_assertions, tuwunel_core::async_noinline)]
async fn walk_replies<'a>(
	&'a self,
	sender_user: &'a UserId,
	ignored: Option<&'a IgnoredUserListEventContent>,
	root_id: RawPduId,
) -> (usize, Option<Pdu>) {
	let limit = self
		.services
		.server
		.config
		.rooms
		.relations
		.thread_summary_search_limit;

	self.services
		.threads
		.thread_replies(root_id, limit)
		.ignore_err()
		.fold((0, None), async |(hidden, newest): (usize, Option<Pdu>), (_, reply)| {
			let shown = self.shown_to(sender_user, ignored, &reply).await;

			match shown {
				| true => (hidden, newest.or(Some(reply))),
				| false => (hidden.saturating_add(1), newest),
			}
		})
		.await
}

/// MSC4025: the stored thread bundle carries a full `latest_event` of any
/// sender; an erased hit swaps in the pruned form for this recipient. The
/// event load and membership check run only on the erased hit.
#[implement(Service)]
#[tracing::instrument(skip_all, level = "trace")]
async fn erase_thread_latest(&self, sender_user: &UserId, pdu: &mut Pdu) {
	let identity = pdu.thread_latest_event().log_err();

	let Some((event_id, sender)) = thread_result_or_drop(pdu, identity).flatten() else {
		return;
	};

	if !self.services.users.is_erased(&sender).await {
		return;
	}

	let latest = self.services.timeline.get_pdu(&event_id).await;
	let Some(latest) = thread_result_or_drop(pdu, latest) else {
		return;
	};

	let Some(pruned) = self
		.services
		.state_accessor
		.erased_view(sender_user, &latest)
		.await
	else {
		return;
	};

	replace_thread_latest(pdu, pruned);
}

fn thread_result_or_drop<T, E>(pdu: &mut Pdu, result: Result<T, E>) -> Option<T> {
	result
		.inspect_err(|_| drop_thread_bundle(pdu))
		.ok()
}

fn drop_thread_bundle(pdu: &mut Pdu) { pdu.remove_thread_bundle().log_err().ok(); }

/// The thread module's aggregated `latest_event` (MSC3856): when the edit
/// fold is enabled, the bundled latest reply carries its own newest
/// `m.replace` edit, so thread previews track edits. Erased-sender bundles
/// stay in their pruned form.
#[implement(Service)]
#[tracing::instrument(skip_all, level = "trace")]
async fn bundle_thread_latest_edit(&self, sender_user: &UserId, pdu: &mut Pdu) {
	let identity = pdu.thread_latest_event().log_err();

	let Some((event_id, _)) = thread_result_or_drop(pdu, identity).flatten() else {
		return;
	};

	let latest_event = self
		.services
		.timeline
		.get_pdu(&event_id)
		.map_ok(|latest| latest.served_unfolded_to(Some(sender_user)))
		.await
		.log_err();

	let Some(mut latest_event) = thread_result_or_drop(pdu, latest_event) else {
		return;
	};

	if self
		.services
		.state_accessor
		.erased_for(sender_user, &latest_event)
		.await
	{
		return;
	}

	let Some(replacement_event) = self
		.newest_replacement(sender_user, &latest_event)
		.await
	else {
		return;
	};

	let replacement_event = replacement_event.served_unfolded_to(Some(sender_user));

	let replacement = latest_event
		.set_replacement_bundle(&replacement_event.into_format())
		.log_err();

	if thread_result_or_drop(pdu, replacement).is_none() {
		return;
	}

	replace_thread_latest(pdu, latest_event);
}

/// MSC3925: the newest `m.replace` edit of `parent` as a full event, or `None`
/// when `parent` is redacted, has no valid edit, or the newest valid edit is
/// hidden from this requester. An edit counts only when it shares the parent's
/// sender and type and is not itself redacted; newest is by `origin_server_ts`,
/// which the typed index sorts on.
#[implement(Service)]
#[tracing::instrument(skip_all, level = "trace")]
async fn newest_replacement(&self, sender_user: &UserId, parent: &Pdu) -> Option<Pdu> {
	if parent.is_redacted() {
		return None;
	}

	let parent_id: PduId = self
		.services
		.timeline
		.get_pdu_id(parent.event_id())
		.map_ok(Into::into)
		.await
		.ok()?;

	let replacements = self.replacement_children(parent, parent_id, usize::MAX);

	pin_mut!(replacements);
	let edit = replacements.next().await?;
	let access = &self.services.state_accessor;

	if access.erased_for(sender_user, &edit).await
		|| !access
			.user_can_see_event(sender_user, &edit)
			.await
	{
		return None;
	}

	Some(edit)
}

/// The ids of `parent`'s `m.replace` edits that the edit bundle counts, listed
/// even when `parent` is redacted.
#[implement(Service)]
#[tracing::instrument(skip(self), level = "debug")]
pub async fn replacement_ids(&self, parent: &EventId) -> Vec<OwnedEventId> {
	let Ok(parent_id) = self.services.timeline.get_pdu_id(parent).await else {
		return Vec::new();
	};

	let Ok(parent) = self
		.services
		.timeline
		.get_pdu_from_id(&parent_id)
		.await
	else {
		return Vec::new();
	};

	self.replacement_children(&parent, parent_id.into(), usize::MAX)
		.map(|child| child.event_id().to_owned())
		.collect()
		.await
}

/// Stream `parent`'s valid `m.replace` children, newest `origin_server_ts`
/// first, from the typed index. A child counts only when it shares the parent's
/// sender and type and is not itself redacted.
#[implement(Service)]
fn replacement_children<'a>(
	&'a self,
	parent: &'a Pdu,
	parent_id: PduId,
	limit: usize,
) -> impl Stream<Item = Pdu> + Send + 'a {
	let shortroomid = parent_id.shortroomid;
	let prefix = prefix(shortroomid, parent_id.count, Tag::Replace);

	let mut seek = Seek::new();

	seek.extend(prefix.iter().copied());
	seek.extend([u8::MAX; size_of::<u64>() * 2]);

	self.db
		.relatesto_typed
		.rev_raw_keys_from(seek.as_slice())
		.ignore_err()
		.ready_take_while(move |key| key.starts_with(&prefix))
		.take(limit)
		.map(|key| u64_from_u8(&key[CHILD_COUNT_OFFSET..KEY_LEN]))
		.map(PduCount::from_unsigned)
		.map(move |count| (shortroomid, count))
		.filter_map(async |(shortroomid, count)| {
			let child_id: RawPduId = PduId { shortroomid, count }.into();
			self.services
				.timeline
				.get_pdu_from_id(&child_id)
				.await
				.ok()
				.filter(|child| !child.is_redacted())
				.filter(|child| child.sender() == parent.sender())
				.filter(|child| child.kind() == parent.kind())
		})
}

/// The spec'd redacted form of a thread root whose sender the requester ignores.
///
/// Content side only (MSC3856). `None` when the ignore list does not hide the
/// root, or on a redaction failure, since serving it unredacted then matches the
/// reference implementation.
#[implement(Service)]
#[tracing::instrument(skip_all, level = "trace")]
pub async fn redacted_root(
	&self,
	ignored: Option<&IgnoredUserListEventContent>,
	root: &Pdu,
) -> Option<Pdu> {
	is_ignored_event(ignored, root)
		.then_async(async || {
			self.services
				.state
				.get_room_version_rules(root.room_id())
				.await
				.log_err()
				.ok()
				.and_then(|rules| root.redacted(&rules.redaction).log_err().ok())
		})
		.await
		.flatten()
}

const TITLE: &str = "io.mindroom.title";
const REPLY: &str = "io.mindroom.latest_reply";
const SECRET_REQUEST: &str = "io.mindroom.secret_request";
const MAX_SECRET_REQUEST_ID_LEN: usize = 512;

/// Thread relation rows, and edit rows per preview, read for the previews.
const PREVIEW_ROWS: usize = 50;

/// MindRoom previews beside the standard thread summary: the newest title (an
/// `m.notice` thread event with a truthy `io.mindroom.thread_summary`) and
/// the newest other conversational thread event (skipping folded anchored
/// `io.mindroom.secret_request` notices from same-homeserver `mindroom_*`
/// agents), each with its newest valid edit the requester may see. Unencrypted
/// rooms only; a field outside the window is omitted.
#[implement(Service)]
async fn bundle_thread_previews(&self, user: &UserId, root: &mut Pdu) -> Result {
	let services = &self.services;
	let access = &services.state_accessor;
	let config = &services.server.config;
	let mut unsigned = root.get_unsigned_as_value();
	let Some(thread) = unsigned
		.pointer_mut("/m.relations/m.thread")
		.and_then(Value::as_object_mut)
	else {
		return Ok(());
	};
	thread.remove(TITLE);
	thread.remove(REPLY);
	if !access.is_encrypted_room(root.room_id()).await
		&& let Ok(root_id) = services
			.timeline
			.get_pdu_id(root.event_id())
			.await
	{
		let ignored = services
			.users
			.ignored_users(user)
			.await
			.unwrap_or_default();
		let shortroomid = PduId::from(root_id).shortroomid;
		// Boxed: the walk stays open across the per-candidate reads below.
		let mut replies = Box::pin(
			services
				.threads
				.thread_replies(root_id, PREVIEW_ROWS)
				.ignore_err()
				.ready_filter(|(_, pdu)| {
					pdu.room_id() == root.room_id()
						&& pdu.state_key().is_none()
						&& *pdu.kind() != TimelineEventType::RoomEncrypted
				}),
		);
		while let Some((count, mut child)) = replies.next().await {
			let title = child.get_content::<Value>().is_ok_and(|content| {
				let marker = &content["io.mindroom.thread_summary"];
				// Truthy in JavaScript, as both clients test the marker. Events
				// hold no floats (canonical JSON), so 0 is the only zero.
				content["msgtype"] == "m.notice"
					&& !(marker.is_null() || *marker == false || *marker == 0 || *marker == "")
			});
			let field = if title { TITLE } else { REPLY };
			if thread.contains_key(field)
				|| ignored.ignored_users.contains_key(child.sender())
				|| config.is_forbidden_remote_server_name(child.sender().server_name())
				|| !self.preview_visible(user, &child).await
			{
				continue;
			}
			let secret_candidate = !title && is_secret_request_candidate(user, &child);
			let edit = if config.rooms.relations.bundle_edits || secret_candidate {
				let edits = self
					.replacement_children(&child, PduId { shortroomid, count }, PREVIEW_ROWS)
					.filter_map(async |edit| {
						self.preview_visible(user, &edit)
							.await
							.then_some(edit)
					});
				pin_mut!(edits);
				edits.next().await
			} else {
				None
			};
			if secret_candidate && is_folded_secret_request(user, &child, edit.as_ref()) {
				continue;
			}
			child.remove_aggregations();
			if config.rooms.relations.bundle_edits
				&& let Some(edit) = edit
			{
				let edit = edit.served_unfolded_to(Some(user));
				child.set_replacement_bundle(&edit.into_format())?;
			}
			let event: Raw<AnySyncMessageLikeEvent> = child.into_format();
			thread.insert(field.to_owned(), serde_json::to_value(event)?);
			if thread.contains_key(TITLE) && thread.contains_key(REPLY) {
				break;
			}
		}
	}
	root.unsigned = Some(to_raw_value(&unsigned)?.into());
	Ok(())
}

/// A preview child or edit is shown only when the requester may see it and
/// its sender is not erased for them.
#[implement(Service)]
async fn preview_visible(&self, user: &UserId, event: &Pdu) -> bool {
	let access = &self.services.state_accessor;
	access.user_can_see_event(user, event).await && !access.erased_for(user, event).await
}

fn is_mindroom_agent_for_viewer(sender: &UserId, viewer: &UserId) -> bool {
	sender.server_name() == viewer.server_name()
		&& sender
			.localpart()
			.as_bytes()
			.get(..9)
			.is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"mindroom_"))
}

fn is_secret_request_id(value: &Value) -> bool {
	let Some(id) = value.as_str() else {
		return false;
	};
	!id.is_empty() && id.encode_utf16().count() <= MAX_SECRET_REQUEST_ID_LEN
}

fn is_secret_request_candidate(user: &UserId, child: &Pdu) -> bool {
	if *child.kind() != TimelineEventType::RoomMessage
		|| child.is_redacted()
		|| !is_mindroom_agent_for_viewer(child.sender(), user)
	{
		return false;
	}
	let Ok(content) = child.get_content::<Value>() else {
		return false;
	};
	content
		.get(SECRET_REQUEST)
		.is_some_and(Value::is_object)
}

fn is_folded_secret_request(user: &UserId, child: &Pdu, edit: Option<&Pdu>) -> bool {
	if !is_secret_request_candidate(user, child) {
		return false;
	}
	let Ok(original) = child.get_content::<Value>() else {
		return false;
	};
	let edit_content = edit
		.filter(|edit| !edit.is_redacted())
		.and_then(|edit| edit.get_content::<Value>().ok());
	let effective = edit_content
		.as_ref()
		.and_then(|content| {
			if let Some(new_content) = content.get("m.new_content")
				&& new_content.is_object()
			{
				return Some(new_content);
			}
			content
				.get(SECRET_REQUEST)
				.is_some()
				.then_some(content)
		})
		.unwrap_or(&original);
	let Some(secret_request) = effective
		.get(SECRET_REQUEST)
		.and_then(Value::as_object)
	else {
		return false;
	};
	secret_request
		.get("status")
		.and_then(Value::as_str)
		.is_some_and(|status| matches!(status, "saved" | "declined" | "expired"))
		&& secret_request
			.get("response_event_id")
			.is_some_and(is_secret_request_id)
		&& secret_request
			.get("tool_call_id")
			.is_some_and(is_secret_request_id)
}

#[cfg(test)]
mod tests {
	use ruma::user_id;
	use serde_json::{from_str, from_value, json};

	use super::*;

	#[test]
	fn thread_latest_load_error_drops_bundle() {
		let mut pdu = member_event(&json!({
			"age": 4612,
			"m.relations": {
				"m.replace": { "event_id": "$edit:example.com" },
				"m.thread": { "count": 3 },
			},
		}));

		let latest: Option<()> = thread_result_or_drop(&mut pdu, Err("missing latest event"));

		assert!(latest.is_none(), "load failure returned a latest event");

		let unsigned = unsigned_of(&pdu);

		assert!(
			unsigned["m.relations"].get("m.thread").is_none(),
			"failed load retained the thread bundle",
		);

		assert_eq!(
			unsigned["m.relations"]["m.replace"],
			json!({ "event_id": "$edit:example.com" }),
			"failed load removed a sibling relation",
		);
		assert_eq!(unsigned["age"], 4612, "failed load removed outer unsigned data");
	}

	#[test]
	fn swapped_latest_carries_no_stored_aggregations() {
		let hidden = json!({ "event_id": "$hidden:example.com" });
		let mut root = member_event(&json!({
			"m.relations": { "m.thread": { "count": 2, "latest_event": hidden } },
		}));

		// A reply rooting a thread of its own, as an older release allowed.
		let latest = member_event(&json!({ "m.relations": { "m.thread": { "count": 1 } } }));

		assert!(apply_thread_view(&mut root, Some((1, Some(latest)))).is_some());

		let thread = &unsigned_of(&root)["m.relations"]["m.thread"];

		assert_eq!(thread["count"], 1);
		assert_eq!(thread["latest_event"]["event_id"], "$member:example.com");
		assert!(
			thread["latest_event"].get("unsigned").is_none(),
			"the swapped-in reply kept its stored aggregations",
		);
	}

	fn member_event(unsigned: &Value) -> Pdu {
		from_value(json!({
			"type": "m.room.member",
			"content": { "membership": "join" },
			"event_id": "$member:example.com",
			"room_id": "!room:example.com",
			"sender": "@alice:example.com",
			"state_key": "@alice:example.com",
			"prev_events": ["$prev:example.com"],
			"auth_events": ["$auth:example.com"],
			"origin_server_ts": 1_838_188_000,
			"depth": 12,
			"hashes": { "sha256": "thishashcoversallfieldsincasethisisredacted" },
			"unsigned": unsigned,
		}))
		.expect("test fixture should deserialize as a valid PDU")
	}

	#[test]
	fn folded_secret_request_accepts_terminal_edits_and_case_insensitive_agent() {
		let viewer = user_id!("@alice:example.com");
		let agent = "@mindroom_helper:example.com";
		let empty = json!({});
		let pending_payload = secret_payload("pending", "$resp:example.com", "call-1");
		let pending = message_event(
			agent,
			"m.room.message",
			&json!({
				"msgtype": "m.notice",
				"body": "Secret requested",
				"io.mindroom.secret_request": pending_payload,
			}),
			&empty,
		);

		for status in ["saved", "declined", "expired"] {
			let payload = secret_payload(status, "$resp:example.com", "call-1");
			let edit = message_event(
				agent,
				"m.room.message",
				&json!({
					"msgtype": "m.notice",
					"body": "* Secret updated",
					"m.new_content": {
						"msgtype": "m.notice",
						"body": "Secret updated",
						"io.mindroom.secret_request": payload,
					},
				}),
				&empty,
			);
			assert!(
				is_folded_secret_request(viewer, &pending, Some(&edit)),
				"{status} edit via m.new_content should fold"
			);
		}

		let saved_payload = secret_payload("saved", "$resp:example.com", "call-1");
		let direct_edit = message_event(
			agent,
			"m.room.message",
			&json!({
				"msgtype": "m.notice",
				"body": "* Secret saved",
				"io.mindroom.secret_request": saved_payload,
			}),
			&empty,
		);
		assert!(
			is_folded_secret_request(viewer, &pending, Some(&direct_edit)),
			"top-level edit fallback should fold when m.new_content is absent"
		);

		let max_ascii = "$".repeat(512);
		let max_emoji = "\u{1F600}".repeat(256);
		let boundary_payload = secret_payload("saved", &max_ascii, &max_emoji);
		let saved_original = message_event(
			"@MindRoom_Helper:example.com",
			"m.room.message",
			&json!({
				"msgtype": "m.notice",
				"body": "Secret saved",
				"io.mindroom.secret_request": boundary_payload,
			}),
			&empty,
		);
		assert!(
			is_folded_secret_request(viewer, &saved_original, None),
			"uppercase MindRoom_ prefix and 512 UTF-16 anchor IDs should fold"
		);

		let unrelated_edit = message_event(
			"@MindRoom_Helper:example.com",
			"m.room.message",
			&json!({ "msgtype": "m.notice", "body": "* plain fallback" }),
			&empty,
		);
		assert!(
			is_folded_secret_request(viewer, &saved_original, Some(&unrelated_edit)),
			"edit without secret request fields should fall back to original"
		);
	}

	#[test]
	fn folded_secret_request_rejects_pending_and_invalid_anchors() {
		let viewer = user_id!("@alice:example.com");
		let agent = "@mindroom_helper:example.com";
		let empty = json!({});
		let over_ascii = secret_payload("saved", &"a".repeat(513), "call-1");
		let over_emoji_id = "\u{1F600}".repeat(257);
		let over_emoji = secret_payload("saved", "$resp:example.com", &over_emoji_id);
		let cases = [
			(
				"pending with past expires_at",
				json!({
					"status": "pending",
					"response_event_id": "$resp:example.com",
					"tool_call_id": "call-1",
					"expires_at": 1,
				}),
			),
			(
				"missing response_event_id",
				json!({
					"status": "saved",
					"tool_call_id": "call-1",
				}),
			),
			(
				"missing tool_call_id",
				json!({
					"status": "saved",
					"response_event_id": "$resp:example.com",
				}),
			),
			(
				"non-string anchor id",
				json!({
					"status": "saved",
					"response_event_id": 42,
					"tool_call_id": "call-1",
				}),
			),
			("empty response_event_id", secret_payload("saved", "", "call-1")),
			("empty tool_call_id", secret_payload("saved", "$resp:example.com", "")),
			("ascii over 512 UTF-16", over_ascii),
			("emoji over 512 UTF-16", over_emoji),
		];

		for (label, payload) in cases {
			let child = message_event(
				agent,
				"m.room.message",
				&json!({
					"msgtype": "m.notice",
					"body": "Secret notice",
					"io.mindroom.secret_request": payload,
				}),
				&empty,
			);
			assert!(!is_folded_secret_request(viewer, &child, None), "{label} must not fold");
		}
	}

	#[test]
	fn folded_secret_request_rejects_untrusted_senders_spoofs_and_non_messages() {
		let viewer = user_id!("@alice:example.com");
		let agent = "@mindroom_helper:example.com";
		let empty = json!({});
		let payload = secret_payload("saved", "$resp:example.com", "call-1");
		let saved_content = json!({
			"msgtype": "m.notice",
			"body": "Secret saved",
			"io.mindroom.secret_request": payload,
		});

		for sender in [
			"@mindroom_helper:other.example",
			"@alice:example.com",
			"@mindroom:example.com",
		] {
			let event = message_event(sender, "m.room.message", &saved_content, &empty);
			assert!(
				!is_folded_secret_request(viewer, &event, None),
				"untrusted sender {sender} must not fold"
			);
		}

		let plain_original = message_event(
			agent,
			"m.room.message",
			&json!({ "msgtype": "m.text", "body": "ordinary reply" }),
			&empty,
		);
		let spoof_edit = message_event(
			agent,
			"m.room.message",
			&json!({
				"msgtype": "m.text",
				"body": "* spoofed",
				"m.new_content": saved_content,
			}),
			&empty,
		);
		assert!(
			!is_folded_secret_request(viewer, &plain_original, Some(&spoof_edit)),
			"edit-only spoof without original io.mindroom.secret_request must not fold"
		);

		let saved_original = message_event(agent, "m.room.message", &saved_content, &empty);
		let removing_edit = message_event(
			agent,
			"m.room.message",
			&json!({
				"msgtype": "m.notice",
				"body": "* replaced",
				"m.new_content": { "msgtype": "m.notice", "body": "replaced" },
			}),
			&empty,
		);
		assert!(
			!is_folded_secret_request(viewer, &saved_original, Some(&removing_edit)),
			"edit removing io.mindroom.secret_request must not fold"
		);

		let redacted = message_event(
			agent,
			"m.room.message",
			&saved_content,
			&json!({ "redacted_because": { "event_id": "$redaction:example.com" } }),
		);
		assert!(
			!is_folded_secret_request(viewer, &redacted, None),
			"redacted event must not fold"
		);

		let non_message = message_event(agent, "io.mindroom.notice", &saved_content, &empty);
		assert!(
			!is_folded_secret_request(viewer, &non_message, None),
			"non-m.room.message event must not fold"
		);
	}

	fn secret_payload(status: &str, response_event_id: &str, tool_call_id: &str) -> Value {
		json!({
			"status": status,
			"response_event_id": response_event_id,
			"tool_call_id": tool_call_id,
		})
	}

	fn message_event(sender: &str, kind: &str, content: &Value, unsigned: &Value) -> Pdu {
		from_value(json!({
			"type": kind,
			"content": content,
			"event_id": "$message:example.com",
			"room_id": "!room:example.com",
			"sender": sender,
			"prev_events": ["$prev:example.com"],
			"auth_events": ["$auth:example.com"],
			"origin_server_ts": 1_838_188_000,
			"depth": 12,
			"hashes": { "sha256": "thishashcoversallfieldsincasethisisredacted" },
			"unsigned": unsigned,
		}))
		.expect("test fixture should deserialize as a valid PDU")
	}

	fn unsigned_of(pdu: &Pdu) -> Value {
		let unsigned = pdu
			.unsigned
			.as_ref()
			.expect("unsigned data should remain");

		from_str(unsigned.json().get()).expect("unsigned data should be valid JSON")
	}
}
