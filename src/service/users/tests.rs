use std::iter::once;

use futures::{StreamExt, future::join, pin_mut, poll};
use ruma::{
	OneTimeKeyAlgorithm, OneTimeKeyId, UserId, device_id,
	encryption::OneTimeKey,
	serde::{Base64, Raw, base64::Standard},
	signatures::{Ed25519KeyPair, KeyPair, to_canonical_json_string_for_signing},
	user_id,
};
use serde_json::{Value, from_value, json};
use tokio::sync::Notify;
use tuwunel_core::{Result, config::Figment, utils::to_canonical_object};
use tuwunel_database::{Cbor, Deserialized, Json, serialize_key};

use super::{DeactivationReason, PASSWORD_SENTINEL, Service, device::DeviceLock};
use crate::{
	Services,
	oauth::Session,
	sync::{Connection, into_connection_key},
	test_utils::{create_devices, fixture, released_after, released_by},
};

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

#[tokio::test]
async fn unused_fallback_keys_stay_with_their_device() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let users = &fixture.services.users;
	let user_id = user_id!("@fallback:localhost");
	let device_id = device_id!("FALLBACK");
	let longer_id = device_id!("FALLBACK2");

	create_devices(users, user_id, &[device_id, longer_id]).await?;

	let key_id = <&OneTimeKeyId>::try_from("signed_curve25519:AAAAAQ")?;
	let key: Raw<OneTimeKey> = from_value(json!("fallback key"))?;

	users
		.add_fallback_keys(user_id, longer_id, once((key_id, &key)))
		.await?;

	let unused = async |device_id| {
		users
			.unused_fallback_key_algorithms(user_id, device_id)
			.collect::<Vec<_>>()
			.await
	};

	assert_eq!(unused(device_id).await, []);
	assert_eq!(unused(longer_id).await, [OneTimeKeyAlgorithm::SignedCurve25519]);

	users
		.take_fallback_key(user_id, longer_id, &OneTimeKeyAlgorithm::SignedCurve25519)
		.await?;

	assert_eq!(unused(longer_id).await, []);

	Ok(())
}

#[tokio::test]
async fn signature_merge_waits_for_the_key_row() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let (users, keyid_key) = (&services.users, &services.db["keyid_key"]);
	let user_id = user_id!("@signing:localhost");
	let device_id = device_id!("SIGNER");
	let signing_key_id = "ed25519:SIGNER";

	create_devices(users, user_id, &[device_id]).await?;

	let signer = key_pair("SIGNER");
	let root_key = public_key(&key_pair("ROOT"));
	let root_row = (user_id, root_key.as_str());
	let root = cross_signing_key(user_id, "master", &root_key);
	let device = json!({"user_id": user_id, "device_id": device_id,
		"keys": {signing_key_id: public_key(&signer)}});

	keyid_key.put(root_row, Json(&root));
	keyid_key.put((user_id, device_id), Json(device));
	services.db["userid_masterkeyid"].insert(user_id, serialize_key(root_row)?);

	let canonical = to_canonical_json_string_for_signing(&to_canonical_object(&root)?)?;
	let signature = signer.sign(canonical.as_bytes()).base64();
	let signatures = [(signing_key_id.to_owned(), signature.clone())].into();
	let other = user_id!("@other:localhost");
	let other_signature = json!({other: {"ed25519:OTHER": "other signature"}});
	let signed_root = signed_cross_signing_key(user_id, "master", &root_key, &other_signature);
	let row_lock = lock_key_row(users, root_row).await;
	let merged_meanwhile = async { keyid_key.put(root_row, Json(&signed_root)) };
	let merge = users.sign_key(user_id, &root_key, signatures, user_id);

	released_after(row_lock, merged_meanwhile, merge).await?;

	let stored: Value = keyid_key.qry(&root_row).await.deserialized()?;

	assert_eq!(stored["signatures"][user_id.as_str()][signing_key_id], signature);
	assert_eq!(stored["signatures"][other.as_str()]["ed25519:OTHER"], "other signature");

	Ok(())
}

#[tokio::test]
async fn device_key_upload_holds_the_device_lock() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let (users, keyid_key) = (&services.users, &services.db["keyid_key"]);
	let user_id = user_id!("@device-keys:localhost");
	let (device_id, removed_id) = (device_id!("UPLOADER"), device_id!("REMOVED"));
	let keys =
		|device_id| from_value(json!({"user_id": user_id, "device_id": device_id, "keys": {}}));

	create_devices(users, user_id, &[device_id, removed_id]).await?;

	let device_lock = users.lock_device(user_id, device_id).await;

	released_by(device_lock, users.add_device_keys(user_id, device_id, &keys(device_id)?))
		.await?;

	assert!(keyid_key.contains(&(user_id, device_id)).await);

	let device_lock = users.lock_device(user_id, removed_id).await;
	let removed_meanwhile =
		async { services.db["userdeviceid_metadata"].del((user_id, removed_id)) };

	let removed_keys = keys(removed_id)?;
	let upload = users.add_device_keys(user_id, removed_id, &removed_keys);

	released_after(device_lock, removed_meanwhile, upload).await?;

	assert!(!keyid_key.contains(&(user_id, removed_id)).await);

	Ok(())
}

