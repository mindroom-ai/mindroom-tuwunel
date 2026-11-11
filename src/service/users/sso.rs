use futures::StreamExt;
use ruma::{OwnedUserId, RoomId, UserId};
use tuwunel_core::{
	Err, Error, Result, err, utils,
	utils::{BoolExt, MutexMapGuard, result::NotFound},
	warn,
};
use tuwunel_database::{Deserialized, Txn};

use super::{PASSWORD_SENTINEL, Service};

/// An account a first sign-in links: its password lock, held until the link
/// is stored, and whether the account has an empty password to convert.
pub type SsoAssignment = (MutexMapGuard<OwnedUserId, ()>, bool);

/// Why deactivations carry a reason. Fork-only: neither Synapse nor upstream
/// Tuwunel has this concept.
///
/// Synapse models deactivation as a bare `users.deactivated` flag (plus the
/// separate, admin-imposed MSC3823 `suspended` flag) and stores no reason
/// anywhere. It hard-blocks a deactivated account from completing SSO login
/// (`auth.py` renders the `sso_account_deactivated_template` 403 page), and
/// its admin-only `activate_account` expects a password hash to be set
/// afterwards — which passwordless SSO accounts don't have. Upstream Tuwunel
/// likewise stores no reason; since v1.8.1 an admin can reactivate a
/// passwordless user via `PASSWORD_SENTINEL`, but there is no self-service
/// path in either server.
///
/// This fork adds self-service reactivation: a *self*-deactivated SSO-origin
/// account is restored when the same IdP identity returns
/// (`maybe_reactivate_deactivated_sso`). Persisting who initiated the
/// deactivation is what makes that safe — admin deactivations must stay
/// final, or a banned user could resurrect their account by simply logging
/// in again. The reason is therefore a *required* parameter of
/// `deactivate_account`: every new call site (typically new upstream
/// Synapse-admin endpoints appearing at each rebase) fails to compile until
/// it explicitly classifies itself as self-service or administrative,
/// instead of inheriting a default.
///
/// Deliberately not upstreamed: reversible deactivation contradicts the
/// semantics Synapse established (its closest concept, MSC3823 suspension,
/// is admin-imposed and different), so upstreaming would effectively require
/// an MSC first. Recorded here so future rebases don't have to re-derive
/// this from the Synapse sources; see also the design note in
/// `FORK_CHANGES.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeactivationReason {
	/// The user deactivated their own account.
	SelfService,
	/// An administrator deactivated the account.
	Admin,
}

impl DeactivationReason {
	pub(super) const fn as_str(self) -> &'static str {
		match self {
			| Self::SelfService => "self",
			| Self::Admin => "admin",
		}
	}
}

fn can_reactivate_deactivated_sso(reason: Option<&str>) -> bool { matches!(reason, Some("self")) }

impl Service {
	/// Reactivate a deactivated local SSO account.
	///
	/// This is used for users who self-deactivated and later return through the
	/// same SSO identity. The account remains the same MXID, but can
	/// authenticate again.
	pub async fn maybe_reactivate_deactivated_sso(&self, user_id: &UserId) -> Result<bool> {
		if !self.services.globals.user_is_local(user_id) {
			return Ok(false);
		}

		if !self.is_deactivated(user_id).await? {
			return Ok(false);
		}

		let Ok(origin) = self.origin(user_id).await else {
			return Ok(false);
		};

		if origin != "sso" {
			return Ok(false);
		}

		if !can_reactivate_deactivated_sso(self.deactivation_reason(user_id).await.as_deref()) {
			return Ok(false);
		}

		self.set_password(user_id, Some(PASSWORD_SENTINEL))
			.await?;
		Ok(true)
	}

	/// Returns the stored reason for an account's deactivation, if present.
	pub async fn deactivation_reason(&self, user_id: &UserId) -> Option<String> {
		self.db
			.userid_deactivation_reason
			.get(user_id)
			.await
			.deserialized()
			.ok()
	}

