use std::cmp::Reverse;

use futures::{FutureExt, Stream, StreamExt, TryFutureExt, TryStreamExt, stream};
use ruma::{
	EventId, OwnedUserId, UserId,
	api::Direction,
	events::{reaction::ReactionEventContent, relation::RelationType},
};
use tuwunel_core::{
	PduId, Result,
	arrayvec::ArrayVec,
	debug_warn, implement, is_equal_to,
	matrix::{Event, Pdu, PduCount, RawPduId, event::RelationTypeEqual},
	utils::{
		result::NotFound,
		stream::{ReadyExt, TryIgnore, TryReadyExt, TryWidebandExt},
		u64_from_u8,
	},
};

use super::Service;
use crate::rooms::short::ShortRoomId;

type StartKey = ArrayVec<u8, 16>;

/// Most relation rows one recursive walk reads.
const RECURSIVE_ROW_LIMIT: usize = 65_536;

/// Parameters controlling a relation-row query.
///
/// The row limit applies before event loading and filtering.
pub struct RelationQuery<'a> {
	/// Numeric room identifier containing the relation rows.
	pub(crate) shortroomid: ShortRoomId,

	/// Event position targeted by each relation row.
	pub(crate) target: PduCount,

	/// Optional exclusive cursor for the first relation row.
	pub(crate) from: Option<PduCount>,

	/// Direction in which relation rows are visited.
	pub(crate) dir: Direction,

	/// Requesting user used to filter transaction identifiers.
	pub(crate) user_id: Option<&'a UserId>,

	/// Maximum number of raw relation rows to visit.
	pub(crate) limit: usize,
}

#[implement(Service)]
#[tracing::instrument(skip(self, from, to), level = "debug")]
pub fn add_relation(&self, from: PduCount, to: PduCount) {
	const BUFSIZE: usize = size_of::<u64>() * 2;

	match (from, to) {
		| (PduCount::Normal(from), PduCount::Normal(to)) => {
			let key: &[u64] = &[to, from];

			self.db
				.tofrom_relation
				.aput_raw::<BUFSIZE, _, _>(key, []);
		},
		| _ => {}, // TODO: Relations with backfilled pdus
	}
}

/// Query relations of an event to determine if matching any of the trailing
/// arguments. When all criteria are None the mere presence of a relation causes
/// this function to return true.
#[implement(Service)]
pub async fn event_has_relation(
	&self,
	event_id: &EventId,
	user_id: Option<&UserId>,
	rel_type: Option<&RelationType>,
	key: Option<&str>,
) -> bool {
	let Ok(pdu_id) = self.services.timeline.get_pdu_id(event_id).await else {
		return false;
	};

	self.has_relation(pdu_id.into(), user_id, rel_type, key)
		.await
}

/// Query relations of an event by PduId to determine if matching any of the
/// trailing arguments. When all criteria are None the mere presence of a
/// relation causes this function to return true.
#[implement(Service)]
pub async fn has_relation(
	&self,
	target: PduId,
	user_id: Option<&UserId>,
	rel_type: Option<&RelationType>,
	key: Option<&str>,
) -> bool {
	self.get_relations(target.shortroomid, target.count, None, Direction::Forward, None)
		.ready_filter(|(_, pdu)| user_id.is_none_or(is_equal_to!(pdu.sender())))
		.ready_filter(|(_, pdu)| {
			debug_assert!(
				key.is_none() || rel_type.is_none_or(is_equal_to!(&RelationType::Annotation)),
				"key argument only applies to Annotation type relations."
			);

			// When key is supplied we don't need to double-parse the content here and
			// below.
			key.is_some() || rel_type.is_none_or(|rel_type| rel_type.relation_type_equal(&pdu))
		})
		.ready_filter(|(_, pdu)| {
			key.is_none_or(|key| {
				pdu.get_content()
					.map(|content: ReactionEventContent| content.relates_to.key == key)
					.unwrap_or(false)
			})
		})
		.ready_any(|_| true)
		.await
}

/// MSC3440 `related_by_*`: whether any event relates to `target` with a
/// `rel_type` in `rel_types` and a `sender` in `senders`. An empty list is
/// unconstrained on that axis; a single relating event must satisfy both.
#[implement(Service)]
pub async fn has_incoming_relation(
	&self,
	target: PduId,
	senders: &[OwnedUserId],
	rel_types: &[RelationType],
) -> bool {
	self.get_relations(target.shortroomid, target.count, None, Direction::Forward, None)
		.ready_any(|(_, pdu)| {
			let sender_matches =
				senders.is_empty() || senders.iter().any(is_equal_to!(pdu.sender()));

			let rel_type_matches = rel_types.is_empty()
				|| rel_types
					.iter()
					.any(|rel_type| rel_type.relation_type_equal(&pdu));

			sender_matches && rel_type_matches
		})
		.await
}

#[implement(Service)]
pub fn get_relations<'a>(
	&'a self,
	shortroomid: ShortRoomId,
	target: PduCount,
	from: Option<PduCount>,
	dir: Direction,
	user_id: Option<&'a UserId>,
) -> impl Stream<Item = (PduCount, Pdu)> + Send + '_ {
	self.try_get_relations(shortroomid, target, from, dir, user_id)
		.ignore_err()
}

/// Walks related events in the requested direction, preserving read failures.
///
/// Missing children are skipped; other load and transaction-ID errors are yielded.
#[implement(Service)]
pub fn try_get_relations<'a>(
	&'a self,
	shortroomid: ShortRoomId,
	target: PduCount,
	from: Option<PduCount>,
	dir: Direction,
	user_id: Option<&'a UserId>,
) -> impl Stream<Item = Result<(PduCount, Pdu)>> + Send + '_ {
	self.try_get_relations_limited(RelationQuery {
		shortroomid,
		target,
		from,
		dir,
		user_id,
		limit: usize::MAX,
	})
}

