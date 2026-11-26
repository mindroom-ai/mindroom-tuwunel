use std::{
	collections::BTreeMap,
	time::{Duration, Instant},
};

use ruma::{OwnedUserId, UserId};
use serde_json::{Map, Value};
use tuwunel_core::{debug, implement, trace};

use super::{Sessions, UserInfo};

/// Lifetime of an admin-approved pending OAuth association before it expires.
pub const ASSOCIATION_PENDING_LIFETIME: Duration = Duration::from_mins(30);

/// One-time pending OAuth associations indexed by provider ID and Matrix user.
#[derive(Default)]
pub(super) struct Pending(BTreeMap<String, Claimants>);

type Claimants = BTreeMap<OwnedUserId, PendingAssociation>;
pub type Claims = BTreeMap<String, String>;

struct PendingAssociation {
	claims: Claims,
	expires_at: Instant,
}

impl Pending {
	fn prune(&mut self, now: Instant) {
		self.0.retain(|_, claimants| {
			claimants.retain(|_, entry| now < entry.expires_at);
			!claimants.is_empty()
		});
	}

	fn set(
		&mut self,
		idp_id: &str,
		user_id: &UserId,
		claims: Claims,
		now: Instant,
	) -> Option<Claims> {
		assert!(
			!claims.is_empty(),
			"Must not insert empty set of claims into association_pending."
		);

		self.prune(now);

		let entry = PendingAssociation {
			claims,
			expires_at: now
				.checked_add(ASSOCIATION_PENDING_LIFETIME)
				.unwrap_or(now),
		};

		self.0
			.entry(idp_id.into())
			.or_default()
			.insert(user_id.into(), entry)
			.map(|previous| previous.claims)
	}

	fn take(&mut self, idp_id: &str, userinfo: &UserInfo, now: Instant) -> Vec<OwnedUserId> {
		self.prune(now);

		let Some(claimants) = self.0.get_mut(idp_id) else {
			return Vec::new();
		};

		let claiming = serde_json::to_value(userinfo)
			.expect("Failed to transform user_info into serde_json::Value");

		let claiming = claiming
			.as_object()
			.expect("Failed to interpret user_info as object");

		assert!(
			!claiming.is_empty(),
			"Expecting at least one claim from user_info such as `sub`"
		);

		debug!(?idp_id, ?claiming, "taking pending associations");

		let mut matched = Vec::new();
		claimants.retain(|user_id, entry| {
			let claimant = &entry.claims;
			trace!(?user_id, ?claimant, "checking against pending association");

			if claims_match(claiming, claimant) {
				matched.push(user_id.clone());
				return false;
			}

			true
		});

		if claimants.is_empty() {
			self.0.remove(idp_id);
		}

		matched
	}

	fn remove_provider(&mut self, idp_id: &str) { self.0.remove(idp_id); }

	fn remove_user(&mut self, user_id: &UserId, idp_id: Option<&str>) {
		self.0.retain(|provider, claiming| {
			if idp_id.is_none_or(|id| id == provider.as_str()) {
				claiming.remove(user_id);
			}

			!claiming.is_empty()
		});
	}

	fn is_pending(&mut self, user_id: &UserId, now: Instant) -> bool {
		self.prune(now);
		self.0
			.values()
			.any(|claiming| claiming.contains_key(user_id))
	}
}

fn claims_match(claiming: &Map<String, Value>, claimant: &Claims) -> bool {
	assert!(
		!claimant.is_empty(),
		"Must not match empty set of claims; should not exist in association_pending."
	);

	for (claim, value) in claimant {
		if claiming.get(claim).and_then(Value::as_str) != Some(value) {
			return false;
		}
	}

	true
}

#[implement(Sessions)]
pub fn set_user_association_pending(
	&self,
	idp_id: &str,
	user_id: &UserId,
	claims: Claims,
) -> Option<Claims> {
	self.association_pending
		.lock()
		.expect("locked")
		.set(idp_id, user_id, claims, Instant::now())
}

#[implement(Sessions)]
#[must_use]
pub fn take_user_associations_pending(
	&self,
	idp_id: &str,
	userinfo: &UserInfo,
) -> Vec<OwnedUserId> {
	self.association_pending
		.lock()
		.expect("locked")
		.take(idp_id, userinfo, Instant::now())
}

#[implement(Sessions)]
pub fn remove_provider_associations_pending(&self, idp_id: &str) {
	self.association_pending
		.lock()
		.expect("locked")
		.remove_provider(idp_id);
}

#[implement(Sessions)]
pub fn remove_user_association_pending(&self, user_id: &UserId, idp_id: Option<&str>) {
	self.association_pending
		.lock()
		.expect("locked")
		.remove_user(user_id, idp_id);
}

#[implement(Sessions)]
#[must_use]
pub fn is_user_association_pending(&self, user_id: &UserId) -> bool {
	self.association_pending
		.lock()
		.expect("locked")
		.is_pending(user_id, Instant::now())
}

#[cfg(test)]
mod tests {
	use std::time::{Duration, Instant};

	use ruma::user_id;

	use super::{ASSOCIATION_PENDING_LIFETIME, Claims, Pending, UserInfo};

	fn claims(pairs: &[(&str, &str)]) -> Claims {
		pairs
			.iter()
			.map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
			.collect()
	}

