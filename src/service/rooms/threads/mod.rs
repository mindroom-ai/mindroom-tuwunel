use std::{
	collections::BTreeMap,
	pin::pin,
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
};

use futures::{Stream, StreamExt, TryFutureExt, TryStreamExt, future::join3};
use ruma::{
	CanonicalJsonObject, CanonicalJsonValue, EventId, OwnedEventId, OwnedUserId, RoomId, UInt,
	UserId,
	api::{Direction, client::threads::get_threads::v1::IncludeThreads},
	events::{
		AnySyncMessageLikeEvent, TimelineEventType, relation::RelationType,
		room::encrypted::Relation,
	},
	serde::Raw,
	uint,
};
use serde::Deserialize;
use serde_json::json;
use tuwunel_core::{
	Event, Result, err, info,
	matrix::pdu::{PduCount, PduEvent, PduId, RawPduId},
	utils::{
		ReadyExt,
		result::NotFound,
		stream::{TryIgnore, TryReadyExt, WidebandExt, automatic_width},
	},
	warn,
};
use tuwunel_database::{Deserialized, Map, Txn};

use crate::rooms::timeline::RoomMutexGuard;

#[cfg(test)]
mod tests;

/// Maximum relation hops walked when resolving thread membership, per
/// the Matrix v1.4 spec recommendation (also MSC3771/MSC3773).
const MAX_THREAD_HOPS: usize = 3;

/// Thread roots a recount checks between progress reports.
const RECOUNT_PROGRESS_INTERVAL: usize = 1000;

/// What [`Service::recount_thread_replies`] did.
#[derive(Clone, Copy, Debug, Default)]
pub struct ThreadRecount {
	/// Thread roots visited.
	pub checked: usize,
	/// Roots whose stored count was rewritten.
	pub changed: usize,
	/// Roots left unchanged because one of their events could not be read.
	pub failed: usize,
}

#[derive(Deserialize)]
struct ExtractThreadRelation {
	#[serde(rename = "m.relates_to")]
	relates_to: ThreadRelation,
}

#[derive(Deserialize)]
struct ThreadRelation {
	rel_type: RelationType,
	event_id: OwnedEventId,
}

/// The relation `append_pdu_effects` reads before `add_to_thread` counts a
/// reply.
#[derive(Deserialize)]
struct ExtractRelatesTo {
	#[serde(rename = "m.relates_to")]
	relates_to: Relation,
}

fn canonical_object_field<'a>(
	object: &'a mut CanonicalJsonObject,
	field: &str,
) -> &'a mut CanonicalJsonObject {
	if !matches!(object.get(field), Some(CanonicalJsonValue::Object(_))) {
		object.insert(field.into(), CanonicalJsonValue::Object(BTreeMap::new()));
	}

	let Some(CanonicalJsonValue::Object(value)) = object.get_mut(field) else {
		unreachable!("canonical object field was initialized as an object");
	};

	value
}

/// Persist a latest event whose embedded sender and event ID come from the
/// same validated event used to index the thread activity.
fn update_thread_bundle<E>(unsigned: &mut CanonicalJsonObject, event: &E)
where
	E: Event,
{
	let latest_event = event.to_sync_message_like_without_unsigned();
	update_thread_bundle_raw(unsigned, &latest_event);
}

fn update_thread_bundle_raw(
	unsigned: &mut CanonicalJsonObject,
	latest_event: &Raw<AnySyncMessageLikeEvent>,
) {
	let relations = canonical_object_field(unsigned, "m.relations");
	let thread = canonical_object_field(relations, "m.thread");
	let count = thread
		.get("count")
		.cloned()
		.and_then(|count| serde_json::from_value::<UInt>(count.into()).ok());

	let count = count.map_or_else(|| uint!(1), |count| count.saturating_add(uint!(1)));
	let latest_event = serde_json::from_str(latest_event.json().get())
		.expect("thread latest event should be canonical JSON");

	thread.insert("latest_event".into(), latest_event);
	thread.insert(
		"count".into(),
		json!(count)
			.try_into()
			.expect("thread count is canonical JSON"),
	);

	if !matches!(thread.get("current_user_participated"), Some(CanonicalJsonValue::Bool(_))) {
		thread.insert("current_user_participated".into(), CanonicalJsonValue::Bool(true));
	}
}