/// Walks at most `limit` relation rows, preserving read failures.
///
/// The limit counts missing children before event loading and filtering.
#[implement(Service)]
pub fn try_get_relations_limited<'a>(
	&'a self,
	RelationQuery {
		shortroomid,
		target,
		from,
		dir,
		user_id,
		limit,
	}: RelationQuery<'a>,
) -> impl Stream<Item = Result<(PduCount, Pdu)>> + Send + '_ {
	self.relation_counts(target, from, dir)
		.take(limit)
		.wide_and_then(move |count| self.load_relation(shortroomid, count, user_id))
		.ready_try_filter_map(Ok)
}

/// Walks the events relating to `target` directly, at depth zero, and through
/// up to `max_depth` further relations, in `dir` order after `from`.
///
/// The positions of the whole tree are read from the relation rows before any
/// event is loaded, so `from` applies to every depth alike and the walk is in
/// position order however deep each event is. At most
/// `RECURSIVE_ROW_LIMIT` rows are read; a larger tree is cut short.
#[implement(Service)]
pub fn get_relations_recursive<'a>(
	&'a self,
	shortroomid: ShortRoomId,
	target: PduCount,
	from: Option<PduCount>,
	dir: Direction,
	user_id: Option<&'a UserId>,
	max_depth: usize,
) -> impl Stream<Item = (usize, PduCount, Pdu)> + Send + 'a {
	self.relation_tree(target, from, dir, max_depth)
		.map(|tree| stream::iter(tree).map(Ok))
		.flatten_stream()
		.wide_and_then(move |(depth, count)| {
			self.load_relation(shortroomid, count, user_id)
				.map_ok(move |relation| relation.map(|(count, pdu)| (depth, count, pdu)))
		})
		.ready_try_filter_map(Ok)
		.ignore_err()
}

/// Positions and depths of the events relating to `target` up to `max_depth`,
/// ordered in `dir` after `from`.
#[implement(Service)]
async fn relation_tree(
	&self,
	target: PduCount,
	from: Option<PduCount>,
	dir: Direction,
	max_depth: usize,
) -> Vec<(usize, PduCount)> {
	// An event relates only to an event stored before it, so it is newer than
	// all its ancestors. Walking backward, nothing below an event at or past
	// `from` comes before `from`, so `from` bounds every level. Walking forward,
	// an event before `from` can have descendants after it.
	let bound = from.filter(|_| dir == Direction::Backward);

	let mut tree = Vec::new();
	let mut parents = vec![target];
	for depth in 0..=max_depth {
		let level: Vec<PduCount> = stream::iter(&parents)
			.map(|&parent| self.relation_counts(parent, bound, dir))
			.flatten()
			.ignore_err()
			.take(RECURSIVE_ROW_LIMIT.saturating_sub(tree.len()))
			.collect()
			.await;

		tree.extend(level.iter().map(|&count| (depth, count)));
		parents = level;
	}

	if tree.len() >= RECURSIVE_ROW_LIMIT {
		debug_warn!(?target, "Recursive relations walk cut short at {RECURSIVE_ROW_LIMIT} rows");
	}

	tree.retain(|&(_, count)| match (from, dir) {
		| (None, _) => true,
		| (Some(from), Direction::Backward) => count < from,
		| (Some(from), Direction::Forward) => count > from,
	});

	match dir {
		| Direction::Backward => tree.sort_unstable_by_key(|&(_, count)| Reverse(count)),
		| Direction::Forward => tree.sort_unstable_by_key(|&(_, count)| count),
	}

	// An event relating to two events of the tree is reached twice.
	tree.dedup_by_key(|&mut (_, count)| count);
	tree
}

/// Walks the positions of the events relating to `target` in `dir` order after
/// `from`, without loading the events.
#[implement(Service)]
fn relation_counts(
	&self,
	target: PduCount,
	from: Option<PduCount>,
	dir: Direction,
) -> impl Stream<Item = Result<PduCount>> + Send + '_ {
	let target = target.to_be_bytes();
	let from = from
		.map(|from| from.saturating_inc(dir))
		.unwrap_or_else(|| match dir {
			| Direction::Backward => PduCount::max(),
			| Direction::Forward => PduCount::default(),
		})
		.to_be_bytes();

	let mut buf = StartKey::new();
	let start = {
		buf.extend(target);
		buf.extend(from);
		buf.as_slice()
	};

	match dir {
		| Direction::Backward => self
			.db
			.tofrom_relation
			.rev_raw_keys_from(start)
			.left_stream(),

		| Direction::Forward => self
			.db
			.tofrom_relation
			.raw_keys_from(start)
			.right_stream(),
	}
	.ready_try_take_while(move |key| Ok(key.starts_with(&target)))
	.map_ok(|to_from| u64_from_u8(&to_from[8..16]))
	.map_ok(PduCount::from_unsigned)
}

/// Loads the relating event at `count`, without the transaction ID unless
/// `user_id` sent it. A missing event is `None`.
#[implement(Service)]
async fn load_relation(
	&self,
	shortroomid: ShortRoomId,
	count: PduCount,
	user_id: Option<&UserId>,
) -> Result<Option<(PduCount, Pdu)>> {
	let pdu_id: RawPduId = PduId { shortroomid, count }.into();

	self.services
		.timeline
		.get_pdu_from_id(&pdu_id)
		.await
		.optional()?
		.map(|pdu| {
			pdu.without_transaction_id_unless_sender(user_id)
				.map(|pdu| (count, pdu))
		})
		.transpose()
}
