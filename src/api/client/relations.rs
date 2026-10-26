use axum::extract::State;
use futures::{FutureExt, StreamExt, TryFutureExt, future::try_join3};
use ruma::{
	EventId, RoomId, UInt, UserId,
	api::{
		Direction,
		client::relations::{
			get_relating_events, get_relating_events_with_rel_type,
			get_relating_events_with_rel_type_and_event_type,
		},
	},
	events::{TimelineEventType, relation::RelationType},
};
use tuwunel_core::{
	Err, Error, Result, at, err,
	matrix::{
		event::{Event, RelationTypeEqual},
		pdu::{PduCount, PduId},
	},
	utils::{
		BoolExt,
		math::usize_from_ruma_bounded,
		result::FlatOk,
		stream::{ReadyExt, WidebandExt},
	},
};
use tuwunel_service::Services;

use crate::{Ruma, client::is_ignored_pdu};

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
) -> Result<get_relating_events::v1::Response> {
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
) -> Result<get_relating_events::v1::Response> {
	let from: Option<PduCount> = from.map(str::parse).transpose()?;

	let to: Option<PduCount> = to.map(str::parse).flat_ok();

	// Spec (v1.10) recommends depth of at least 3
	let max_depth: usize = if recurse { 3 } else { 0 };

	let limit = limit.map_or(30, |limit| usize_from_ruma_bounded(limit, 30, 100));

	let target_event_id: &EventId = target;

	let target = services
		.timeline
		.get_pdu_id(target)
		.map_ok(PduId::from)
		.map_ok(Ok::<_, Error>);

	let visible = services
		.state_accessor
		.user_can_see_state_events(sender_user, room_id)
		.map(|visible| {
			visible
				.into_option()
				.ok_or_else(|| err!(Request(Forbidden("You cannot view this room."))))
		});

	let shortroomid = services.short.get_shortroomid(room_id);

	let (shortroomid, target, ()) = try_join3(shortroomid, target, visible).await?;

	let Ok(target) = target else {
		return Ok(get_relating_events::v1::Response::new(Vec::new()));
	};

	if shortroomid != target.shortroomid {
		return Err!(Request(NotFound("Event not found in room.")));
	}

	if let PduCount::Backfilled(_) = target.count {
		return Ok(get_relating_events::v1::Response::new(Vec::new()));
	}

	if let Ok(target_pdu) = services.timeline.get_pdu(target_event_id).await
		&& is_ignored_pdu(services, &target_pdu, sender_user).await
	{
		return Err!(HttpJson(NOT_FOUND, {
			"errcode": "M_SENDER_IGNORED",
			"error": "You have ignored the user that sent this event",
			"sender": target_pdu.sender().as_str(),
		}));
	}

	let (recursion_depth, relations) = match recurse {
		| false => {
			let relations = services.pdu_metadata.get_relations(
				shortroomid,
				target.count,
				from,
				dir,
				Some(sender_user),
			);

			(None, relations.left_stream())
		},
		| true => {
			let (depth, relations) = services
				.pdu_metadata
				.get_relations_recursive(
					shortroomid,
					target.count,
					from,
					dir,
					Some(sender_user),
					max_depth,
				)
				.await;

			(Some(depth), relations.right_stream())
		},
	};

	let mut events = relations
		.ready_filter(|(count, _)| matches!(count, PduCount::Normal(_)))
		.ready_take_while(|&(count, _)| match to {
			// The recursive walk is in position order, so it ends at the first
			// event past `to`, whether or not `to` is one of its events.
			| Some(to) if recurse => match dir {
				| Direction::Backward => count > to,
				| Direction::Forward => count < to,
			},
			| to => Some(count) != to,
		})
		.ready_filter(|(_, pdu)| {
			filter_event_type
				.as_ref()
				.is_none_or(|kind| kind == pdu.kind())
		})
		.ready_filter(|(_, pdu)| {
			filter_rel_type
				.as_ref()
				.is_none_or(|rel_type| rel_type.relation_type_equal(pdu))
		})
		.wide_filter_map(async |(count, pdu)| {
			services
				.state_accessor
				.user_can_see_event(sender_user, &pdu)
				.await
				.then_some((count, pdu))
		})
		// One event past the page shows whether another page follows.
		.take(limit.saturating_add(1))
		.wide_then(async |(count, pdu)| {
			let pdu = services
				.pdu_metadata
				.bundle_aggregations(sender_user, pdu)
				.await;

			(count, pdu)
		})
		.collect::<Vec<_>>()
		.await;

	let more = events.len() > limit;
	events.truncate(limit);

	Ok(get_relating_events::v1::Response {
		// The depth the walk reached, the same on every page of it.
		recursion_depth: recursion_depth
			.map(TryInto::try_into)
			.transpose()?,

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
			.map(Event::into_format)
			.collect(),
	})
}
