//! Registration invites over HTTP: a server administrator lists, writes and
//! removes the email addresses that may register through single sign-on.
//!
//! The routes are MindRoom's own, so their request types are defined here.

use std::collections::HashSet;

use axum::extract::State;
use futures::{StreamExt, future::join};
use ruma::{OwnedRoomOrAliasId, RoomAliasId};
use serde::{Deserialize, Serialize};
use tuwunel_core::{Err, Result};
use tuwunel_service::{threepid::canonicalize_email, users::RegistrationInvite};

use crate::RumaAdmin;

/// An invite as the routes answer with it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Invite {
	/// The address, in its canonical form.
	email: String,

	/// Rooms the account joins.
	rooms: Vec<OwnedRoomOrAliasId>,

	/// Whether the account is external.
	external: bool,
}

impl Invite {
	fn new(email: String, invite: RegistrationInvite) -> Self {
		let RegistrationInvite { rooms, external } = invite;

		Self { email, rooms, external }
	}
}

pub(crate) mod list {
	use ruma::{
		OwnedRoomId,
		api::{auth_scheme::AccessToken, metadata, request, response},
	};

	use super::Invite;

	metadata! {
		method: GET,
		rate_limited: false,
		authentication: AccessToken,
		path: "/_matrix/client/unstable/io.mindroom/admin/registration_invites",
	}

	#[request]
	pub(crate) struct Request;

	#[response]
	pub(crate) struct Response {
		/// Every invite, by address.
		pub(crate) invites: Vec<Invite>,

		/// The rooms of `external_forbidden_rooms`, as room IDs, which an
		/// external invite may not name.
		pub(crate) external_forbidden_rooms: Vec<OwnedRoomId>,
	}
}

pub(crate) mod put {
	use ruma::{
		OwnedRoomOrAliasId,
		api::{auth_scheme::AccessToken, metadata, request, response},
	};

	use super::Invite;

	metadata! {
		method: PUT,
		rate_limited: false,
		authentication: AccessToken,
		path: "/_matrix/client/unstable/io.mindroom/admin/registration_invites/{email}",
	}

	#[request]
	pub(crate) struct Request {
		/// The address to invite, in any case.
		#[ruma_api(path)]
		pub(crate) email: String,

		/// Rooms the account joins.
		#[serde(default)]
		pub(crate) rooms: Vec<OwnedRoomOrAliasId>,

		/// Whether the account is external.
		#[serde(default)]
		pub(crate) external: bool,
	}

	#[response]
	pub(crate) struct Response {
		/// The invite as it is stored.
		#[ruma_api(body)]
		pub(crate) invite: Invite,
	}
}

pub(crate) mod delete {
	use ruma::api::{auth_scheme::AccessToken, metadata, request, response};

	metadata! {
		method: DELETE,
		rate_limited: false,
		authentication: AccessToken,
		path: "/_matrix/client/unstable/io.mindroom/admin/registration_invites/{email}",
	}

	#[request]
	pub(crate) struct Request {
		/// The address whose invite to remove, in any case.
		#[ruma_api(path)]
		pub(crate) email: String,
	}

	#[response]
	pub(crate) struct Response;
}

/// Lists the invites through
/// `GET /_matrix/client/unstable/io.mindroom/admin/registration_invites`.
pub(crate) async fn admin_list_registration_invites_route(
	State(services): State<crate::State>,
	_body: RumaAdmin<list::Request>,
) -> Result<list::Response> {
	let invites = services
		.users
		.registration_invites()
		.map(|(email, invite)| Invite::new(email, invite))
		.collect();

	let external_forbidden_rooms = services
		.users
		.external_forbidden_rooms()
		.collect();

	let (invites, mut external_forbidden_rooms): (_, Vec<_>) =
		join(invites, external_forbidden_rooms).await;

	// A room listed by alias and by ID, or by two aliases, is named once.
	let mut named = HashSet::new();
	external_forbidden_rooms.retain(|room| named.insert(room.clone()));

	Ok(list::Response { invites, external_forbidden_rooms })
}

/// Writes the invite of an address through
/// `PUT /_matrix/client/unstable/io.mindroom/admin/registration_invites/{email}`,
/// replacing the one it had.
///
/// An address an account already has is refused: only a sign-in that creates
/// an account uses an invite, so that one would stay in the list for good.
/// As for a registration rule, an alias must be on this server, and no room
/// is checked to exist: one that cannot be joined is logged at the sign-in.
/// An external invite may not name a room of `external_forbidden_rooms`, by
/// its ID or by an alias that resolves to it.
pub(crate) async fn admin_put_registration_invite_route(
	State(services): State<crate::State>,
	body: RumaAdmin<put::Request>,
) -> Result<put::Response> {
	let put::Request { email, rooms, external } = body.body;
	let email = canonicalize_email(&email)?;

	if let Some(alias) = rooms
		.iter()
		.filter_map(|room| <&RoomAliasId>::try_from(&**room).ok())
		.find(|alias| !services.globals.alias_is_local(alias))
	{
		return Err!(Request(InvalidParam("{alias} is not a local room alias.")));
	}

	for room in rooms.iter().filter(|_| external) {
		if services.users.external_forbidden_room(room).await {
			return Err!(Request(Forbidden("External accounts may not be in {room}.")));
		}
	}

	if let Some(user_id) = services
		.threepid
		.user_id_for_email(&email)
		.await?
	{
		return Err!(Request(ThreepidInUse("That email address already belongs to {user_id}.")));
	}

	let invite = RegistrationInvite { rooms, external };

	services
		.users
		.set_registration_invite(&email, &invite);

	Ok(put::Response { invite: Invite::new(email, invite) })
}

/// Removes the invite of an address through
/// `DELETE /_matrix/client/unstable/io.mindroom/admin/registration_invites/{email}`.
/// The address is read as `PUT` reads it, so a malformed one is refused.
pub(crate) async fn admin_delete_registration_invite_route(
	State(services): State<crate::State>,
	body: RumaAdmin<delete::Request>,
) -> Result<delete::Response> {
	let email = canonicalize_email(&body.body.email)?;

	if !services
		.users
		.remove_registration_invite(&email)
		.await
	{
		return Err!(Request(NotFound("That email address has no registration invite.")));
	}

	Ok(delete::Response)
}
