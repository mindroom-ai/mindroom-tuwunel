//! Registration invites: addresses an administrator lets register, each for
//! one sign-in, beside the provider's registration rules.

use tuwunel_core::{
	Result, config::RegistrationRule, ruma::events::room::member::MembershipState,
};
use tuwunel_service::{Services, threepid::canonicalize_email, users::RegistrationInvite};

use super::{
	alias, make_rooms, membership_of, refused, run, sign_in, signed_in, update_rules, user_id,
};

/// Store the invite of an address, as the admin API does once it has checked
/// the request.
fn invite(services: &Services, email: &str, rooms: &[&str], external: bool) -> Result {
	let invite = RegistrationInvite {
		rooms: rooms.iter().copied().map(alias).collect(),
		external,
	};

	services
		.users
		.set_registration_invite(&canonicalize_email(email)?, &invite);

	Ok(())
}

/// Whether the address still has an invite.
async fn pending(services: &Services, email: &str) -> Result<bool> {
	Ok(services
		.users
		.registration_invite(email)
		.await?
		.is_some())
}

/// A verified address no rule lists is refused until it is invited, in
/// whatever case; its account is then external as the invite says and joined
/// to the invite's room, the invite is used up, and the account signs in again
/// without one.
#[test]
fn invite_admits_once() -> Result {
	run("sso-registration-invites-admits", async |services, base| {
		let rooms = make_rooms(services, base).await?;

		refused(services, base, "other").await?;
		invite(services, "OTHER@school.example", &["#class:localhost"], true)?;

		let response = sign_in(base, "other").await?;
		let other = signed_in(services, &response).await?;

		assert_eq!(other, user_id(services, "other")?);
		assert!(services.users.is_external(&other).await, "the invite makes it external");
		assert_eq!(
			membership_of(services, &other, &rooms.class).await,
			Some(MembershipState::Join)
		);
		assert!(!pending(services, "other@school.example").await?, "the invite is used up");

		let response = sign_in(base, "other").await?;

		assert_eq!(signed_in(services, &response).await?, other);

		Ok(())
	})
}

/// An invite that was removed admits no one.
#[test]
fn removed_invite_refuses() -> Result {
	run("sso-registration-invites-removed", async |services, base| {
		make_rooms(services, base).await?;
		invite(services, "other@school.example", &[], false)?;

		let users = &services.users;

		assert!(
			users
				.remove_registration_invite("other@school.example")
				.await,
			"there was an invite"
		);
		assert!(
			!users
				.remove_registration_invite("other@school.example")
				.await,
			"it is gone"
		);

		refused(services, base, "other").await
	})
}

/// An invite admits only an address the provider vouches for, and stays for
/// the sign-in that is.
#[test]
fn unverified_address_is_not_admitted() -> Result {
	run("sso-registration-invites-unverified", async |services, base| {
		make_rooms(services, base).await?;
		invite(services, "nobody@elsewhere.example", &[], false)?;

		refused(services, base, "nobody").await?;

		assert!(pending(services, "nobody@elsewhere.example").await?, "the invite stays");

		Ok(())
	})
}

/// An account that meets a rule and has an invite gets both: the rule's room
/// and the invite's, and it is external because the invite says so.
#[test]
fn invite_adds_to_a_met_rule() -> Result {
	run("sso-registration-invites-adds", async |services, base| {
		let rooms = make_rooms(services, base).await?;

		update_rules(services, |rules| {
			rules.push(RegistrationRule {
				claims: [("hd".to_owned(), "visit.example".to_owned())].into(),
				rooms: vec![alias("#open:localhost")],
				external: false,
			});
		})?;

		invite(services, "vera@visit.example", &["#class:localhost", "#open:localhost"], true)?;

		let response = sign_in(base, "vera").await?;
		let vera = signed_in(services, &response).await?;

		assert!(services.users.is_external(&vera).await, "the invite makes it external");

		for room_id in [&rooms.open, &rooms.class] {
			assert_eq!(
				membership_of(services, &vera, room_id).await,
				Some(MembershipState::Join),
				"{room_id}"
			);
		}

		assert!(!pending(services, "vera@visit.example").await?, "the invite is used up");

		Ok(())
	})
}

/// Invites do not close a provider without rules: an address without one
/// still registers, as a normal account, and the invited one gets its
/// invite's terms.
#[test]
fn open_provider_stays_open() -> Result {
	run("sso-registration-invites-open", async |services, base| {
		let rooms = make_rooms(services, base).await?;

		update_rules(services, Vec::clear)?;
		invite(services, "other@school.example", &["#class:localhost"], true)?;

		let response = sign_in(base, "olga").await?;
		let olga = signed_in(services, &response).await?;

		assert!(!services.users.is_external(&olga).await, "olga is not external");

		let response = sign_in(base, "other").await?;
		let other = signed_in(services, &response).await?;

		assert!(services.users.is_external(&other).await, "the invited account is external");
		assert_eq!(
			membership_of(services, &other, &rooms.class).await,
			Some(MembershipState::Join)
		);

		Ok(())
	})
}

/// An invite naming a room that cannot be joined still admits its address:
/// the sign-in succeeds and the invite is used up.
#[test]
fn unjoinable_room_does_not_fail_the_sign_in() -> Result {
	run("sso-registration-invites-unjoinable", async |services, base| {
		make_rooms(services, base).await?;
		invite(services, "other@school.example", &["#nowhere:localhost"], false)?;

		let response = sign_in(base, "other").await?;
		let other = signed_in(services, &response).await?;

		assert_eq!(other, user_id(services, "other")?);
		assert!(!pending(services, "other@school.example").await?, "the invite is used up");

		Ok(())
	})
}

/// An account that exists already is not changed by an invite for its
/// address, and leaves the invite where it is.
#[test]
fn existing_account_keeps_its_terms() -> Result {
	run("sso-registration-invites-existing", async |services, base| {
		let rooms = make_rooms(services, base).await?;

		update_rules(services, Vec::clear)?;

		let response = sign_in(base, "other").await?;
		let other = signed_in(services, &response).await?;

		invite(services, "other@school.example", &["#class:localhost"], true)?;

		let response = sign_in(base, "other").await?;

		assert_eq!(signed_in(services, &response).await?, other);
		assert!(!services.users.is_external(&other).await, "the account is not made external");
		assert_eq!(membership_of(services, &other, &rooms.class).await, None);
		assert!(pending(services, "other@school.example").await?, "the invite stays");

		Ok(())
	})
}