	/// Whether the account is external, as a registration rule with `external`
	/// made it: it joins only rooms it is invited to, creates no rooms, invites
	/// no one, and finds in the user directory only people it shares a room
	/// with. The row's presence is the whole state; no client API writes it.
	pub async fn is_external(&self, user_id: &UserId) -> bool {
		self.db
			.userid_external
			.exists(user_id)
			.await
			.is_ok()
	}

	/// Mark the account external. Nothing clears the mark.
	pub fn set_external(&self, user_id: &UserId) { self.db.userid_external.insert(user_id, b""); }

	/// Refuse an external account's room creation.
	pub async fn external_create_check(&self, user_id: &UserId) -> Result {
		self.is_external(user_id)
			.await
			.is_false()
			.ok_or_else(|| err!(Request(Forbidden("This account may not create rooms."))))
	}

	/// Refuse an external account's room alias creation.
	pub async fn external_alias_check(&self, user_id: &UserId) -> Result {
		self.is_external(user_id)
			.await
			.is_false()
			.ok_or_else(|| err!(Request(Forbidden("This account may not create room aliases."))))
	}

	/// Refuse making an external account a server admin.
	pub async fn external_admin_check(&self, user_id: &UserId) -> Result {
		self.is_external(user_id)
			.await
			.is_false()
			.ok_or_else(|| err!(Request(Forbidden("This account may not be a server admin."))))
	}

	/// Refuse an external account's invite.
	pub async fn external_invite_check(&self, user_id: &UserId) -> Result {
		self.is_external(user_id)
			.await
			.is_false()
			.ok_or_else(|| err!(Request(Forbidden("This account may not invite others."))))
	}

	/// Refuse an external account's join to a room it is neither joined to nor
	/// invited to. A join it is already joined with stands, as a profile change
	/// resends it, and an approved knock is an invite.
	pub async fn external_join_check(&self, user_id: &UserId, room_id: &RoomId) -> Result {
		let state_cache = &self.services.state_cache;

		if !self.is_external(user_id).await
			|| state_cache.is_joined(user_id, room_id).await
			|| state_cache.is_invited(user_id, room_id).await
		{
			return Ok(());
		}

		Err!(Request(Forbidden("This account may join a room only when invited.")))
	}

	/// Compatibility repair for legacy SSO users whose origin was accidentally
	/// rewritten to `password` during account creation.
	pub async fn maybe_repair_legacy_sso_origin(&self, user_id: &UserId) -> bool {
		let oauth_sessions = self
			.services
			.oauth
			.sessions
			.get_sess_id_by_user(user_id);
		futures::pin_mut!(oauth_sessions);

		let Some(Ok(_)) = oauth_sessions.next().await else {
			return false;
		};

		let Ok(origin) = self.origin(user_id).await else {
			return false;
		};

		if origin != "password" {
			return false;
		}

		let Ok(password_hash) = self.password_hash(user_id).await else {
			return false;
		};

		if password_hash.is_empty() {
			return false;
		}

		let is_sentinel_password = password_hash == PASSWORD_SENTINEL
			|| utils::hash::verify_password(PASSWORD_SENTINEL, &password_hash).is_ok();
		if !is_sentinel_password {
			return false;
		}

		self.set_origin(user_id, "sso");
		true
	}

	/// Check the account `assigned_accounts` gives a signing-in identity that
	/// has none yet. The assignment holds the account's password lock until the
	/// link is stored, and says whether the account has an empty password.
	pub async fn assign_sso_account(&self, user_id: &UserId) -> Result<SsoAssignment> {
		let guard = self.changing_password.lock(user_id).await;
		let password = self.password_hash(user_id).await;
		let empty = password.as_deref().is_ok_and(str::is_empty);
		let refusals = [
			(!self.services.globals.user_is_local(user_id), "is not local"),
			(user_id == self.services.globals.server_user, "is the server user"),
			(password.as_ref().is_err_and(Error::is_missing), "does not exist"),
			(password.is_err(), "cannot be read"),
			(empty && self.deactivation_recorded(user_id).await, "is deactivated"),
			(
				self.services
					.oauth
					.sessions
					.is_linked(user_id)
					.await,
				"is linked to another identity",
			),
		];

		if let Some((_, reason)) = refusals.into_iter().find(|(refused, _)| *refused) {
			return Err!(Request(Forbidden(warn!(
				"This sign-in is assigned to {user_id}, which {reason}."
			))));
		}

		Ok((guard, empty))
	}

