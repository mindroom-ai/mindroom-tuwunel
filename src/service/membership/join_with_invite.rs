use ruma::{
	OwnedRoomId, OwnedUserId, RoomId, RoomOrAliasId, UserId,
	events::room::power_levels::UserPowerLevel,
};
use tuwunel_core::{Err, Result, err, implement, utils::stream::ReadyExt};

use super::{Join, Service};

/// The reason such a join carries, the one `auto_join_rooms` gives.
const REASON: &str = "Automatically joining this room upon registration";

/// Join a local user to a room this server is in, on the server's behalf.
///
/// When the room's join rule would refuse the user uninvited, the room's
/// highest-powered local member allowed to invite invites them first. A user
/// already joined stays joined, so an invite another path accepted meanwhile
/// does no harm. A room this server is not in is refused rather than joined
/// over federation.
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

/// The room's highest-powered local member other than `user_id` whose power
/// allows inviting, picked as the admin grant picks its sender.
#[implement(Service)]
async fn inviter(&self, room_id: &RoomId, user_id: &UserId) -> Result<OwnedUserId> {
	let power_levels = self
		.services
		.state_accessor
		.get_power_levels(room_id)
		.await?;

	self.services
		.state_cache
		.local_users_in_room(room_id)
		.ready_filter(|member| *member != user_id && power_levels.user_can_invite(member))
		.ready_fold(None, |best: Option<(OwnedUserId, UserPowerLevel)>, member| {
			let level = power_levels.for_user(member);
			match best {
				| Some((_, best_level)) if best_level >= level => best,
				| _ => Some((member.to_owned(), level)),
			}
		})
		.await
		.map(|(member, _)| member)
		.ok_or_else(|| err!(Request(Forbidden("No local member of {room_id} may invite."))))
}
