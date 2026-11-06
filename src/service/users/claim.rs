//! Existing accounts an identity provider sign-in may claim.
//!
//! An operator can create accounts before their owners first sign in, for
//! example through an appservice that fills them with history from another
//! system. Such an account is marked unclaimed when it is created. With
//! `claim_unlinked_accounts`, the first sign-in whose claims name it takes it,
//! as long as nobody can sign in to it yet.

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
/// this is settled or dropped, so the caller links the identity before anything
/// else can find the account unlinked.
#[must_use]
pub struct Claim {
	user_id: OwnedUserId,
	_password_guard: MutexMapGuard<OwnedUserId, ()>,
}

// The guard's own output lists every account whose password lock is held.
impl fmt::Debug for Claim {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Claim").finish_non_exhaustive()
	}
}

/// Mark an account an appservice or an admin created without a password as
/// unclaimed, so its owner's identity provider sign-in may claim it.
///
/// An account a person registered for themselves is never marked: anyone could
/// otherwise register a name ahead of its owner and keep a way into it.
#[implement(super::Service)]
pub fn mark_unclaimed(&self, user_id: &UserId) { self.db.userid_unclaimed.insert(user_id, ""); }

/// Whether an account carries the unclaimed mark.
///
/// A settled claim, a password and a deactivation remove it.
#[implement(super::Service)]
pub async fn is_unclaimed(&self, user_id: &UserId) -> bool {
	self.db
		.userid_unclaimed
		.get(user_id)
		.await
		.is_ok()
}

/// Turn an account nobody can sign in to into an SSO account, or change
/// nothing and return `None`.
///
/// The account gets the password sentinel and the SSO origin in one write, as
/// SSO registration gives a new account. It keeps its unclaimed mark, which
/// still admits such an account, until `settle_claim` once the identity is
/// linked, so a sign-in cut short in between leaves it claimable. The caller
/// has already refused names that are forbidden or that an appservice
/// reserves exclusively.
#[implement(super::Service)]
pub async fn claim_unlinked(&self, user_id: &UserId) -> Option<Claim> {
	let password_guard = self.changing_password.lock(user_id).await;

	if !self.is_claimable(user_id).await {
		return None;
	}

	let mut txn = self.services.db.txn();

	txn.insert_raw(&self.db.userid_origin, user_id, "sso");
	txn.insert_raw(&self.db.userid_password, user_id, PASSWORD_SENTINEL);
	txn.execute();

	Some(Claim {
		user_id: user_id.to_owned(),
		_password_guard: password_guard,
	})
}

/// Complete a claim once the identity is linked to the account: the account
/// loses its unclaimed mark, and its password lock is released.
#[implement(super::Service)]
pub fn settle_claim(&self, claim: Claim) {
	self.db.userid_unclaimed.remove(&claim.user_id);
	drop(claim);
}

/// Whether nobody can sign in to an existing local account.
///
/// The account must not be the server user and must carry the unclaimed mark.
/// Its origin must be password or SSO (others sign in through LDAP, JWT or a
/// delegated authentication service), it must hold no password, and an empty
/// one must come with no recorded deactivation. It must not be locked, and must
/// have no device, whose token or keys someone holds, no bound third-party
/// identifier, through which its owner could reset a password, no identity
/// linked and no admin-approved association pending. Anything unreadable
/// counts against it.
#[implement(super::Service)]
async fn is_claimable(&self, user_id: &UserId) -> bool {
	if !self.services.globals.user_is_local(user_id)
		|| user_id == self.services.globals.server_user
		|| !self.is_unclaimed(user_id).await
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
		&& !self.is_locked(user_id).await
		&& !self.has_devices(user_id).await
		&& !self.services.threepid.has_bindings(user_id).await
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