/// The `m.thread` bundle stored on a thread root, if it has one.
fn thread_bundle(root: &mut CanonicalJsonObject) -> Option<&mut CanonicalJsonObject> {
	let Some(CanonicalJsonValue::Object(unsigned)) = root.get_mut("unsigned") else {
		return None;
	};

	let Some(CanonicalJsonValue::Object(relations)) = unsigned.get_mut("m.relations") else {
		return None;
	};

	let Some(CanonicalJsonValue::Object(thread)) = relations.get_mut("m.thread") else {
		return None;
	};

	Some(thread)
}

fn thread_count(thread: &CanonicalJsonObject) -> Option<UInt> {
	thread
		.get("count")
		.cloned()
		.and_then(|count| serde_json::from_value(count.into()).ok())
}

/// Set the root's bundled reply count, returning whether it changed. A root
/// without a thread bundle is left alone.
fn set_thread_count(root: &mut CanonicalJsonObject, count: UInt) -> bool {
	let Some(thread) = thread_bundle(root) else {
		return false;
	};

	if thread_count(thread) == Some(count) {
		return false;
	}

	thread.insert("count".into(), CanonicalJsonValue::Integer(count.into()));

	true
}

fn is_thread_reply(pdu: &PduEvent) -> bool {
	pdu.get_content()
		.is_ok_and(|content: ExtractRelatesTo| matches!(content.relates_to, Relation::Thread(_)))
}

pub struct Service {
	db: Data,
	services: Arc<crate::services::OnceServices>,
}

pub(super) struct Data {
	threadid_userids: Arc<Map>,
	threadactivityid_rootid: Arc<Map>,
	threadrootid_latestcount: Arc<Map>,
}

impl crate::Service for Service {
	fn build(args: &crate::Args<'_>) -> Result<Arc<Self>> {
		Ok(Arc::new(Self {
			db: Data {
				threadid_userids: args.db["threadid_userids"].clone(),
				threadactivityid_rootid: args.db["threadactivityid_rootid"].clone(),
				threadrootid_latestcount: args.db["threadrootid_latestcount"].clone(),
			},
			services: args.services.clone(),
		}))
	}

	fn name(&self) -> &str { crate::service::make_name(std::module_path!()) }
}

impl Service {
	/// Resolves the thread root for `event` by walking up `m.relates_to`
	/// links, bounded at `MAX_THREAD_HOPS`. Returns `None` for events
	/// that belong to the main timeline. Redaction events carry no
	/// `m.relates_to` of their own; their thread is resolved from the
	/// redacted target event per MSC3771/MSC3773.
	pub async fn get_thread_id<E>(&self, event: &E) -> Option<OwnedEventId>
	where
		E: Event,
	{
		let initial = match event.get_content::<ExtractThreadRelation>() {
			| Ok(t) => Some(t.relates_to),
			| Err(_) => self.relates_to_via_redaction_target(event).await,
		};

		let mut relates_to = initial?;

		for _ in 0..MAX_THREAD_HOPS {
			if relates_to.rel_type == RelationType::Thread {
				return Some(relates_to.event_id);
			}

			relates_to = self
				.services
				.timeline
				.get_pdu(&relates_to.event_id)
				.await
				.ok()?
				.get_content::<ExtractThreadRelation>()
				.ok()?
				.relates_to;
		}

		None
	}

	/// Resolve a redaction event's thread by looking through to the
	/// redacted target. Returns `None` for non-redaction events and for
	/// redactions whose target is unknown or carries no thread relation.
	async fn relates_to_via_redaction_target<E>(&self, event: &E) -> Option<ThreadRelation>
	where
		E: Event,
	{
		if *event.kind() != TimelineEventType::RoomRedaction {
			return None;
		}

		let room_rules = self
			.services
			.state
			.get_room_version_rules(event.room_id())
			.await
			.ok()?;

		let target_id = event.redacts_id(&room_rules)?;

		self.services
			.timeline
			.get_pdu(&target_id)
			.await
			.ok()?
			.get_content::<ExtractThreadRelation>()
			.ok()
			.map(|t| t.relates_to)
	}

	/// `get_thread_id` for an event referenced by id; events missing
	/// locally resolve to `None` (the main timeline).
	pub async fn get_thread_id_for_event(&self, event_id: &EventId) -> Option<OwnedEventId> {
		let pdu = self
			.services
			.timeline
			.get_pdu(event_id)
			.await
			.ok()?;

		self.get_thread_id(&pdu).await
	}

