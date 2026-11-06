use futures::{future::join, pin_mut, poll};
use ruma::{UserId, user_id};
use tokio::sync::Notify;
use tuwunel_core::{Result, config::Figment};

use super::{DeactivationReason, PASSWORD_SENTINEL};
use crate::{Services, oauth::Session, test_utils::fixture};

const OLD_PASSWORD: &str = "old password";
const NEW_PASSWORD: &str = "new password";

#[tokio::test]
async fn deactivation_before_password_change_rejects_write() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let users = &fixture.services.users;
	let user_id = user_id!("@deactivation-first:localhost");

	users
		.create(user_id, Some(OLD_PASSWORD), None)
		.await?;

	let password_guard = users.changing_password.lock(user_id).await;
	let password_clear = users.set_password(user_id, None);
	let password_change = users.set_password_if_active(user_id, Some(NEW_PASSWORD));

	pin_mut!(password_clear);
	pin_mut!(password_change);
	assert!(poll!(password_clear.as_mut()).is_pending());
	assert!(poll!(password_change.as_mut()).is_pending());

	drop(password_guard);

	password_clear.await?;
	assert!(password_change.await.is_err());
	assert!(users.is_deactivated(user_id).await?);

	Ok(())
}

#[tokio::test]
async fn password_change_before_deactivation_leaves_disabled() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let users = &fixture.services.users;
	let user_id = user_id!("@password-first:localhost");

	users
		.create(user_id, Some(OLD_PASSWORD), None)
		.await?;

	let checked = Notify::new();
	let release = Notify::new();
	let after_check = async {
		checked.notify_one();
		release.notified().await;
	};

	let password_change =
		users.set_password_if_active_after(user_id, Some(NEW_PASSWORD), after_check);

	let deactivate = async {
		checked.notified().await;

		let password_clear = users.set_password(user_id, None);

		pin_mut!(password_clear);
		assert!(poll!(password_clear.as_mut()).is_pending());
		release.notify_one();

		password_clear.await
	};

	let (password_change, password_clear) = join(password_change, deactivate).await;

	password_change?;
	password_clear?;
	assert!(users.is_deactivated(user_id).await?);

	Ok(())
}

#[tokio::test]
async fn assignment_never_reactivates_a_deactivation_queued_first() -> Result {
	Box::pin(assignment_against_deactivation(false)).await
}

#[tokio::test]
async fn deactivation_queued_after_an_assignment_still_deactivates() -> Result {
	Box::pin(assignment_against_deactivation(true)).await
}

/// With the password lock held, a deactivation and an assignment queue on it,
/// the assignment first when `assignment_first`. Whichever takes the lock
/// first, the account ends deactivated: a deactivation first is seen whole and
/// refuses the assignment, and one after the assignment empties the password
/// it set.
async fn assignment_against_deactivation(assignment_first: bool) -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let users = &fixture.services.users;
	let user_id = user_id!("@assigned-deactivated:localhost");

	users.create(user_id, None, None).await?;

	let password_guard = users.changing_password.lock(user_id).await;
	let deactivate = users.deactivate_account(user_id, DeactivationReason::Admin);
	let assign = assign(&fixture.services, user_id);

	pin_mut!(deactivate);
	pin_mut!(assign);

	if assignment_first {
		assert!(poll!(assign.as_mut()).is_pending());
		assert!(poll!(deactivate.as_mut()).is_pending());
	} else {
		assert!(poll!(deactivate.as_mut()).is_pending());
		assert!(poll!(assign.as_mut()).is_pending());
	}

	drop(password_guard);

	let (deactivated, assigned) = join(deactivate, assign).await;

	deactivated?;
	assert!(!assignment_first || assigned.is_ok(), "the assignment queued first goes ahead");
	assert!(users.is_deactivated(user_id).await?);
	assert_eq!(
		users
			.deactivation_reason(user_id)
			.await
			.as_deref(),
		Some("admin")
	);

	Ok(())
}

/// An assignment as a sign-in runs it: the conversion is written with the
/// identity's link, then the account is released.
async fn assign(services: &Services, user_id: &UserId) -> Result {
	let (guard, empty) = services.users.assign_sso_account(user_id).await?;
	let session = Session {
		sess_id: Some("assigned-session".to_owned()),
		user_id: Some(user_id.to_owned()),
		..Default::default()
	};

	services.oauth.sessions.put(&session).await;

	if empty {
		let mut txn = services.db.txn();

		services
			.users
			.convert_assigned_account(&mut txn, user_id);
		txn.execute();
	}

	drop(guard);

	Ok(())
}

#[tokio::test]
async fn unlinked_hold_refuses_a_link_stored_while_it_waited() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let users = &fixture.services.users;
	let user_id = user_id!("@fallback-held:localhost");

	users
		.create(user_id, Some(PASSWORD_SENTINEL), Some("sso"))
		.await?;

	let password_guard = users.changing_password.lock(user_id).await;
	let hold = users.hold_unlinked_account(user_id);

	pin_mut!(hold);
	assert!(poll!(hold.as_mut()).is_pending());

	// An assignment holding the lock links the account to its identity.
	let session = Session {
		sess_id: Some("assigned-session".to_owned()),
		user_id: Some(user_id.to_owned()),
		..Default::default()
	};

	fixture
		.services
		.oauth
		.sessions
		.put(&session)
		.await;
	drop(password_guard);

	assert!(hold.await.is_err(), "the held account was linked meanwhile");

	Ok(())
}

#[tokio::test]
async fn password_locks_are_independent_and_cancel_safe() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let users = &fixture.services.users;
	let user_a = user_id!("@password-lock-a:localhost");
	let user_b = user_id!("@password-lock-b:localhost");

	users
		.create(user_a, Some(OLD_PASSWORD), None)
		.await?;

	users
		.create(user_b, Some(OLD_PASSWORD), None)
		.await?;

	let password_guard = users.changing_password.lock(user_a).await;

	let mut queued = Box::pin(users.set_password_if_active(user_a, Some(NEW_PASSWORD)));

	assert!(poll!(queued.as_mut()).is_pending());
	drop(queued);

	users
		.set_password_if_active(user_b, Some(NEW_PASSWORD))
		.await?;

	drop(password_guard);

	users
		.set_password_if_active(user_a, Some(NEW_PASSWORD))
		.await?;

	Ok(())
}

#[tokio::test]
async fn rejected_password_change_releases_lock() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let users = &fixture.services.users;
	let user_id = user_id!("@password-origin:localhost");

	users
		.create(user_id, Some(PASSWORD_SENTINEL), Some("ldap"))
		.await?;

	assert!(
		users
			.set_password_if_active(user_id, Some(NEW_PASSWORD))
			.await
			.is_err()
	);

	users.set_password(user_id, None).await?;
	assert!(users.is_deactivated(user_id).await?);

	Ok(())
}
