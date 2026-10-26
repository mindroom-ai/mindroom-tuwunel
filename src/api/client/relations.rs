use std::{collections::HashSet, mem::take};

use axum::extract::State;
use futures::{
	Stream, StreamExt,
	future::Either::{Left, Right},
	pin_mut,
};
use ruma::{
	EventId, RoomId, UInt, UserId,
	api::{
		Direction,
		client::relations::{
			get_relating_events::{self, v1::Response},
			get_relating_events_with_rel_type, get_relating_events_with_rel_type_and_event_type,
		},
	},
	events::{TimelineEventType, relation::RelationType},
};
use tuwunel_core::{
	Err, Result, at, debug_warn, err,
	utils::{
		math::usize_from_ruma_bounded,
		result::FlatOk,
		stream::{IterStream, ReadyExt, WidebandExt},
	},
	warn,
};
use tuwunel_matrix::{
	event::{Event, RelationTypeEqual},
	pdu::{Pdu, PduCount, PduId},
};
use tuwunel_service::{Services, rooms::pdu_metadata::RequesterQuery};

use crate::{Ruma, is_ignored_pdu};

/// Limits on one recursive `/relations` walk.
#[derive(Clone, Copy)]
struct WalkLimits {
	events: usize,
	rows: usize,
}

const WALK_LIMITS: WalkLimits = WalkLimits { events: 32_768, rows: 262_144 };

/// Progress carried across levels of a recursive `/relations` walk.
struct WalkState<T> {
	frontier: Vec<PduCount>,
	events: Vec<(PduCount, T)>,
	seen: HashSet<PduCount>,
	rows: usize,
}

/// How much of the room's history a requester may read through its relations.
///
/// The parent and every relation returned are judged under the same access, and
/// world-readable access serves relations without bundled aggregations.
#[derive(Clone, Copy)]
enum Access {
	/// The room admits or invites them, so each event is judged on its own.
	Room,

	/// They never joined, so only events sent while history was world-readable.
	WorldReadable,
}

// Matrix v1.10 recommends traversing at least 3 levels of relationships.
const RECURSION_DEPTH: u32 = 3;

/// # `GET /_matrix/client/r0/rooms/{roomId}/relations/{eventId}/{relType}/{eventType}`
pub(crate) async fn get_relating_events_with_rel_type_and_event_type_route(
	State(services): State<crate::State>,
	body: Ruma<get_relating_events_with_rel_type_and_event_type::v1::Request>,
) -> Result<get_relating_events_with_rel_type_and_event_type::v1::Response> {
	paginate_relations_with_filter(
		&services,
		body.sender_user(),
		&body.room_id,
		&body.event_id,
		body.event_type.clone().into(),
		body.rel_type.clone().into(),
		body.from.as_deref(),
		body.to.as_deref(),
		body.limit,
		body.recurse,
		body.dir,
	)
	.await
	.map(|res| get_relating_events_with_rel_type_and_event_type::v1::Response {
		chunk: res.chunk,
		next_batch: res.next_batch,
		prev_batch: res.prev_batch,
		recursion_depth: res.recursion_depth,
	})
}

/// # `GET /_matrix/client/r0/rooms/{roomId}/relations/{eventId}/{relType}`
pub(crate) async fn get_relating_events_with_rel_type_route(
	State(services): State<crate::State>,
	body: Ruma<get_relating_events_with_rel_type::v1::Request>,
) -> Result<get_relating_events_with_rel_type::v1::Response> {
	paginate_relations_with_filter(
		&services,
		body.sender_user(),
		&body.room_id,
		&body.event_id,
		None,
		body.rel_type.clone().into(),
		body.from.as_deref(),
		body.to.as_deref(),
		body.limit,
		body.recurse,
		body.dir,
	)
	.await
	.map(|res| get_relating_events_with_rel_type::v1::Response {
		chunk: res.chunk,
		next_batch: res.next_batch,
		prev_batch: res.prev_batch,
		recursion_depth: res.recursion_depth,
	})
}

/// # `GET /_matrix/client/r0/rooms/{roomId}/relations/{eventId}`
pub(crate) async fn get_relating_events_route(
	State(services): State<crate::State>,
	body: Ruma<get_relating_events::v1::Request>,
) -> Result<Response> {
	paginate_relations_with_filter(
		&services,
		body.sender_user(),
		&body.room_id,
		&body.event_id,
		None,
		None,
		body.from.as_deref(),
		body.to.as_deref(),
		body.limit,
		body.recurse,
		body.dir,
	)
	.await
}