	pub async fn add_to_thread<E>(
		&self,
		root_event_id: &EventId,
		pdu_id: RawPduId,
		event: &E,
	) -> Result
	where
		E: Event,
	{
		let root_id = self
			.services
			.timeline
			.get_pdu_id(root_event_id)
			.await
			.map_err(|e| {
				err!(Request(InvalidParam("Invalid event_id in thread message: {e:?}")))
			})?;

		let root_pdu = self
			.services
			.timeline
			.get_pdu_from_id(&root_id)
			.await
			.map_err(|e| err!(Request(InvalidParam("Thread root not found: {e:?}"))))?;

		if root_pdu.room_id() != event.room_id() {
			return Ok(());
		}

		let mut root_pdu_json = self
			.services
			.timeline
			.get_pdu_json_from_id(&root_id)
			.await
			.map_err(|e| err!(Request(InvalidParam("Thread root pdu not found: {e:?}"))))?;

		let mut users = self
			.get_participants(&root_id)
			.await
			.unwrap_or_else(|_| vec![root_pdu.sender().to_owned()]);

		users.push(event.sender().to_owned());

		let mut txn = self.services.db.txn();

		self.update_participants(&mut txn, &root_id, &users);

		let count = pdu_id.pdu_count();

		if matches!(count, PduCount::Normal(_)) {
			txn.insert_raw(&self.db.threadactivityid_rootid, pdu_id, root_id);
			txn.insert_raw(&self.db.threadrootid_latestcount, root_id, count.to_be_bytes());
		}

		if let CanonicalJsonValue::Object(unsigned) = root_pdu_json
			.entry("unsigned".into())
			.or_insert_with(|| CanonicalJsonValue::Object(BTreeMap::default()))
		{
			update_thread_bundle(unsigned, event);

			self.services
				.timeline
				.stage_replace_pdu(&mut txn, &root_id, &root_pdu_json);
		}

		txn.execute();
		Ok(())
	}

	/// Stage the root's bundled `m.thread.count` for the redaction of one of
	/// its replies, in the transaction that stages the redacted reply, so the
	/// two are written together. The caller reads `root_event_id` from the
	/// reply before redacting it; an already redacted reply names no root.
	///
	/// The count is corrected as the recount corrects it, leaving the reply
	/// out. A root in another room was never counted for this reply and is
	/// left alone. Nothing is staged on error.
	pub async fn stage_reply_redaction(
		&self,
		txn: &mut Txn,
		root_event_id: &EventId,
		reply_id: &RawPduId,
		_state_lock: &RoomMutexGuard,
	) -> Result {
		let Some(root_id) = self
			.services
			.timeline
			.get_pdu_id(root_event_id)
			.await
			.optional()?
		else {
			return Ok(());
		};

		if root_id.shortroomid() != reply_id.shortroomid() {
			return Ok(());
		}

		self.stage_thread_count(txn, root_id, Some(reply_id.pdu_count()))
			.await?;

		Ok(())
	}

	/// Stage a corrected `m.thread.count` for a thread root, returning whether
	/// it changed. The caller holds the root's room state lock.
	///
	/// A timeline root is recounted from the thread replies in its relation
	/// index, leaving out `redacted`, a reply whose redaction is staged in the
	/// same transaction and not yet visible. Replies to a backfilled root are
	/// not in that index, so its count only drops by one for a redacted reply
	/// that `add_to_thread` counted, stopping at zero. A root without a thread
	/// bundle (itself redacted) is left alone. The relation rows and
	/// `latest_event` are kept. Nothing is staged on error.
	async fn stage_thread_count(
		&self,
		txn: &mut Txn,
		root_id: RawPduId,
		redacted: Option<PduCount>,
	) -> Result<bool> {
		let Some(mut root_pdu_json) = self
			.services
			.timeline
			.get_pdu_json_from_id(&root_id)
			.await
			.optional()?
		else {
			return Ok(false);
		};

		let Some(stored) = thread_bundle(&mut root_pdu_json).map(|thread| thread_count(thread))
		else {
			return Ok(false);
		};

		let root: PduId = root_id.into();
		let count = match (root.count, redacted) {
			| (PduCount::Normal(_), redacted) =>
				self.count_thread_replies(root, redacted).await?,
			| (PduCount::Backfilled(_), Some(PduCount::Normal(_))) => {
				let Some(count) = stored.and_then(|count| count.checked_sub(uint!(1))) else {
					return Ok(false);
				};

				count
			},
			| (PduCount::Backfilled(_), _) => return Ok(false),
		};

		if !set_thread_count(&mut root_pdu_json, count) {
			return Ok(false);
		}

		self.services
			.timeline
			.stage_replace_pdu(txn, &root_id, &root_pdu_json);

		Ok(true)
	}