	/// Hold an account a first sign-in takes by name, new or existing, until its
	/// link is stored. It is refused if another identity, such as an
	/// assignment's, is linked to it by the account's index, which every
	/// sign-in's link writes; reading only that, an unreadable session of some
	/// other account does not refuse every first sign-in.
	pub async fn hold_unlinked_account(&self, user_id: &UserId) -> Result<SsoAssignment> {
		let guard = self.changing_password.lock(user_id).await;

		if self
			.services
			.oauth
			.sessions
			.has_indexed_link(user_id)
			.await
		{
			return Err!(Request(UserInUse(warn!("{user_id} is linked to another identity."))));
		}

		Ok((guard, false))
	}

	/// Make an assigned account without a password one SSO signs in to, in the
	/// batch that stores its link, so no reader finds one without the other.
	pub fn convert_assigned_account(&self, txn: &mut Txn, user_id: &UserId) {
		warn!(%user_id, "Making the assigned account, without a password, an SSO account.");
		txn.insert_raw(&self.db.userid_origin, user_id, "sso");
		txn.insert_raw(&self.db.userid_password, user_id, PASSWORD_SENTINEL);
	}

	/// Whether a deactivation is recorded for the account, here or in the
	/// column a migrated database left behind. Creation without a password
	/// records neither; anything unreadable counts.
	pub async fn deactivation_recorded(&self, user_id: &UserId) -> bool {
		let migrated = match self.services.db.open_cf("userid_deactivated") {
			| Ok(Some(marks)) => !marks.get(user_id).await.is_missing(),
			| Ok(None) => false,
			| Err(_) => true,
		};

		migrated
			|| !self
				.db
				.userid_deactivation_reason
				.get(user_id)
				.await
				.is_missing()
	}

	#[cfg(test)]
	fn test_can_reactivate_deactivated_sso(reason: Option<&str>) -> bool {
		can_reactivate_deactivated_sso(reason)
	}
}

#[cfg(test)]
mod tests {
	use std::{
		fs,
		path::{Path, PathBuf},
		sync::{
			Arc,
			atomic::{AtomicU64, Ordering},
		},
	};

	use ruma::user_id;
	use tracing::subscriber::NoSubscriber;
	use tuwunel_core::{
		Server,
		config::{Config, Sources},
		log::{Logging, capture::State as CaptureState},
		metrics::Metrics,
	};

	use super::{super::PASSWORD_SENTINEL, DeactivationReason, Service};
	use crate::Services;

	static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

	#[test]
	fn sso_reactivation_requires_self_deactivation_reason() {
		assert!(Service::test_can_reactivate_deactivated_sso(Some("self")));
		assert!(!Service::test_can_reactivate_deactivated_sso(None));
		assert!(!Service::test_can_reactivate_deactivated_sso(Some("admin")));
		assert!(!Service::test_can_reactivate_deactivated_sso(Some("unknown")));
	}

	#[tokio::test]
	async fn self_deactivated_sso_account_reactivates() {
		let temp_dir = unique_temp_dir();
		let services = open_services(&temp_dir).await;
		let user_id = user_id!("@alice:example.com");

		services
			.users
			.create(user_id, Some(PASSWORD_SENTINEL), Some("sso"))
			.await
			.expect("create SSO user");
		services
			.users
			.deactivate_account(user_id, DeactivationReason::SelfService)
			.await
			.expect("deactivate SSO user");

		let reactivated = services
			.users
			.maybe_reactivate_deactivated_sso(user_id)
			.await
			.expect("reactivation check should succeed");

		assert!(reactivated, "self-deactivated SSO user should reactivate");
		assert!(
			!services
				.users
				.is_deactivated(user_id)
				.await
				.expect("deactivation state"),
			"user should no longer be deactivated"
		);
		assert_eq!(
			services
				.users
				.origin(user_id)
				.await
				.expect("origin"),
			"sso",
			"reactivation should preserve SSO origin"
		);
		assert_eq!(
			services
				.users
				.password_hash(user_id)
				.await
				.expect("password hash"),
			PASSWORD_SENTINEL,
			"reactivation should restore the sentinel password"
		);

		cleanup_temp_dir(&temp_dir);
	}