#[expect(clippy::too_many_arguments)]
#[tracing::instrument(
	name = "relations",
	level = "debug",
	skip_all,
	fields(room_id, target, from, to, dir, limit, recurse)
)]
async fn paginate_relations_with_filter(
	services: &Services,
	sender_user: &UserId,
	room_id: &RoomId,
	target: &EventId,
	filter_event_type: Option<TimelineEventType>,
	filter_rel_type: Option<RelationType>,
	from: Option<&str>,
	to: Option<&str>,
	limit: Option<UInt>,
	recurse: bool,
	dir: Direction,
) -> Result<Response> {
	let from: Option<PduCount> = from.map(str::parse).transpose()?;

	let to: Option<PduCount> = to.map(str::parse).flat_ok();

	let limit = limit.map_or(30, |limit| usize_from_ruma_bounded(limit, 30, 100));

	let (target, access) = {
		let (target, parent, access) = visible_parent(services, sender_user, room_id, target)
			.await
			.ok_or_else(|| err!(Request(NotFound("Event not found."))))?;

		if is_ignored_pdu(services, &parent, sender_user).await {
			return Err!(HttpJson(NOT_FOUND, {
				"errcode": "M_SENDER_IGNORED",
				"error": "You have ignored the user that sent this event",
				"sender": parent.sender().as_str(),
			}));
		}

		(target, access)
	};

	let PduCount::Normal(_) = target.count else {
		return Ok(Response {
			recursion_depth: recurse.then(|| UInt::from(RECURSION_DEPTH)),
			..Response::new(Vec::new())
		});
	};

	let ignored = services.users.ignored_users(sender_user).await;

	let filter = |(count, pdu): &(PduCount, Pdu)| {
		matches!(count, PduCount::Normal(_))
			&& filter_event_type
				.as_ref()
				.is_none_or(|kind| kind == pdu.kind())
			&& filter_rel_type
				.as_ref()
				.is_none_or(|rel_type| rel_type.relation_type_equal(pdu))
	};

	let query = RequesterQuery {
		shortroomid: target.shortroomid,
		target: target.count,
		from,
		to,
		dir,
		user_id: sender_user,
		ignored: ignored.as_ref(),
	};

	let walked = match recurse {
		| true => Some(
			walk_relations(
				query,
				WALK_LIMITS,
				move |target, row_room| {
					services.pdu_metadata.requester_relation_rows(
						RequesterQuery { target, from: None, to: None, ..query },
						row_room,
					)
				},
				&filter,
			)
			.await,
		),
		| false => None,
	};

	let (recursion_depth, relations) = match walked {
		| Some(Some((depth, events))) =>
			(Some(UInt::from(depth)), Left(events.into_iter().stream())),
		| fallback => (
			fallback.map(|_| UInt::from(0_u32)),
			Right(
				services
					.pdu_metadata
					.requester_relations(query)
					.ready_filter(&filter),
			),
		),
	};

	let mut events = relations
		.wide_filter_map(async |(count, pdu)| {
			visible_to(services, sender_user, access, &pdu)
				.await
				.then_some((count, pdu))
		})
		.take(limit.saturating_add(1))
		.collect::<Vec<_>>()
		.await;

	let more = events.len() > limit;
	events.truncate(limit);

	Ok(Response {
		recursion_depth,

		next_batch: events
			.last()
			.filter(|_| more)
			.map(at!(0))
			.as_ref()
			.map(ToString::to_string),

		prev_batch: events
			.first()
			.map(at!(0))
			.or(from)
			.as_ref()
			.map(ToString::to_string),

		chunk: events
			.into_iter()
			.map(at!(1))
			.stream()
			.wide_filter_map(|pdu| served(services, sender_user, access, pdu))
			.map(Event::into_format)
			.collect()
			.await,
	})
}

/// Finds the parent event, its timeline position and the requester's access
/// when it is in the room and the requester may read it.
///
/// An unknown parent, one only stored as an outlier, one in another room, one
/// whose rows fail to read and a denied one all come back `None`, so the caller
/// refuses each with the one 404 the spec gives for "not found or no
/// permission".
async fn visible_parent(
	services: &Services,
	sender_user: &UserId,
	room_id: &RoomId,
	target: &EventId,
) -> Option<(PduId, Pdu, Access)> {
	let pdu_id = services.timeline.get_pdu_id(target).await.ok()?;

	let parent = services
		.timeline
		.get_pdu_from_id(&pdu_id)
		.await
		.ok()?;

	if parent.room_id() != room_id {
		return None;
	}

	let access = access(services, sender_user, room_id).await?;

	visible_to(services, sender_user, access, &parent)
		.await
		.then_some((pdu_id.into(), parent, access))
}

/// Decides how much of the room's history the requester may read, if any.
///
/// The room admits members, departed members and everyone while its history is
/// world-readable, and a pending invitee reads as well. If the room does not
/// admit them, one who never joined reads only world-readable history, and one
/// who once joined, as after a forget, reads none. A failed read narrows access.
async fn access(services: &Services, sender_user: &UserId, room_id: &RoomId) -> Option<Access> {
	let admitted = services
		.state_accessor
		.user_can_see_state_events(sender_user, room_id)
		.await
		|| services
			.state_cache
			.is_invited(sender_user, room_id)
			.await;

	if admitted {
		return Some(Access::Room);
	}

	matches!(
		services
			.state_cache
			.try_once_joined(sender_user, room_id)
			.await,
		Ok(false)
	)
	.then_some(Access::WorldReadable)
}