	/// The thread replies in the relation index of `root`, a timeline root,
	/// leaving out `except`. Fails on a read error rather than count a partial
	/// walk.
	async fn count_thread_replies(&self, root: PduId, except: Option<PduCount>) -> Result<UInt> {
		let replies = self
			.services
			.pdu_metadata
			.try_get_relations(root.shortroomid, root.count)
			.ready_try_fold(0_usize, |replies, (count, pdu)| {
				Ok(if Some(count) != except && is_thread_reply(&pdu) {
					replies.saturating_add(1)
				} else {
					replies
				})
			})
			.await?;

		Ok(replies.try_into()?)
	}

	pub fn threads_until<'a>(
		&'a self,
		user_id: &'a UserId,
		room_id: &'a RoomId,
		count: PduCount,
		include: &'a IncludeThreads,
	) -> impl Stream<Item = Result<(PduCount, PduEvent)>> + Send {
		let participated = matches!(include, IncludeThreads::Participated);

		self.services
			.short
			.get_shortroomid(room_id)
			.map_ok(move |shortroomid| PduId {
				shortroomid,
				count: count.saturating_sub(1),
			})
			.map_ok(Into::into)
			.map_ok(move |current: RawPduId| {
				self.db
					.threadactivityid_rootid
					.rev_raw_stream_from(&current)
					.ignore_err()
					.map(|(key, root_id)| (RawPduId::from(key), RawPduId::from(root_id)))
					.ready_take_while(move |(activity_id, _)| {
						activity_id.shortroomid() == current.shortroomid()
					})
					.map(move |(activity_id, root_id)| {
						(activity_id, root_id, user_id, participated)
					})
					.wide_filter_map(async |(activity_id, root_id, user_id, participated)| {
						self.live_thread(user_id, participated, activity_id, root_id)
							.await
					})
					.map(Ok)
			})
			.try_flatten_stream()
	}

	/// Resolve one activity row to its thread root, skipping and reaping rows
	/// the validity pointer has left behind.
	async fn live_thread(
		&self,
		user_id: &UserId,
		participated: bool,
		activity_id: RawPduId,
		root_id: RawPduId,
	) -> Option<(PduCount, PduEvent)> {
		let count = activity_id.pdu_count();

		let pointer = self
			.db
			.threadrootid_latestcount
			.get(&root_id)
			.await
			.deserialized()
			.map(PduCount::from_unsigned)
			.ok()?;

		if count != pointer {
			// A row ahead of the pointer is a write in flight; only rows behind
			// the pointer are dead and safe to reap.
			if count < pointer {
				self.db
					.threadactivityid_rootid
					.remove(&activity_id);
			}

			return None;
		}

		if participated && !self.is_participant(&root_id, user_id).await {
			return None;
		}

		let mut pdu = self
			.services
			.timeline
			.get_pdu_from_id(&root_id)
			.await
			.ok()?;

		pdu.remove_transaction_id_unless_sender(Some(user_id))
			.ok()?;

		Some((count, pdu))
	}

	async fn is_participant(&self, root_id: &RawPduId, user_id: &UserId) -> bool {
		self.db
			.threadid_userids
			.get(root_id)
			.await
			.is_ok_and(|participants| {
				participants
					.split(|&byte| byte == 0xFF)
					.any(|user| user == user_id.as_bytes())
			})
	}

	pub(super) fn update_participants(
		&self,
		txn: &mut Txn,
		root_id: &RawPduId,
		participants: &[OwnedUserId],
	) {
		let users = participants
			.iter()
			.map(|user| user.as_bytes())
			.collect::<Vec<_>>()
			.join(&[0xFF][..]);

		txn.insert_raw(&self.db.threadid_userids, root_id, &users);
	}

	pub(super) async fn get_participants(&self, root_id: &RawPduId) -> Result<Vec<OwnedUserId>> {
		self.db
			.threadid_userids
			.get(root_id)
			.await
			.deserialized()
	}

	/// MSC3816: whether `user_id` has participated in the thread rooted at
	/// `root_event_id`, having sent the root event or a threaded reply to it.
	pub async fn user_participated(&self, root_event_id: &EventId, user_id: &UserId) -> bool {
		let Ok(root_id) = self
			.services
			.timeline
			.get_pdu_id(root_event_id)
			.await
		else {
			return false;
		};

		self.is_participant(&root_id, user_id).await
	}

	#[tracing::instrument(skip(self), level = "debug")]
	pub(super) async fn delete_all_rooms_threads(&self, room_id: &RoomId) -> Result {
		let Ok(shortroomid) = self.services.short.get_shortroomid(room_id).await else {
			return Ok(());
		};

		join3(
			self.db.threadid_userids.del_prefix(&shortroomid),
			self.db
				.threadactivityid_rootid
				.del_prefix(&shortroomid),
			self.db
				.threadrootid_latestcount
				.del_prefix(&shortroomid),
		)
		.await;

		Ok(())
	}

	/// Rebuild the thread activity index from every thread root. Run once at
	/// startup behind a `global` marker, and on demand from the admin command.
	/// Clears first so a partial or stale index is replaced wholesale.
	pub async fn rebuild_thread_activity(&self) -> Result {
		self.db.threadactivityid_rootid.clear().await;
		self.db.threadrootid_latestcount.clear().await;

		self.db
			.threadid_userids
			.raw_keys()
			.ignore_err()
			.map(RawPduId::from)
			.for_each_concurrent(automatic_width(), async |root_id| {
				self.index_thread_activity(root_id).await;
			})
			.await;

		Ok(())
	}

	async fn index_thread_activity(&self, root_id: RawPduId) {
		let root: PduId = root_id.into();

		let replies = self
			.services
			.pdu_metadata
			.get_relations(root.shortroomid, root.count, None, Direction::Backward, None)
			.ready_filter_map(|(count, pdu)| {
				pdu.get_content()
					.is_ok_and(|content: ExtractThreadRelation| {
						content.relates_to.rel_type == RelationType::Thread
					})
					.then_some(count)
			});

		let mut replies = pin!(replies);

		let latest = replies.next().await.unwrap_or(root.count);

		let activity_id: RawPduId = PduId {
			shortroomid: root.shortroomid,
			count: latest,
		}
		.into();

		let mut txn = self.services.db.txn();

		txn.insert_raw(&self.db.threadactivityid_rootid, activity_id, root_id);
		txn.insert_raw(&self.db.threadrootid_latestcount, root_id, latest.to_be_bytes());
		txn.execute();
	}

	/// Recount every thread root's bundled `m.thread.count` from the thread
	/// replies in its relation index, rewriting only counts that differ.
	/// Counts kept before redaction updated them still include redacted
	/// replies, which no longer carry `m.relates_to`. A root with an event that
	/// cannot be read is logged, counted as failed and left unchanged. Stops
	/// with an error at shutdown or if the roots cannot be listed. Run once at
	/// startup behind a `global` marker, and on demand from the admin command.
	pub async fn recount_thread_replies(&self) -> Result<ThreadRecount> {
		let checked = AtomicUsize::new(0);
		let changed = AtomicUsize::new(0);
		let failed = AtomicUsize::new(0);

		self.db
			.threadid_userids
			.raw_keys()
			.map_ok(RawPduId::from)
			.try_for_each_concurrent(automatic_width(), async |root_id| {
				self.services.server.check_running()?;

				match self.recount_thread(root_id).await {
					| Ok(true) => {
						changed.fetch_add(1, Ordering::Relaxed);
					},
					| Ok(false) => {},
					| Err(error) => {
						failed.fetch_add(1, Ordering::Relaxed);
						warn!(?root_id, %error, "Thread replies could not be recounted");
					},
				}

				let checked = checked
					.fetch_add(1, Ordering::Relaxed)
					.saturating_add(1);

				if checked.is_multiple_of(RECOUNT_PROGRESS_INTERVAL) {
					let changed = changed.load(Ordering::Relaxed);
					info!(checked, changed, "Recounting thread replies");
				}

				Ok(())
			})
			.await?;

		let recount = ThreadRecount {
			checked: checked.into_inner(),
			changed: changed.into_inner(),
			failed: failed.into_inner(),
		};

		info!(?recount, "Recounted thread replies");

		Ok(recount)
	}

	async fn recount_thread(&self, root_id: RawPduId) -> Result<bool> {
		// Replies to a backfilled root are not in the relation index.
		if !matches!(root_id.pdu_count(), PduCount::Normal(_)) {
			return Ok(false);
		}

		let Some(root_pdu) = self
			.services
			.timeline
			.get_pdu_from_id(&root_id)
			.await
			.optional()?
		else {
			return Ok(false);
		};

		// Appends and redactions rewrite the root under this lock too.
		let _lock = self
			.services
			.state
			.mutex
			.lock(root_pdu.room_id())
			.await;

		let mut txn = self.services.db.txn();

		if !self
			.stage_thread_count(&mut txn, root_id, None)
			.await?
		{
			return Ok(false);
		}

		txn.execute();

		Ok(true)
	}
}
