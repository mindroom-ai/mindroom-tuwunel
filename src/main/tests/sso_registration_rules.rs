#![cfg(test)]

mod admin;
mod client;
mod fixture;

#[path = "client/admin_api.rs"]
mod admin_api;
#[path = "client/get.rs"]
mod get;
#[path = "client/membership.rs"]
mod membership;
#[path = "client/put.rs"]
mod put;

use std::{collections::HashMap, net::TcpListener, pin::pin, time::Duration};

use axum::{
	Form, Json, Router,
	extract::State,
	http::HeaderMap,
	routing::{get, post},
};
use axum_server::from_tcp;
use futures::future::select;
use reqwest::{
	Method, Response, StatusCode, Url,
	header::{AUTHORIZATION, COOKIE, LOCATION, SET_COOKIE},
	redirect::Policy,
};
use serde_json::{Value, json};
use tuwunel_core::{
	Err, Result,
	config::{RegistrationRule, check::reload},
	err,
	result::NotFound,
	ruma::{
		OwnedRoomId, OwnedRoomOrAliasId, OwnedUserId, RoomAliasId, RoomId, UserId,
		events::{StateEventType, room::member::MembershipState},
	},
};
use tuwunel_service::{
	Services,
	oauth::{fallback_localpart, unique_id_sub},
};

use self::{
	client::{Client, poll_until, register},
	fixture::boot,
};

type TokenForm = HashMap<String, String>;

const IDP: &str = "test-idp";

const CLIENT: &str = "https://client.example/callback";

const CREATOR_TOKEN: &str = "sso-registration-rules-creator-token";

const AARON_TOKEN: &str = "sso-registration-rules-aaron-token";

const MALLORY_TOKEN: &str = "sso-registration-rules-mallory-token";

const GUEST_TOKEN: &str = "sso-registration-rules-guest-token";

const AMY_TOKEN: &str = "sso-registration-rules-amy-token";

const OUTSIDER_TOKEN: &str = "sso-registration-rules-outsider-token";

/// The provider's rules: a hosted domain for `#staff` and `#open`, one
/// address for `#class` that makes its account external, an address the
/// provider says is unverified, a hosted domain whose first room this server
/// never joined, one whose first alias no room has yet, and one naming the
/// admin room.
const RULES: &str = r##"[
	{ claims = { hd = "example.com" }, rooms = ["#staff:localhost", "#open:localhost"] },
	{ claims = { email = "guest@school.example" }, rooms = ["#class:localhost"], external = true },
	{ claims = { email = "guest2@school.example" }, rooms = ["#class:localhost"] },
	{ claims = { hd = "far.example" }, rooms = ["!never:localhost", "#open:localhost"] },
	{ claims = { hd = "late.example" }, rooms = ["#late:localhost", "#open:localhost"] },
	{ claims = { hd = "admin.example" }, rooms = ["#admins:localhost"] }
]"##;

const REFUSAL: &str =
	"This sign-in may not create an account here; ask an administrator to invite you.";

const REASON: &str = "Automatically joining this room upon registration";

const UNINVITED: &str = "This account may join a room only when invited.";

const NO_ROOMS: &str = "This account may not create rooms.";

const NO_ALIASES: &str = "This account may not create room aliases.";

const NO_ADMIN: &str = "This account may not be a server admin.";

const NO_INVITES: &str = "This account may not invite others.";

/// The rooms the rules name, and two that external accounts are kept out of,
/// made by one local account.
struct Rooms {
	staff: OwnedRoomId,
	open: OwnedRoomId,
	class: OwnedRoomId,
	public: OwnedRoomId,
	club: OwnedRoomId,
}

/// An identity meeting the hosted-domain rule gets an account joined to its
/// rooms and no other: the invite-only room after an invite from its creator,
/// its most powerful member, rather than from a member at power 0 who may
/// invite as well and comes first; the public one without an invite; each
/// with the registration reason.
#[test]
fn hosted_domain_joins_its_rooms() -> Result {
	run("sso-registration-rules-hd", async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let creator = creator(services, base);
		let aaron_id = register(services, "aaron", AARON_TOKEN).await?;
		let aaron = Client { services, base, token: AARON_TOKEN };

		creator
			.admit(&rooms.staff, &aaron, &aaron_id)
			.await?;

		let power_levels = services
			.state_accessor
			.get_power_levels(&rooms.staff)
			.await?;

		assert!(power_levels.user_can_invite(&aaron_id), "aaron may invite");

		let response = sign_in(base, "amy").await?;
		let amy = signed_in(services, &response).await?;

		assert_eq!(
			membership_of(services, &amy, &rooms.staff).await,
			Some(MembershipState::Join)
		);
		assert_eq!(membership_of(services, &amy, &rooms.open).await, Some(MembershipState::Join));
		assert_eq!(membership_of(services, &amy, &rooms.class).await, None);

		let staff_invite = member_event(&creator, &rooms.staff, &amy, "invite").await?;
		let staff_join = member_event(&creator, &rooms.staff, &amy, "join").await?;
		let open_join = member_event(&creator, &rooms.open, &amy, "join").await?;

		assert_eq!(
			staff_invite.map(|event| event["sender"].clone()),
			Some(json!("@creator:localhost"))
		);
		assert_eq!(member_event(&creator, &rooms.open, &amy, "invite").await?, None);

		for join in [staff_join, open_join] {
			assert_eq!(join.map(|event| event["content"]["reason"].clone()), Some(json!(REASON)));
		}

		Ok(())
	})
}