/// Decides whether the requester may see an event within their access.
///
/// World-readable access also needs the event sent while history was
/// world-readable, before its own visibility is judged.
async fn visible_to(
	services: &Services,
	sender_user: &UserId,
	access: Access,
	pdu: &Pdu,
) -> bool {
	let visible = services
		.state_accessor
		.user_can_see_event(sender_user, pdu);

	match access {
		| Access::Room => visible.await,
		| Access::WorldReadable =>
			services
				.state_accessor
				.is_world_readable_at(pdu)
				.await
				&& visible.await,
	}
}

/// Readies a relation for the requester within their access, or `None` to
/// withhold it.
///
/// World-readable access folds no aggregations, since each bundled child would
/// need its own visibility check against that access. If the erasure prune a
/// relation requires fails, the relation is withheld.
async fn served(
	services: &Services,
	sender_user: &UserId,
	access: Access,
	pdu: Pdu,
) -> Option<Pdu> {
	let metadata = &services.pdu_metadata;

	match access {
		| Access::Room => metadata
			.bundle_aggregations(sender_user, pdu)
			.await
			.into(),

		| Access::WorldReadable => metadata
			.unbundled(sender_user, pdu)
			.await
			.inspect_err(|error| warn!(%error, "Erasure prune failed; relation withheld."))
			.ok(),
	}
}

/// Walks matching relations by level within `limits` and returns the depth
/// reached and the requested window in count order, or `None` when depth 1
/// overflows so the caller can fall back to direct relations lazily.
async fn walk_relations<T, F, S, P>(
	query: RequesterQuery<'_>,
	limits: WalkLimits,
	fetch: F,
	filter: &P,
) -> Option<(u32, Vec<(PduCount, T)>)>
where
	T: Send,
	F: Fn(PduCount, usize) -> S + Send + Sync,
	S: Stream<Item = Option<(PduCount, T)>> + Send,
	P: Fn(&(PduCount, T)) -> bool + Sync,
{
	let mut state = WalkState {
		frontier: vec![query.target],
		events: Vec::new(),
		seen: HashSet::from([query.target]),
		rows: 0,
	};
	let mut reached = RECURSION_DEPTH;

	for depth in 1..=RECURSION_DEPTH {
		if state.frontier.is_empty() {
			break;
		}
		if !walk_level(query, limits, &fetch, filter, &mut state, depth).await {
			debug_warn!(
				target = ?query.target,
				depth,
				"Recursive relations walk dropped a level over its limits",
			);
			reached = depth.checked_sub(1).filter(|&d| d > 0)?;
			break;
		}
	}

	state
		.events
		.sort_unstable_by(|(a, _), (b, _)| match query.dir {
			| Direction::Forward => a.cmp(b),
			| Direction::Backward => b.cmp(a),
		});

	Some((reached, state.events))
}

async fn walk_level<T, F, S, P>(
	query: RequesterQuery<'_>,
	limits: WalkLimits,
	fetch: &F,
	filter: &P,
	state: &mut WalkState<T>,
	depth: u32,
) -> bool
where
	T: Send,
	F: Fn(PduCount, usize) -> S + Sync,
	S: Stream<Item = Option<(PduCount, T)>> + Send,
	P: Fn(&(PduCount, T)) -> bool + Sync,
{
	let frontier = take(&mut state.frontier);
	let row_room = limits
		.rows
		.saturating_sub(state.rows)
		.saturating_add(1);
	let stream = frontier
		.into_iter()
		.stream()
		.flat_map(|target| fetch(target, row_room))
		.take(row_room);

	pin_mut!(stream);

	let mut rows = state.rows;
	let (mut next_frontier, mut level_events) = (Vec::new(), Vec::new());
	while let Some(item) = stream.next().await {
		rows = rows.saturating_add(1);
		if rows > limits.rows {
			return false;
		}
		let Some((count, event)) = item.filter(filter) else {
			continue;
		};
		if !state.seen.insert(count) {
			continue;
		}
		if state.seen.len().saturating_sub(1) > limits.events {
			return false;
		}
		if depth < RECURSION_DEPTH {
			next_frontier.push(count);
		}
		if query
			.from
			.is_none_or(|from| from.is_before(count, query.dir))
			&& query
				.to
				.is_none_or(|to| count.is_before(to, query.dir))
		{
			level_events.push((count, event));
		}
	}

	state.frontier = next_frontier;
	state.events.extend(level_events);
	state.rows = rows;
	true
}

