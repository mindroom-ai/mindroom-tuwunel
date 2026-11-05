//! Existing accounts an identity provider sign-in may claim.
//!
//! An operator can create accounts before their owners first sign in, for
//! example through an appservice that fills them with history from another
//! system. With `claim_unlinked_accounts`, the first sign-in whose claims name
//! such an account takes it, as long as nobody can sign in to it yet.

use std::fmt;

use ruma::{OwnedUserId, UserId};
use tuwunel_core::{
	implement,
	utils::{MutexMapGuard, ReadyExt, result::NotFound},
};
use tuwunel_database::Interfix;

use super::{PASSWORD_DISABLED, PASSWORD_SENTINEL};

/// Holds the password lock of an account a sign-in has claimed.
///
/// Password changes, deactivation and other claims of the account wait until
/// this is dropped, so the caller links the identity before anything else can
/// find the account unlinked.
#[must_use]
pub struct Claim {
	_password_guard: MutexMapGuard<OwnedUserId, ()>,
}

// The guard's own output lists every account whose password lock is held.
impl fmt::Debug for Claim {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Claim").finish_non_exhaustive()
	}
}

/// Turn an account nobody can sign in to into an SSO account, or change
/// nothing and return `None`.
///
/// The account gets the password sentinel and the SSO origin in one write, as
/// SSO registration gives a new account. The caller has already refused names
/// that are forbidden or that an appservice reserves exclusively, and links the
/// identity while it holds the returned claim.
#[implement(super::Service)]
pub async fn claim_unlinked(&self, user_id: &UserId) -> Option<Claim> {
	let password_guard = self.changing_password.lock(user_id).await;

	if !self.is_unclaimed(user_id).await {
		return None;
	}

	let mut txn = self.services.db.txn();

	txn.insert_raw(&self.db.userid_origin, user_id, "sso");
	txn.insert_raw(&self.db.userid_password, user_id, PASSWORD_SENTINEL);
	txn.execute();

	Some(Claim { _password_guard: password_guard })
}

/// Whether nobody can sign in to an existing local account.
///
/// The account must not be the server user, must have the password or SSO
/// origin (others sign in through LDAP, JWT or a delegated authentication
/// service) and must hold no password. An empty password means the account
/// was created without one unless a deactivation is recorded. It must have no
/// device, whose token or keys someone holds, no identity linked and no
/// admin-approved association pending. Anything unreadable counts against it.
#[implement(super::Service)]
async fn is_unclaimed(&self, user_id: &UserId) -> bool {
	if !self.services.globals.user_is_local(user_id)
		|| user_id == self.services.globals.server_user
	{
		return false;
	}

	let origin = self.origin(user_id).await;
	let origin = match origin.as_deref() {
		| Ok(origin) => matches!(origin, "password" | "sso"),
		| Err(_) => origin.is_missing(),
	};

	let passwordless = match self.password_hash(user_id).await.as_deref() {
		| Ok(PASSWORD_SENTINEL) => true,
		| Ok(PASSWORD_DISABLED) => self
			.db
			.userid_deactivation_reason
			.get(user_id)
			.await
			.is_missing(),
		| _ => false,
	};

	origin
		&& passwordless
		&& !self.has_devices(user_id).await
		&& !self
			.services
			.oauth
			.sessions
			.is_linked(user_id)
			.await
		&& !self
			.services
			.oauth
			.sessions
			.is_user_association_pending(user_id)
}

/// Whether the account has a device, counting one that cannot be read.
#[implement(super::Service)]
async fn has_devices(&self, user_id: &UserId) -> bool {
	self.db
		.userdeviceid_metadata
		.keys_prefix_raw(&(user_id, Interfix))
		.ready_any(|_| true)
		.await
}