/// A verified address in another case than its rule spells it gets an account
/// joined to that rule's room only.
#[test]
fn verified_address_joins_its_room() -> Result {
	run("sso-registration-rules-email", async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let response = sign_in(base, "guest").await?;
		let guest = signed_in(services, &response).await?;

		assert_eq!(
			membership_of(services, &guest, &rooms.class).await,
			Some(MembershipState::Join)
		);
		assert_eq!(membership_of(services, &guest, &rooms.staff).await, None);
		assert_eq!(membership_of(services, &guest, &rooms.open).await, None);

		Ok(())
	})
}

/// An identity meeting no rule is refused, and gets no account and no hold on
/// one; once a reload lists its address, the same identity signs in and joins.
#[test]
fn unlisted_address_is_refused_until_listed() -> Result {
	run("sso-registration-rules-unlisted", async |services, base| {
		let rooms = make_rooms(services, base).await?;

		refused(services, base, "other").await?;

		update_rules(services, |rules| {
			rules.push(RegistrationRule {
				claims: [("email".to_owned(), "other@school.example".to_owned())].into(),
				rooms: vec![alias("#class:localhost")],
				external: false,
			});
		})?;

		let response = sign_in(base, "other").await?;
		let other = signed_in(services, &response).await?;

		assert_eq!(other, user_id(services, "other")?);
		assert_eq!(
			membership_of(services, &other, &rooms.class).await,
			Some(MembershipState::Join)
		);

		Ok(())
	})
}

/// A listed address the provider says is unverified meets no rule.
#[test]
fn unverified_address_is_refused() -> Result {
	run("sso-registration-rules-unverified", async |services, base| {
		make_rooms(services, base).await?;
		refused(services, base, "guest2").await
	})
}

/// An account made before the rules were set signs in although it meets none.
#[test]
fn existing_account_signs_in() -> Result {
	run("sso-registration-rules-existing", async |services, base| {
		let rules = current_rules(services)?;

		update_rules(services, Vec::clear)?;

		let response = sign_in(base, "early").await?;
		let early = signed_in(services, &response).await?;

		update_rules(services, |current| *current = rules)?;

		let response = sign_in(base, "early").await?;

		assert_eq!(signed_in(services, &response).await?, early);

		Ok(())
	})
}

/// A rule room this server never joined fails alone: the sign-in succeeds and
/// the rule's other room is joined.
#[test]
fn room_never_joined_is_skipped() -> Result {
	run("sso-registration-rules-never", async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let response = sign_in(base, "far").await?;
		let far = signed_in(services, &response).await?;

		assert_eq!(membership_of(services, &far, &rooms.open).await, Some(MembershipState::Join));

		Ok(())
	})
}

/// A rule alias no room had, which a local account outside an invite-only
/// room then points at it, brings the new account no invite there: the room
/// does not name the alias as its own. The sign-in succeeds and the rule's
/// next room is joined.
#[test]
fn claimed_alias_brings_no_invite() -> Result {
	run("sso-registration-rules-claimed-alias", async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let mallory_id = register(services, "mallory", MALLORY_TOKEN).await?;
		let mallory = Client { services, base, token: MALLORY_TOKEN };

		mallory
			.put("directory/room/%23late:localhost", &json!({ "room_id": rooms.staff }))
			.await?;

		assert_eq!(membership_of(services, &mallory_id, &rooms.staff).await, None);

		let response = sign_in(base, "lena").await?;
		let lena = signed_in(services, &response).await?;
		let creator = creator(services, base);

		assert_eq!(membership_of(services, &lena, &rooms.staff).await, None);
		assert_eq!(member_event(&creator, &rooms.staff, &lena, "invite").await?, None);
		assert_eq!(
			membership_of(services, &lena, &rooms.open).await,
			Some(MembershipState::Join)
		);

		Ok(())
	})
}

/// A rule naming the admin room, by its alias or its ID, never makes the new
/// account a member there or a server admin, though an active admin there
/// may invite.
#[test]
fn admin_room_is_never_joined() -> Result {
	let options = ["registration.grant_admin_to_first_user=false"];

	run_with("sso-registration-rules-admin-room", &options, async |services, base| {
		let admin = register(services, "creator", CREATOR_TOKEN).await?;

		services.admin.make_user_admin(&admin).await?;

		let admin_room = services.admin.get_admin_room().await?;
		let power_levels = services
			.state_accessor
			.get_power_levels(&admin_room)
			.await?;

		assert_eq!(
			membership_of(services, &admin, &admin_room).await,
			Some(MembershipState::Join)
		);
		assert!(power_levels.user_can_invite(&admin), "the admin may invite");

		update_rules(services, |rules| {
			rules.push(RegistrationRule {
				claims: [("hd".to_owned(), "admin-id.example".to_owned())].into(),
				rooms: vec![admin_room.clone().into()],
				external: false,
			});
		})?;

		for sub in ["ada", "adam"] {
			let response = sign_in(base, sub).await?;
			let user_id = signed_in(services, &response).await?;

			assert_eq!(membership_of(services, &user_id, &admin_room).await, None, "{sub}");
			assert!(!services.admin.user_is_admin(&user_id).await, "{sub} is no admin");
		}

		Ok(())
	})
}