#[cfg(test)]
mod tests {
	use std::collections::HashMap;

	use ruma::user_id;

	use super::*;

	type Graph = HashMap<u64, Vec<Option<u64>>>;

	fn sample_graph() -> Graph {
		// Depth 1 (under 1): 2, 3, None (unservable), 4 -> 4 rows, 3 events
		// Depth 2 (under 2, 3, 4): 5, 6, None, 7 -> 4 rows, 3 events
		// Depth 3 (under 5): 8 -> 1 row, 1 event
		HashMap::from([
			(1, vec![Some(2), Some(3), None, Some(4)]),
			(2, vec![Some(5), None]),
			(3, vec![Some(6)]),
			(4, vec![Some(7)]),
			(5, vec![Some(8)]),
		])
	}

	fn test_query(from: Option<PduCount>, dir: Direction) -> RequesterQuery<'static> {
		RequesterQuery {
			shortroomid: 1,
			target: PduCount::Normal(1),
			from,
			to: None,
			dir,
			user_id: user_id!("@alice:localhost"),
			ignored: None,
		}
	}

	async fn run(
		graph: &Graph,
		query: RequesterQuery<'_>,
		limits: WalkLimits,
	) -> Option<(u32, Vec<u64>)> {
		let fetch = |target: PduCount, row_room: usize| {
			let rows: Vec<_> = graph
				.get(&target.into_unsigned())
				.into_iter()
				.flatten()
				.take(row_room)
				.map(|opt| opt.map(|n| (PduCount::Normal(n), ())))
				.collect();
			rows.into_iter().stream()
		};

		walk_relations(query, limits, fetch, &|_| true)
			.await
			.map(|(depth, events)| {
				(
					depth,
					events
						.into_iter()
						.map(|(c, ())| c.into_unsigned())
						.collect(),
				)
			})
	}

	#[tokio::test]
	async fn walk_relations_drops_levels_over_event_or_row_limits() {
		let graph = sample_graph();
		let full = test_query(None, Direction::Forward);

		assert_eq!(
			run(&graph, full, WalkLimits { events: 7, rows: 9 }).await,
			Some((3, vec![2, 3, 4, 5, 6, 7, 8])),
			"exact budget keeps all three levels",
		);
		assert_eq!(
			run(&graph, full, WalkLimits { events: 6, rows: usize::MAX }).await,
			Some((2, vec![2, 3, 4, 5, 6, 7])),
			"event cap drops depth 3 whole",
		);
		assert_eq!(
			run(&graph, full, WalkLimits { events: 5, rows: usize::MAX }).await,
			Some((1, vec![2, 3, 4])),
			"event cap drops depth 2 whole",
		);
		assert_eq!(
			run(&graph, full, WalkLimits { events: 2, rows: usize::MAX }).await,
			None,
			"depth 1 event overflow triggers lazy depth-0 fallback",
		);
		assert_eq!(
			run(&graph, full, WalkLimits { events: usize::MAX, rows: 8 }).await,
			Some((2, vec![2, 3, 4, 5, 6, 7])),
			"row cap counts unservable rows and drops depth 3",
		);
		assert_eq!(
			run(&graph, full, WalkLimits { events: usize::MAX, rows: 7 }).await,
			Some((1, vec![2, 3, 4])),
			"row cap drops depth 2 whole",
		);
		assert_eq!(
			run(&graph, full, WalkLimits { events: usize::MAX, rows: 3 }).await,
			None,
			"depth 1 row overflow triggers lazy depth-0 fallback",
		);
	}

	#[tokio::test]
	async fn walk_relations_dedups_event_reached_under_two_parents() {
		let mut graph = sample_graph();
		// Event 9 relates to both 3 and 4, and has child 10.
		graph.entry(3).or_default().push(Some(9));
		graph.entry(4).or_default().push(Some(9));
		graph.insert(9, vec![Some(10)]);

		let full = test_query(None, Direction::Forward);
		// 9 distinct events (2..=10), 12 raw rows (incl. duplicate row for 9).
		assert_eq!(
			run(&graph, full, WalkLimits { events: 9, rows: usize::MAX }).await,
			Some((3, vec![2, 3, 4, 5, 6, 7, 8, 9, 10])),
			"event reached twice is served once, counted once, and walked once",
		);
	}

	#[tokio::test]
	async fn walk_relations_level_cut_does_not_depend_on_window() {
		let graph = sample_graph();
		let paged = test_query(Some(PduCount::Normal(3)), Direction::Forward);

		// Depth 2 overflows events=5 regardless of `from`, so only depth 1's 4 returns.
		assert_eq!(
			run(&graph, paged, WalkLimits { events: 5, rows: usize::MAX }).await,
			Some((1, vec![4])),
			"level cut must stay consistent across pages",
		);
	}
}