	fn userinfo(sub: &str, preferred_username: &str) -> UserInfo {
		UserInfo {
			sub: sub.to_owned(),
			preferred_username: Some(preferred_username.to_owned()),
			..Default::default()
		}
	}

	#[test]
	fn take_consumes_pending_association_once() {
		let mut pending = Pending::default();
		let now = Instant::now();
		let alice = user_id!("@alice:example.com");
		let info = userinfo("alice-1", "alice");
		let rule = claims(&[("preferred_username", "alice")]);

		assert_eq!(pending.set("idp", alice, rule, now), None, "initial set replaces nothing");
		assert!(pending.is_pending(alice, now), "association is pending before take");

		let taken = pending.take("idp", &info, now);

		assert_eq!(taken, vec![alice.to_owned()], "first take matches and consumes");
		assert!(pending.take("idp", &info, now).is_empty(), "second take must not match");
		assert!(!pending.is_pending(alice, now), "consumed association is no longer pending");
		assert!(pending.0.is_empty(), "emptied provider map is removed after take");
	}

	#[test]
	fn take_consumes_all_overlapping_associations() {
		let mut pending = Pending::default();
		let now = Instant::now();
		let alice = user_id!("@alice:example.com");
		let bob = user_id!("@bob:example.com");
		let info = userinfo("alice-1", "alice");

		pending.set("idp", alice, claims(&[("sub", "alice-1")]), now);
		pending.set("idp", bob, claims(&[("preferred_username", "alice")]), now);

		let taken = pending.take("idp", &info, now);

		assert_eq!(
			taken,
			vec![alice.to_owned(), bob.to_owned()],
			"take returns every overlapping association"
		);
		assert!(!pending.is_pending(alice, now), "alice association is consumed");
		assert!(!pending.is_pending(bob, now), "bob association is consumed");
		assert!(pending.0.is_empty(), "emptied provider map is removed after take");
		assert!(pending.take("idp", &info, now).is_empty(), "second take returns nothing");
	}

	#[test]
	fn pending_association_expires_at_lifetime_boundary() {
		let mut pending = Pending::default();
		let now = Instant::now();
		let alice = user_id!("@alice:example.com");
		let info = userinfo("alice-1", "alice");
		let just_short = ASSOCIATION_PENDING_LIFETIME.saturating_sub(Duration::from_secs(1));
		let before_expiry = now
			.checked_add(just_short)
			.expect("instant within range");
		let at_expiry = now
			.checked_add(ASSOCIATION_PENDING_LIFETIME)
			.expect("instant within range");

		let initial = claims(&[("preferred_username", "alice")]);
		let updated = claims(&[("preferred_username", "alice-new")]);
		let latest = claims(&[("preferred_username", "alice-latest")]);

		pending.set("idp", alice, initial.clone(), now);

		assert!(pending.is_pending(alice, before_expiry), "pending before expiry");

		let taken = pending.take("idp", &info, before_expiry);

		assert_eq!(taken, vec![alice.to_owned()], "association matches just before expiry");

		pending.set("idp", alice, initial.clone(), now);

		let expired = pending.take("idp", &info, at_expiry);

		assert!(expired.is_empty(), "take ignores expired entry");

		pending.set("idp", alice, initial.clone(), now);
		assert!(!pending.is_pending(alice, at_expiry), "expired entry is not pending");

		// Overwriting an expired entry prunes first and reports no replacement.
		pending.set("idp", alice, initial, now);

		let replaced = pending.set("idp", alice, updated.clone(), at_expiry);

		assert_eq!(replaced, None, "setting after expiry reports no replacement");
		assert!(pending.is_pending(alice, at_expiry), "freshly set entry is pending");

		let replaced = pending.set("idp", alice, latest, at_expiry);

		assert_eq!(replaced, Some(updated), "setting before expiry returns previous claims");
	}

	#[test]
	fn remove_user_respects_provider_filter() {
		let mut pending = Pending::default();
		let now = Instant::now();
		let alice = user_id!("@alice:example.com");
		let bob = user_id!("@bob:example.com");
		let alice_info = userinfo("alice-1", "alice");
		let bob_info = userinfo("bob-1", "bob");

		pending.set("idp-a", alice, claims(&[("preferred_username", "alice")]), now);
		pending.set("idp-b", alice, claims(&[("preferred_username", "alice")]), now);
		pending.set("idp-b", bob, claims(&[("preferred_username", "bob")]), now);

		pending.remove_user(alice, Some("idp-a"));

		assert!(!pending.0.contains_key("idp-a"), "emptied provider map is removed");
		let removed = pending.take("idp-a", &alice_info, now);

		assert!(removed.is_empty(), "removed from idp-a");
		assert!(pending.is_pending(alice, now), "other provider entry stays intact");

		pending.set("idp-a", alice, claims(&[("preferred_username", "alice")]), now);
		pending.remove_user(alice, None);

		assert!(!pending.is_pending(alice, now), "None removes user across all providers");
		assert!(!pending.0.contains_key("idp-a"), "emptied provider map is removed for None");
		assert!(pending.is_pending(bob, now), "other users on shared provider stay pending");
		assert_eq!(pending.take("idp-b", &bob_info, now), vec![bob.to_owned()], "bob remains");
	}
}