/// A rule room an upgrade replaced is skipped, even named by its ID: an old
/// admin room must never be joined so.
#[test]
fn upgraded_room_is_skipped() -> Result {
	run("sso-registration-rules-upgraded", async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let creator = creator(services, base);
		let version = services
			.state
			.get_room_version(&rooms.open)
			.await?;

		creator
			.post(&format!("rooms/{}/upgrade", rooms.open), &json!({ "new_version": version }))
			.await?;

		update_rules(services, |rules| {
			rules.push(RegistrationRule {
				claims: [("hd".to_owned(), "old.example".to_owned())].into(),
				rooms: vec![rooms.open.clone().into(), alias("#class:localhost")],
				external: false,
			});
		})?;

		let response = sign_in(base, "olga").await?;
		let olga = signed_in(services, &response).await?;

		assert_eq!(membership_of(services, &olga, &rooms.open).await, None);
		assert_eq!(
			membership_of(services, &olga, &rooms.class).await,
			Some(MembershipState::Join)
		);

		Ok(())
	})
}

/// A locked member, however powerful, does not invite a new account; the
/// next member that may invite does.
#[test]
fn locked_member_does_not_invite() -> Result {
	run("sso-registration-rules-locked-inviter", async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let creator = creator(services, base);
		let creator_id = creator_id(services)?;
		let aaron_id = register(services, "aaron", AARON_TOKEN).await?;
		let aaron = Client { services, base, token: AARON_TOKEN };

		creator
			.admit(&rooms.staff, &aaron, &aaron_id)
			.await?;
		services
			.users
			.set_locked(&creator_id, &creator_id);

		let response = sign_in(base, "amy").await?;
		let amy = signed_in(services, &response).await?;

		services.users.clear_locked(&creator_id);

		assert_eq!(
			membership_of(services, &amy, &rooms.staff).await,
			Some(MembershipState::Join)
		);
		assert_eq!(
			member_event(&creator, &rooms.staff, &amy, "invite")
				.await?
				.map(|event| event["sender"].clone()),
			Some(json!(aaron_id))
		);

		Ok(())
	})
}

/// An invite the server accepts on the account's behalf as well does no harm:
/// whichever join lands first stands.
#[test]
fn auto_accepted_invite_is_harmless() -> Result {
	let options = ["rooms.invites.auto_accept=true"];

	run_with("sso-registration-rules-auto-accept", &options, async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let response = sign_in(base, "amy").await?;
		let amy = signed_in(services, &response).await?;

		assert_eq!(
			membership_of(services, &amy, &rooms.staff).await,
			Some(MembershipState::Join)
		);
		assert_eq!(membership_of(services, &amy, &rooms.open).await, Some(MembershipState::Join));

		Ok(())
	})
}

/// An account a rule with `external` creates joins that rule's invite-only
/// room, invited by its creator, and no room it is not invited to: not a
/// public room until invited there, and not a room restricted to the members
/// of a room it is in. A second sign-in finds it external still, as the
/// database records it.
#[test]
fn external_account_joins_only_by_invite() -> Result {
	run("sso-registration-rules-external-join", async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let creator = creator(services, base);
		let (guest_id, guest) = sign_in_with(services, base, "guest", GUEST_TOKEN).await?;

		assert_eq!(guest.get("joined_rooms").await?["joined_rooms"], json!([rooms.class]));
		assert_eq!(
			member_event(&creator, &rooms.class, &guest_id, "invite")
				.await?
				.map(|event| event["sender"].clone()),
			Some(json!("@creator:localhost"))
		);

		let join_public = guest.url("join/%23public:localhost");

		forbidden(guest.post_url(&join_public, &json!({})).await?, UNINVITED).await?;
		assert_eq!(membership_of(services, &guest_id, &rooms.public).await, None);

		let response = sign_in(base, "guest").await?;

		assert_eq!(signed_in(services, &response).await?, guest_id);
		assert!(services.users.is_external(&guest_id).await, "the guest is still external");
		services.db["userid_external"]
			.exists(guest_id.as_str())
			.await?;
		forbidden(guest.post_url(&join_public, &json!({})).await?, UNINVITED).await?;

		creator
			.act_on(&rooms.public, "invite", &guest_id)
			.await?;
		guest
			.post("join/%23public:localhost", &json!({}))
			.await?;

		assert_eq!(
			membership_of(services, &guest_id, &rooms.public).await,
			Some(MembershipState::Join)
		);

		let join_club = guest.url("join/%23club:localhost");

		forbidden(guest.post_url(&join_club, &json!({})).await?, UNINVITED).await?;
		assert_eq!(membership_of(services, &guest_id, &rooms.club).await, None);

		Ok(())
	})
}