#[tokio::test]
async fn cross_signing_upload_waits_for_its_key_rows() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let (users, keyid_key) = (&services.users, &services.db["keyid_key"]);
	let user_id = user_id!("@cross-signing:localhost");
	let key_ids = ["ROOT", "SELF", "USER"].map(|version| public_key(&key_pair(version)));
	let [root_key, self_key, user_key] = &key_ids;
	let key = |usage, key_id: &str| from_value(cross_signing_key(user_id, usage, key_id));
	let root = Some(key("master", root_key)?);
	let self_signing = Some(key("self_signing", self_key)?);
	let user_signing = Some(key("user_signing", user_key)?);

	users.create(user_id, None, None).await?;

	let row_lock = lock_key_row(users, (user_id, self_key)).await;
	let unwritten_meanwhile = async {
		for key_id in &key_ids {
			assert!(!keyid_key.contains(&(user_id, key_id)).await);
			assert_eq!(
				users
					.is_cross_signing_key_id(user_id, key_id)
					.await
					.ok(),
				Some(false)
			);
		}
	};

	let upload =
		users.add_cross_signing_keys(user_id, &root, &self_signing, &user_signing, false);

	released_after(row_lock, unwritten_meanwhile, upload).await?;

	for key_id in &key_ids {
		assert!(keyid_key.contains(&(user_id, key_id)).await);
		assert!(
			users
				.is_cross_signing_key_id(user_id, key_id)
				.await?
		);
	}

	Ok(())
}

#[tokio::test]
async fn remote_master_key_keeps_our_signatures() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let (users, keyid_key) = (&services.users, &services.db["keyid_key"]);
	let user_id = user_id!("@remote:example.org");
	let (ours, other) = (user_id!("@ours:localhost"), user_id!("@other:localhost"));
	let root_key = public_key(&key_pair("REMOTE"));
	let root_row = (user_id, root_key.as_str());
	let root = |signatures| signed_cross_signing_key(user_id, "master", &root_key, &signatures);

	keyid_key.put(root_row, Json(root(json!({ours: {"ed25519:OURS": "ours"}}))));

	let remote = from_value(root(json!({user_id: {"ed25519:REMOTE": "theirs"}})))?;
	let row_lock = lock_key_row(users, root_row).await;

	let signed_meanwhile = async {
		let signatures = json!({ours: {"ed25519:OURS": "ours"},
			other: {"ed25519:OTHER": "other"}, user_id: {"ed25519:OLD": "old"}});

		keyid_key.put(root_row, Json(root(signatures)));
	};

	let store = users.store_remote_master_key(user_id, &remote);

	released_after(row_lock, signed_meanwhile, store).await?;

	let stored: Value = keyid_key.qry(&root_row).await.deserialized()?;
	let expected = [
		(user_id, "ed25519:REMOTE", "theirs"),
		(user_id, "ed25519:OLD", "old"),
		(ours, "ed25519:OURS", "ours"),
		(other, "ed25519:OTHER", "other"),
	];

	for (signer, key_id, signature) in expected {
		assert_eq!(stored["signatures"][signer.as_str()][key_id], signature);
	}

	assert!(
		users
			.is_cross_signing_key_id(user_id, &root_key)
			.await?
	);

	Ok(())
}

#[tokio::test]
async fn device_removal_keeps_its_connections() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let (users, sync) = (&services.users, &services.sync);
	let user_id = user_id!("@conn:localhost");
	let device_id = device_id!("D");
	let key = into_connection_key(user_id, Some(device_id), Some("room-list"));
	let next_batch = 42;

	create_devices(users, user_id, &[device_id]).await?;

	let conn = sync.load_or_init_connection(&key).await?;

	conn.lock().await.next_batch = next_batch;
	sync.store_connection(&key, &conn, &*conn.lock().await)
		.await?;

	users.remove_device(user_id, device_id).await;

	assert!(sync.is_connection_stored(&key).await);

	users
		.create_device(user_id, Some(device_id), (None, None), None, None, None)
		.await?;

	let resumed = sync.load_or_init_connection(&key).await?;

	assert_eq!(resumed.lock().await.next_batch, next_batch);

	let Cbor(stored): Cbor<Connection> = services.db["userdeviceconnid_conn"]
		.qry(&key)
		.await
		.deserialized()?;

	assert_eq!(stored.next_batch, next_batch);

	Ok(())
}

fn key_pair(version: &str) -> Ed25519KeyPair {
	Ed25519KeyPair::from_der(&Ed25519KeyPair::generate(), version.into())
		.expect("generated key pair should parse")
}

fn public_key(key_pair: &Ed25519KeyPair) -> String {
	Base64::<Standard, _>::new(key_pair.public_key()).encode()
}

fn cross_signing_key(user_id: &UserId, usage: &str, key_id: &str) -> Value {
	signed_cross_signing_key(user_id, usage, key_id, &json!({}))
}

fn signed_cross_signing_key(
	user_id: &UserId,
	usage: &str,
	key_id: &str,
	signatures: &Value,
) -> Value {
	json!({"user_id": user_id, "usage": [usage],
		"keys": {format!("ed25519:{key_id}"): key_id}, "signatures": signatures})
}

async fn lock_key_row(users: &Service, (user_id, key_id): (&UserId, &str)) -> DeviceLock {
	// Key rows share the device lock, keyed by key ID.
	users.lock_device(user_id, key_id.into()).await
}

#[tokio::test]
async fn create_new_refuses_an_account_that_exists() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let users = &fixture.services.users;
	let user_id = user_id!("@created-first:localhost");

	users.create(user_id, None, None).await?;

	let refused = users
		.create_new(user_id, Some(NEW_PASSWORD), Some("sso"))
		.await;

	assert!(refused.is_err(), "create_new kept the account that exists");
	assert_eq!(users.password_hash(user_id).await?, "");
	assert_eq!(users.origin(user_id).await?, "password");

	Ok(())
}