	#[tokio::test]
	async fn admin_deactivated_sso_account_does_not_reactivate() {
		let temp_dir = unique_temp_dir();
		let services = open_services(&temp_dir).await;
		let user_id = user_id!("@alice:example.com");

		services
			.users
			.create(user_id, Some(PASSWORD_SENTINEL), Some("sso"))
			.await
			.expect("create SSO user");
		services
			.users
			.deactivate_account(user_id, DeactivationReason::Admin)
			.await
			.expect("deactivate SSO user");

		let reactivated = services
			.users
			.maybe_reactivate_deactivated_sso(user_id)
			.await
			.expect("reactivation check should succeed");

		assert!(!reactivated, "admin-deactivated SSO user must stay deactivated");
		assert!(
			services
				.users
				.is_deactivated(user_id)
				.await
				.expect("deactivation state"),
			"user should remain deactivated"
		);

		cleanup_temp_dir(&temp_dir);
	}

	#[tokio::test]
	async fn self_deactivated_password_account_does_not_reactivate_as_sso() {
		let temp_dir = unique_temp_dir();
		let services = open_services(&temp_dir).await;
		let user_id = user_id!("@alice:example.com");

		services
			.users
			.create(user_id, Some("password"), Some("password"))
			.await
			.expect("create password user");
		services
			.users
			.deactivate_account(user_id, DeactivationReason::SelfService)
			.await
			.expect("deactivate password user");

		let reactivated = services
			.users
			.maybe_reactivate_deactivated_sso(user_id)
			.await
			.expect("reactivation check should succeed");

		assert!(!reactivated, "password-origin user must not reactivate via SSO path");
		assert!(
			services
				.users
				.is_deactivated(user_id)
				.await
				.expect("deactivation state"),
			"user should remain deactivated"
		);
		assert_eq!(
			services
				.users
				.origin(user_id)
				.await
				.expect("origin"),
			"password",
			"SSO reactivation check should not change user origin"
		);

		cleanup_temp_dir(&temp_dir);
	}

	async fn open_services(temp_dir: &Path) -> Arc<Services> {
		let db_path = temp_dir.join("db");
		let config_path = temp_dir.join("tuwunel.toml");

		fs::create_dir_all(temp_dir).expect("create test temp dir");
		let config_contents = format!(
			r#"[global]
server_name = "example.com"
database_path = "{}"
"#,
			db_path.display(),
		);
		fs::write(&config_path, config_contents).expect("write test config");

		let figment = Config::load(std::iter::once(config_path.as_path())).expect("load config");
		let config = Config::new(&figment).expect("parse config");
		let log = Logging {
			reload: Default::default(),
			capture: Arc::new(CaptureState::new()),
			subscriber: Arc::new(NoSubscriber::new()),
		};
		let runtime = tokio::runtime::Handle::current();
		let metrics = Metrics::new(Some(&runtime));
		let server =
			Arc::new(Server::new(config, Sources::default(), Some(&runtime), log, metrics));

		Services::build(server)
			.await
			.expect("build services")
	}

	fn unique_temp_dir() -> PathBuf {
		let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
		let pid = std::process::id();
		let path = std::env::temp_dir().join(format!("tuwunel-users-{pid}-{id}"));
		fs::create_dir_all(&path).expect("create unique test dir");
		path
	}

	fn cleanup_temp_dir(path: &Path) { drop(fs::remove_dir_all(path)); }
}