/// An external account creates no room, by `createRoom` or by upgrading a
/// room it has the power to upgrade, and invites no one, by `/invite` or by
/// a member event sent as state. Its own join sent as state is refused as
/// `/join` refuses it.
#[test]
fn external_account_creates_and_invites_nothing() -> Result {
	run("sso-registration-rules-external-create", async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let creator = creator(services, base);
		let outsider_id = register(services, "outsider", OUTSIDER_TOKEN).await?;
		let (guest_id, guest) = sign_in_with(services, base, "guest", GUEST_TOKEN).await?;
		let state = |room_id: &RoomId, user_id: &UserId| {
			guest.url(&format!("rooms/{room_id}/state/m.room.member/{user_id}"))
		};

		let create = guest
			.post_url(&guest.url("createRoom"), &json!({ "preset": "private_chat" }))
			.await?;

		forbidden(create, NO_ROOMS).await?;

		for invitee in [&creator_id(services)?, &outsider_id] {
			let invite = guest
				.post_url(
					&guest.url(&format!("rooms/{}/invite", rooms.class)),
					&json!({ "user_id": invitee }),
				)
				.await?;

			forbidden(invite, NO_INVITES).await?;
		}

		let invite = guest
			.request(
				Method::PUT,
				&state(&rooms.class, &outsider_id),
				&json!({ "membership": "invite" }),
			)
			.await?;

		forbidden(invite, NO_INVITES).await?;
		assert_eq!(membership_of(services, &outsider_id, &rooms.class).await, None);

		let join = guest
			.request(
				Method::PUT,
				&state(&rooms.public, &guest_id),
				&json!({ "membership": "join" }),
			)
			.await?;

		forbidden(join, UNINVITED).await?;
		assert_eq!(membership_of(services, &guest_id, &rooms.public).await, None);

		let power_levels = format!("rooms/{}/state/m.room.power_levels", rooms.class);
		let mut levels = creator.get(&power_levels).await?;

		// Since room version 12 an upgrade needs more power than 100 by default.
		let upgrader = levels["events"]["m.room.tombstone"].clone();

		levels["users"][guest_id.as_str()] = upgrader;
		creator.put(&power_levels, &levels).await?;

		let version = services
			.state
			.get_room_version(&rooms.class)
			.await?;
		let upgrade = guest
			.post_url(
				&guest.url(&format!("rooms/{}/upgrade", rooms.class)),
				&json!({ "new_version": version }),
			)
			.await?;

		forbidden(upgrade, NO_ROOMS).await
	})
}

/// An external account creates no room alias, while a staff account's same
/// call succeeds.
#[test]
fn external_account_creates_no_room_alias() -> Result {
	run("sso-registration-rules-external-alias", async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let (_, guest) = sign_in_with(services, base, "guest", GUEST_TOKEN).await?;
		let (_, amy) = sign_in_with(services, base, "amy", AMY_TOKEN).await?;
		let target = json!({ "room_id": rooms.class });

		let squat = guest
			.request(Method::PUT, &guest.url("directory/room/%23squat:localhost"), &target)
			.await?;

		forbidden(squat, NO_ALIASES).await?;
		assert!(
			services
				.alias
				.resolve_local_alias(&RoomAliasId::parse("#squat:localhost")?)
				.await
				.is_err(),
			"the alias was not created"
		);

		let staff = amy
			.request(Method::PUT, &amy.url("directory/room/%23staffalias:localhost"), &target)
			.await?;

		assert!(staff.status().is_success(), "staff creates an alias: {}", staff.status());
		Ok(())
	})
}

/// An external account upgrades no room, not even the admin room, whose
/// replacement the server user would create. The guest is not in the admin
/// room, so the 403 with the create text proves the entry check runs before
/// the power check; the old room gets no tombstone.
#[test]
fn external_account_upgrades_no_admin_room() -> Result {
	run("sso-registration-rules-external-upgrade", async |services, base| {
		make_rooms(services, base).await?;
		let (_, guest) = sign_in_with(services, base, "guest", GUEST_TOKEN).await?;
		let admin_room = services.admin.get_admin_room().await?;
		let version = services
			.state
			.get_room_version(&admin_room)
			.await?;

		let upgrade = guest
			.post_url(
				&guest.url(&format!("rooms/{admin_room}/upgrade")),
				&json!({ "new_version": version }),
			)
			.await?;

		forbidden(upgrade, NO_ROOMS).await?;
		assert!(
			services
				.state_accessor
				.room_state_get(&admin_room, &StateEventType::RoomTombstone, "")
				.await
				.is_err(),
			"the admin room has no tombstone"
		);

		Ok(())
	})
}

/// An external account an admin invites into the admin room and that joins it
/// is still no admin, so the admin API refuses it, and granting it admin is
/// refused.
#[test]
fn external_account_is_never_a_server_admin() -> Result {
	let options = ["registration.grant_admin_to_first_user=false"];

	run_with(
		"sso-registration-rules-external-admin-never",
		&options,
		async |services, base| {
			make_rooms(services, base).await?;
			let creator = creator(services, base);
			let (guest_id, guest) = sign_in_with(services, base, "guest", GUEST_TOKEN).await?;

			services
				.admin
				.make_user_admin(&creator_id(services)?)
				.await?;

			let admin_room = services.admin.get_admin_room().await?;

			creator
				.post_url(
					&creator.url(&format!("rooms/{admin_room}/invite")),
					&json!({ "user_id": guest_id }),
				)
				.await?;
			guest
				.post(&format!("join/{admin_room}"), &json!({}))
				.await?;

			assert_eq!(
				membership_of(services, &guest_id, &admin_room).await,
				Some(MembershipState::Join)
			);
			assert!(!services.admin.user_is_admin(&guest_id).await, "the guest is no admin");

			let reply = guest
				.admin_api(Method::GET, &format!("v1/users/{guest_id}/admin"), &json!({}), 403)
				.await?;

			assert!(reply.contains("Only server administrators"), "{reply}");

			let grant = services
				.admin
				.make_user_admin(&guest_id)
				.await
				.expect_err("the grant is refused");

			assert!(grant.to_string().contains(NO_ADMIN), "{grant}");

			Ok(())
		},
	)
}

