//! Registration invites: email addresses a server admin lets register
//! through single sign-on, each for one account. Keys are addresses in their
//! canonical form, which callers pass.

use futures::{Stream, StreamExt};
use ruma::OwnedRoomOrAliasId;
use serde::{Deserialize, Serialize};
use tuwunel_core::{
	Result, implement,
	utils::{result::NotFound, stream::TryIgnore},
};
use tuwunel_database::{Deserialized, Json};

/// What an invite gives the account its address creates.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RegistrationInvite {
	/// Rooms the account joins, as it joins a registration rule's rooms.
	#[serde(default)]
	pub rooms: Vec<OwnedRoomOrAliasId>,

	/// Whether the account is external.
	#[serde(default)]
	pub external: bool,
}

/// The invite of an address. Only a missing row reads as no invite: a failed
/// read is an error, so a storage fault never registers an invited external
/// account as a normal one.
#[implement(super::Service)]
pub async fn registration_invite(&self, email: &str) -> Result<Option<RegistrationInvite>> {
	self.db
		.registrationemail_invite
		.get(email)
		.await
		.deserialized()
		.present()
}

/// Store the invite of an address, replacing the one it had.
#[implement(super::Service)]
pub fn set_registration_invite(&self, email: &str, invite: &RegistrationInvite) {
	self.db
		.registrationemail_invite
		.raw_put(email, Json(invite));
}

/// Remove the invite of an address, returning whether it had one.
#[implement(super::Service)]
pub async fn remove_registration_invite(&self, email: &str) -> bool {
	let invites = &self.db.registrationemail_invite;
	let existed = invites.exists(email).await.is_ok();

	invites.remove(email);

	existed
}

/// Every invite with its address, by address.
#[implement(super::Service)]
pub fn registration_invites(
	&self,
) -> impl Stream<Item = (String, RegistrationInvite)> + Send + '_ {
	self.db
		.registrationemail_invite
		.stream()
		.ignore_err()
		.map(|(email, invite): (&str, RegistrationInvite)| (email.to_owned(), invite))
}
