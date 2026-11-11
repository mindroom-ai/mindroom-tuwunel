use futures::StreamExt;
use ruma::{
	OwnedRoomId, OwnedUserId, RoomAliasId, RoomId, RoomOrAliasId, UserId,
	events::{
		StateEventType,
		room::{canonical_alias::RoomCanonicalAliasEventContent, power_levels::UserPowerLevel},
	},
};
use tuwunel_core::{
	Err, Result, err, implement,
	utils::{
		future::{BoolExt, ReadyBoolExt},
		stream::ReadyExt,
	},
};

use super::{Join, Service};

/// The reason such a join carries, the one `auto_join_rooms` gives.
const REASON: &str = "Automatically joining this room upon registration";

/// Join a local user to a room this server is in, on the server's behalf.
///
/// When the room's join rule would refuse the user uninvited, as every join
/// rule refuses an external account, the room's highest-powered active local
/// member allowed to invite invites them first. A user already joined stays
/// joined, so an invite another path accepted meanwhile does no harm.
///
/// Refused, with nothing sent: a room this server is not in, rather than a
/// join over federation; the admin room, whose members are server admins; a
/// room an upgrade replaced, such as an old admin room; and an alias the room
/// it resolves to does not name in its own `m.room.canonical_alias`. Any
/// local user may point a free alias at any room, while only a member with
/// power in the room sets that state, so the invite this sends on the room's
/// behalf goes only where the room's own members put the alias.
#[implement(Service)]
#[tracing::instrument(level = "debug", skip_all, fields(%user_id, %room))]
pub async fn join_with_invite(
	&self,
	user_id: &UserId,
	room: &RoomOrAliasId,
) -> Result<OwnedRoomId> {
	let (room_id, servers) = self
		.services
		.alias
		.maybe_resolve_with_servers(room, None)
		.await?;

	if !self
		.services
		.state_cache
		.server_in_room(self.services.globals.server_name(), &room_id)
		.await
	{
		return Err!(Request(NotFound("This server is not in {room_id}.")));
	}

	if self.services.admin.is_admin_room(&room_id).await {
		return Err!(Request(Forbidden("{room_id} is the admin room, never joined so.")));
	}

	if self
		.services
		.state_accessor
		.room_state_get(&room_id, &StateEventType::RoomTombstone, "")
		.await
		.is_ok()
	{
		return Err!(Request(Forbidden(
			"{room_id} was replaced by an upgrade, never joined so."
		)));
	}

	if let Ok(alias) = <&RoomAliasId>::try_from(room)
		&& !self.names_alias(&room_id, alias).await
	{
		return Err!(Request(Forbidden("{room_id} does not name {alias} as its own alias.")));
	}

	if self.join_needs_invite(&room_id, user_id).await {
		let sender = self.inviter(&room_id, user_id).await?;

		self.invite(&sender, user_id, &room_id, None, false)
			.await?;
	}

	self.join(Join {
		sender_user: user_id,
		room_id: &room_id,
		orig_room_id: Some(room),
		reason: Some(REASON.to_owned()),
		servers: &servers,
		is_appservice: false,
		extra_content: None,
	})
	.await?;

	Ok(room_id)
}

/// Whether the room's `m.room.canonical_alias` names the alias, as its alias
/// or one of its alternatives.
#[implement(Service)]
async fn names_alias(&self, room_id: &RoomId, alias: &RoomAliasId) -> bool {
	self.services
		.state_accessor
		.room_state_get_content(room_id, &StateEventType::RoomCanonicalAlias, "")
		.await
		.is_ok_and(|content: RoomCanonicalAliasEventContent| {
			content
				.aliases()
				.any(|named| named.as_str() == alias.as_str())
		})
}

/// The room's highest-powered active local member other than `user_id` whose
/// power allows inviting, picked as the admin grant picks its sender. Active
/// members have a password or SSO sign-in, so the server's own user and
/// appservice accounts are none; locked, suspended and external accounts may
/// not invite either.
#[implement(Service)]
async fn inviter(&self, room_id: &RoomId, user_id: &UserId) -> Result<OwnedUserId> {
	let power_levels = self
		.services
		.state_accessor
		.get_power_levels(room_id)
		.await?;

	let users = &self.services.users;

	self.services
		.state_cache
		.active_local_users_in_room(room_id)
		.ready_filter(|member| *member != user_id && power_levels.user_can_invite(member))
		.filter(|&member| {
			users
				.is_locked(member)
				.is_false()
				.and2(users.is_suspended(member).is_false(), users.is_external(member).is_false())
		})
		.ready_fold(None, |best: Option<(OwnedUserId, UserPowerLevel)>, member| {
			let level = power_levels.for_user(member);
			match best {
				| Some((_, best_level)) if best_level >= level => best,
				| _ => Some((member.to_owned(), level)),
			}
		})
		.await
		.map(|(member, _)| member)
		.ok_or_else(|| {
			err!(Request(Forbidden("No active local member of {room_id} may invite.")))
		})
}