/// A joined external account sends no third-party invite, even in a room
/// whose power levels would let it.
#[test]
fn external_account_sends_no_third_party_invite() -> Result {
	run("sso-registration-rules-external-third-party", async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let creator = creator(services, base);
		let (guest_id, guest) = sign_in_with(services, base, "guest", GUEST_TOKEN).await?;

		assert_eq!(
			membership_of(services, &guest_id, &rooms.class).await,
			Some(MembershipState::Join)
		);

		let power_levels = format!("rooms/{}/state/m.room.power_levels", rooms.class);
		let mut levels = creator.get(&power_levels).await?;

		levels["invite"] = json!(0);
		levels["events"]["m.room.third_party_invite"] = json!(0);
		creator.put(&power_levels, &levels).await?;

		let invite = guest
			.request(
				Method::PUT,
				&guest
					.url(&format!("rooms/{}/state/m.room.third_party_invite/token", rooms.class)),
				&json!({
					"display_name": "Alice",
					"key_validity_url": "https://school.example/validity",
					"public_key": "dGVzdA",
				}),
			)
			.await?;

		forbidden(invite, NO_INVITES).await
	})
}

/// An external account's directory search finds only people it shares a
/// room with, although the server shows every local user to others.
#[test]
fn external_account_finds_only_people_it_shares_a_room_with() -> Result {
	let options = ["directory.users.show_all_local=true"];

	run_with("sso-registration-rules-external-directory", &options, async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let outsider_id = register(services, "outsider", OUTSIDER_TOKEN).await?;
		let outsider = Client { services, base, token: OUTSIDER_TOKEN };

		outsider.act(&rooms.open, "join").await?;

		let (_, guest) = sign_in_with(services, base, "guest", GUEST_TOKEN).await?;
		let (_, amy) = sign_in_with(services, base, "amy", AMY_TOKEN).await?;

		assert_eq!(found(&guest, "creator").await?, [creator_id(services)?]);
		assert_eq!(found(&guest, "outsider").await?, Vec::<OwnedUserId>::new());
		assert_eq!(found(&amy, "outsider").await?, [outsider_id]);

		Ok(())
	})
}

/// An account meeting a rule with `external` and one without is external,
/// and each rule room invites it first, even a public room that would admit
/// anyone else.
#[test]
fn external_rule_invites_even_to_public_rooms() -> Result {
	run("sso-registration-rules-external-public", async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let creator = creator(services, base);

		update_rules(services, |rules| {
			rules.push(RegistrationRule {
				claims: [("hd".to_owned(), "visit.example".to_owned())].into(),
				rooms: vec![alias("#open:localhost")],
				external: false,
			});
			rules.push(RegistrationRule {
				claims: [("email".to_owned(), "vera@visit.example".to_owned())].into(),
				rooms: vec![alias("#public:localhost")],
				external: true,
			});
		})?;

		let response = sign_in(base, "vera").await?;
		let vera = signed_in(services, &response).await?;

		assert!(services.users.is_external(&vera).await, "vera is external");

		for room_id in [&rooms.open, &rooms.public] {
			assert_eq!(
				membership_of(services, &vera, room_id).await,
				Some(MembershipState::Join),
				"{room_id}"
			);
			assert_eq!(
				member_event(&creator, room_id, &vera, "invite")
					.await?
					.map(|event| event["sender"].clone()),
				Some(json!("@creator:localhost")),
				"{room_id}"
			);
		}

		Ok(())
	})
}

/// An account a rule without `external` creates joins a public room it is
/// not invited to and creates rooms, as any account does.
#[test]
fn staff_account_is_unchanged() -> Result {
	run("sso-registration-rules-staff", async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let (amy_id, amy) = sign_in_with(services, base, "amy", AMY_TOKEN).await?;

		assert!(!services.users.is_external(&amy_id).await, "amy is not external");

		amy.post("join/%23public:localhost", &json!({}))
			.await?;

		assert_eq!(
			membership_of(services, &amy_id, &rooms.public).await,
			Some(MembershipState::Join)
		);

		amy.create_room(&json!({ "preset": "private_chat" }))
			.await?;

		Ok(())
	})
}

/// A new external account joins none of `auto_join_rooms` and, although it is
/// the server's first account to sign in, is not made its admin; the next
/// account of a rule without `external` joins them.
#[test]
fn external_account_skips_registration_extras() -> Result {
	let options = [r##"registration.auto_join_rooms=["#public:localhost"]"##];

	run_with("sso-registration-rules-external-extras", &options, async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let (guest_id, _) = sign_in_with(services, base, "guest", GUEST_TOKEN).await?;
		let (amy_id, _) = sign_in_with(services, base, "amy", AMY_TOKEN).await?;

		assert_eq!(membership_of(services, &guest_id, &rooms.public).await, None);
		assert!(!services.admin.user_is_admin(&guest_id).await, "the guest is no admin");
		assert!(services.admin.user_is_admin(&amy_id).await, "the grant went to amy instead");
		assert_eq!(
			membership_of(services, &amy_id, &rooms.public).await,
			Some(MembershipState::Join)
		);

		Ok(())
	})
}

