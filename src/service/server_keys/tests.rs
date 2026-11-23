use ruma::{
	MilliSecondsSinceUnixEpoch, OwnedServerSigningKeyId,
	api::federation::discovery::{ServerSigningKeys, VerifyKey},
	serde::Base64,
	server_name,
};
use tuwunel_core::{Result, config::Figment};
use tuwunel_database::Json;

use super::MAX_STORED_KEYS_BYTES;
use crate::test_utils::fixture;

fn key_id(id: usize) -> OwnedServerSigningKeyId {
	format!("ed25519:{id}")
		.try_into()
		.expect("valid key id")
}

fn keys(ids: impl Iterator<Item = usize>) -> ServerSigningKeys {
	let origin = server_name!("remote.test").to_owned();
	let mut keys = ServerSigningKeys::new(origin, MilliSecondsSinceUnixEpoch::now());
	keys.verify_keys
		.extend(ids.map(|id| (key_id(id), VerifyKey::new(Base64::new(vec![0; 32])))));

	keys
}

#[tokio::test]
async fn stored_signing_keys_stay_bounded() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let service = &fixture.services.server_keys;
	let map = &fixture.services.db["server_signingkeys"];
	let origin = server_name!("remote.test");
	let has = async |id| {
		service
			.verify_key_exists(origin, &key_id(id))
			.await
	};

	// A row that grew before the limit is ignored by lookups.
	map.raw_put(origin, Json(keys(0..2000)));
	assert!(service.verify_keys_for(origin).await.is_empty());
	assert!(!has(0).await);

	// Fetches with new key ids replace the row instead of growing it.
	for batch in 0..40 {
		service
			.add_signing_keys(keys(batch * 50..(batch + 1) * 50), &[])
			.await;

		assert!(map.get(origin).await?.len() <= MAX_STORED_KEYS_BYTES);
		assert!(has(batch * 50).await);
	}

	// A fetched document too large on its own is not stored.
	service
		.add_signing_keys(keys(5000..7000), &[])
		.await;
	assert!(!has(5000).await);
	assert!(has(1950).await);

	Ok(())
}

/// A rollover keeps the keys the caller is acquiring.
///
/// Two documents for one server each fit the row and together do not. The
/// second must not drop the key the first brought while one acquisition
/// wants both.
#[tokio::test]
async fn rollover_keeps_the_wanted_keys() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let service = &fixture.services.server_keys;
	let origin = server_name!("remote.test");
	let wanted = [key_id(0), key_id(1000)];

	service
		.add_signing_keys(keys(0..700), &wanted)
		.await;
	let stored = service
		.add_signing_keys(keys(1000..1700), &wanted)
		.await;

	for key_id in &wanted {
		assert!(service.verify_key_exists(origin, key_id).await, "{key_id}");
		assert!(stored.verify_keys.contains_key(key_id), "{key_id}");
	}

	// Keys nobody wants still roll over.
	assert!(
		!service
			.verify_key_exists(origin, &key_id(1))
			.await
	);

	Ok(())
}