/// The server's own flows that send an external account's member events
/// still work: accepting an invite on its behalf, a display name change in
/// the rooms it is in, and leaving.
#[test]
fn external_account_keeps_its_own_membership_flows() -> Result {
	let options = ["rooms.invites.auto_accept=true"];

	run_with("sso-registration-rules-external-flows", &options, async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let creator = creator(services, base);
		let (guest_id, guest) = sign_in_with(services, base, "guest", GUEST_TOKEN).await?;

		creator
			.act_on(&rooms.public, "invite", &guest_id)
			.await?;

		let joined = poll_until(Duration::from_secs(10), async || {
			membership_of(services, &guest_id, &rooms.public).await == Some(MembershipState::Join)
		})
		.await;

		assert!(joined, "the invite to the public room is accepted");

		guest
			.put(&format!("profile/{guest_id}/displayname"), &json!({ "displayname": "Guest" }))
			.await?;

		let renamed = poll_until(Duration::from_secs(10), async || {
			services
				.state_accessor
				.get_member(&rooms.class, &guest_id)
				.await
				.is_ok_and(|member| member.displayname.as_deref() == Some("Guest"))
		})
		.await;

		assert!(renamed, "the member event in the class carries the new name");

		guest.act(&rooms.class, "leave").await?;

		assert_eq!(
			membership_of(services, &guest_id, &rooms.class).await,
			Some(MembershipState::Leave)
		);

		Ok(())
	})
}

/// An admin joins an external account to a room by inviting it first, as
/// the admin join API does, even where the join rule admits anyone; the
/// `force-join-room` command, which joins without an invite, is refused,
/// while it still joins an account that is not external.
#[test]
fn admins_join_external_accounts_by_invite() -> Result {
	let options = ["registration.grant_admin_to_first_user=false"];

	run_with("sso-registration-rules-external-admin", &options, async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let creator = creator(services, base);
		let (guest_id, _) = sign_in_with(services, base, "guest", GUEST_TOKEN).await?;
		let (amy_id, _) = sign_in_with(services, base, "amy", AMY_TOKEN).await?;

		services
			.admin
			.make_user_admin(&creator_id(services)?)
			.await?;

		admin::refused(
			services,
			&format!("users force-join-room {guest_id} {}", rooms.public),
			UNINVITED,
		)
		.await?;
		admin::accepted(services, &format!("users force-join-room {amy_id} {}", rooms.public))
			.await?;

		let path = format!("v1/join/{}", rooms.open);

		creator
			.admin_api(Method::POST, &path, &json!({ "user_id": guest_id }), 200)
			.await?;

		assert_eq!(
			membership_of(services, &guest_id, &rooms.open).await,
			Some(MembershipState::Join)
		);
		assert_eq!(
			member_event(&creator, &rooms.open, &guest_id, "invite")
				.await?
				.map(|event| event["sender"].clone()),
			Some(json!("@creator:localhost"))
		);

		Ok(())
	})
}

/// Boot a server whose provider has the rules, with the provider stand-in
/// serving beside it, and run one exercise.
fn run<Exercise>(name: &str, exercise: Exercise) -> Result
where
	Exercise: AsyncFnOnce(&Services, &str) -> Result,
{
	run_with(name, &[], exercise)
}

/// `run`, with further server options.
fn run_with<Exercise>(name: &str, extra: &[&str], exercise: Exercise) -> Result
where
	Exercise: AsyncFnOnce(&Services, &str) -> Result,
{
	let provider = TcpListener::bind(("127.0.0.1", 0))?;
	let issuer = format!("http://{}", provider.local_addr()?);

	provider.set_nonblocking(true)?;

	let options = provider_options(&issuer)
		.into_iter()
		.chain(extra.iter().copied().map(str::to_owned))
		.collect::<Vec<_>>();

	boot(name, options.iter().map(String::as_str), async move |services, base| {
		let serve = serve_provider(provider, issuer);

		select(pin!(serve), pin!(exercise(services, base)))
			.await
			.factor_first()
			.0
	})
}

/// The provider's settings. Every boot leaves the process-wide tracing
/// subscriber alone, so the tests of this binary boot their servers side by
/// side.
fn provider_options(issuer: &str) -> Vec<String> {
	let option = |key: &str, value: &str| format!("identity_provider.{IDP}.{key}={value}");

	vec![
		option("client_id", &format!("\"{IDP}\"")),
		option("client_secret", "\"test-secret\""),
		option("brand", "\"test\""),
		option("issuer_url", &format!("\"{issuer}\"")),
		option("userid_claims", "[\"email\"]"),
		option("registration_rules", &RULES.replace(['\n', '\t'], "")),
		"oidc_registration_allowed_redirect_hosts=[\"client.example\"]".to_owned(),
		"log_enable=false".to_owned(),
	]
}

/// Make `#staff` and `#class`, both invite-only, the public `#open` and
/// `#public`, and `#club`, restricted to the members of `#public`, as one
/// local account.
async fn make_rooms(services: &Services, base: &str) -> Result<Rooms> {
	register(services, "creator", CREATOR_TOKEN).await?;

	let creator = creator(services, base);
	let room = async |preset: &str, alias: &str| {
		creator
			.create_room(&json!({ "preset": preset, "room_alias_name": alias }))
			.await
	};

	let public = room("public_chat", "public").await?;
	let club = creator
		.create_room(&json!({
			"preset": "private_chat",
			"room_alias_name": "club",
			"initial_state": [{
				"type": "m.room.join_rules",
				"state_key": "",
				"content": {
					"join_rule": "restricted",
					"allow": [{ "type": "m.room_membership", "room_id": public }],
				},
			}],
		}))
		.await?;

	Ok(Rooms {
		staff: room("private_chat", "staff").await?,
		open: room("public_chat", "open").await?,
		class: room("private_chat", "class").await?,
		public,
		club,
	})
}

fn creator<'a>(services: &'a Services, base: &'a str) -> Client<'a> {
	Client { services, base, token: CREATOR_TOKEN }
}

fn creator_id(services: &Services) -> Result<OwnedUserId> { user_id(services, "creator") }

/// Sign in as `sub` and give the account a device holding `token`, for the
/// client API.
async fn sign_in_with<'a>(
	services: &'a Services,
	base: &'a str,
	sub: &str,
	token: &'a str,
) -> Result<(OwnedUserId, Client<'a>)> {
	let response = sign_in(base, sub).await?;
	let user_id = signed_in(services, &response).await?;

	services
		.users
		.create_device(&user_id, None, (Some(token), None), None, None, None)
		.await?;

	Ok((user_id, Client { services, base, token }))
}

/// The accounts a user directory search for `term` finds.
async fn found(client: &Client<'_>, term: &str) -> Result<Vec<OwnedUserId>> {
	let response = client
		.post("user_directory/search", &json!({ "search_term": term }))
		.await?;

	response["results"]
		.as_array()
		.into_iter()
		.flatten()
		.map(|user| {
			OwnedUserId::try_from(user["user_id"].as_str().unwrap_or_default())
				.map_err(Into::into)
		})
		.collect()
}

/// Expect a refusal with `M_FORBIDDEN` and the text.
async fn forbidden(response: Response, text: &str) -> Result {
	assert_eq!(response.status(), StatusCode::FORBIDDEN, "refused: {text}");

	let error: Value = response.json().await?;

	assert_eq!(error["errcode"], "M_FORBIDDEN");
	assert_eq!(error["error"], format!("M_FORBIDDEN: {text}"));

	Ok(())
}

async fn membership_of(
	services: &Services,
	user_id: &UserId,
	room_id: &RoomId,
) -> Option<MembershipState> {
	services
		.state_cache
		.user_membership(user_id, room_id)
		.await
}

/// The user's membership event of the kind, as the room's creator reads it in
/// the room's history.
async fn member_event(
	creator: &Client<'_>,
	room_id: &RoomId,
	user_id: &UserId,
	membership: &str,
) -> Result<Option<Value>> {
	let messages = creator
		.get(&format!("rooms/{room_id}/messages?dir=b&limit=100"))
		.await?;

	Ok(messages["chunk"]
		.as_array()
		.into_iter()
		.flatten()
		.find(|event| {
			event["type"] == "m.room.member"
				&& event["state_key"] == user_id.as_str()
				&& event["content"]["membership"] == membership
		})
		.cloned())
}

/// Sign in as `sub` and expect the refusal, linking the identity to nothing
/// and leaving the account its address names absent.
async fn refused(services: &Services, base: &str, sub: &str) -> Result {
	forbidden(sign_in(base, sub).await?, REFUSAL).await?;

	assert!(unlinked(services, sub).await?, "{sub} is linked to no account");

	let creator = creator(services, base);

	for user_id in [user_id(services, sub)?, fallback(services, sub).await?] {
		assert!(!services.users.exists(&user_id).await, "{user_id} has no account");

		let profile = creator
			.get_url(&creator.url(&format!("profile/{user_id}")))
			.await?;

		assert_eq!(profile.status(), StatusCode::NOT_FOUND, "{user_id} has no profile");
	}

	Ok(())
}

fn current_rules(services: &Services) -> Result<Vec<RegistrationRule>> {
	Ok(services
		.oauth
		.providers
		.find_config(IDP)?
		.registration_rules
		.clone())
}

/// Change the provider's rules through a config reload.
fn update_rules<Update>(services: &Services, update: Update) -> Result
where
	Update: FnOnce(&mut Vec<RegistrationRule>),
{
	let mut config = (*services.server.config.clone()).clone();
	let provider = config
		.identity_provider
		.values_mut()
		.find(|provider| provider.id() == IDP)
		.ok_or_else(|| err!("the provider is configured"))?;

	update(&mut provider.registration_rules);
	reload(&services.config, &config)?;
	services.server.config.update(config)?;

	Ok(())
}

fn alias(alias: &str) -> OwnedRoomOrAliasId { alias.try_into().expect("a room alias") }

fn user_id(services: &Services, localpart: &str) -> Result<OwnedUserId> {
	Ok(UserId::parse_with_server_name(localpart, services.globals.server_name())?)
}

/// Serve the identity provider endpoints until the test is done.
///
/// A callback's code is the identity's `sub`, which the token endpoint returns
/// as the access token and userinfo answers with that identity's claims.
async fn serve_provider(listener: TcpListener, issuer: String) -> Result {
	let app = Router::new()
		.route("/.well-known/openid-configuration", get(discover))
		.route("/token", post(token))
		.route("/userinfo", get(userinfo))
		.with_state(issuer);

	from_tcp(listener)?
		.serve(app.into_make_service())
		.await?;

	Err!("the identity provider stand-in stopped serving")
}

async fn discover(State(issuer): State<String>) -> Json<Value> {
	Json(json!({
		"issuer": issuer,
		"authorization_endpoint": format!("{issuer}/authorize"),
		"token_endpoint": format!("{issuer}/token"),
		"userinfo_endpoint": format!("{issuer}/userinfo"),
	}))
}

async fn token(Form(form): Form<TokenForm>) -> Json<Value> {
	Json(json!({
		"access_token": form["code"],
		"token_type": "Bearer",
	}))
}

async fn userinfo(headers: HeaderMap) -> Json<Value> {
	let sub = headers[AUTHORIZATION]
		.to_str()
		.expect("bearer token")
		.trim_start_matches("Bearer ");

	Json(claims(sub))
}

/// The claims the provider sends for each of the test's identities.
fn claims(sub: &str) -> Value {
	match sub {
		| "amy" => json!({ "sub": sub, "email": "amy@example.com", "hd": "example.com" }),
		| "guest" => json!({
			"sub": sub,
			"email": "GUEST@school.example",
			"email_verified": true,
		}),
		| "other" => json!({
			"sub": sub,
			"email": "other@school.example",
			"email_verified": true,
		}),
		| "guest2" => json!({
			"sub": sub,
			"email": "guest2@school.example",
			"email_verified": false,
		}),
		| "far" => json!({ "sub": sub, "email": "far@far.example", "hd": "far.example" }),
		| "lena" => json!({ "sub": sub, "email": "lena@late.example", "hd": "late.example" }),
		| "ada" => json!({ "sub": sub, "email": "ada@admin.example", "hd": "admin.example" }),
		| "adam" => json!({
			"sub": sub,
			"email": "adam@admin-id.example",
			"hd": "admin-id.example",
		}),
		| "olga" => json!({ "sub": sub, "email": "olga@old.example", "hd": "old.example" }),
		| "vera" => json!({
			"sub": sub,
			"email": "vera@visit.example",
			"email_verified": true,
			"hd": "visit.example",
		}),
		| _ => json!({ "sub": sub, "email": format!("{sub}@elsewhere.example") }),
	}
}

/// Whether the identity `sub` is linked to no account.
async fn unlinked(services: &Services, sub: &str) -> Result<bool> {
	let provider = services.oauth.providers.get(IDP).await?;
	let linked = services
		.oauth
		.sessions
		.get_by_unique_id(&unique_id_sub((&provider, sub))?)
		.await;

	Ok(linked.is_not_found())
}

/// The fallback account of the identity `sub`.
async fn fallback(services: &Services, sub: &str) -> Result<OwnedUserId> {
	let provider = services.oauth.providers.get(IDP).await?;
	let localpart = fallback_localpart(&unique_id_sub((&provider, sub))?);

	user_id(services, &localpart)
}

/// Sign in as `sub`, returning the callback's unfollowed response.
async fn sign_in(base: &str, sub: &str) -> Result<Response> {
	let client = reqwest::Client::builder()
		.redirect(Policy::none())
		.timeout(Duration::from_secs(10))
		.build()?;

	let url = format!("{base}/_matrix/client/v3/login/sso/redirect/{IDP}");
	let redirect = client
		.get(url)
		.query(&[("redirectUrl", CLIENT)])
		.send()
		.await?;

	assert_eq!(redirect.status(), StatusCode::FOUND);

	let state = parameter(&location(&redirect)?, "state");
	let url = format!("{base}/_matrix/client/unstable/login/sso/callback/{IDP}");

	client
		.get(url)
		.query(&[("code", sub), ("state", &state)])
		.header(COOKIE, cookie(&redirect))
		.send()
		.await
		.map_err(Into::into)
}

/// Resolve the account named by a finished sign-in's login token.
///
/// Fails when the callback redirected anywhere but the client.
async fn signed_in(services: &Services, response: &Response) -> Result<OwnedUserId> {
	let destination = location(response)?;

	if !destination.as_str().starts_with(CLIENT) {
		return Err!("sign-in finished at {destination} rather than the client");
	}

	services
		.users
		.find_from_login_token(&parameter(&destination, "loginToken"))
		.await
}

/// The grant cookie a response sets, as a request sends it back.
fn cookie(response: &Response) -> &str {
	response.headers()[SET_COOKIE]
		.to_str()
		.expect("cookie header text")
		.split(';')
		.next()
		.expect("cookie pair")
}

fn location(response: &Response) -> Result<Url> {
	response
		.headers()
		.get(LOCATION)
		.ok_or_else(|| err!("{} response has no location", response.status()))?
		.to_str()
		.expect("location header text")
		.parse()
		.map_err(Into::into)
}

fn parameter(url: &Url, name: &str) -> String {
	url.query_pairs()
		.find(|(key, _)| key == name)
		.expect("query parameter")
		.1
		.into_owned()
}
