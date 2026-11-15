//! `external_forbidden_rooms`: rooms an external account may never be in,
//! whoever invites it and however it joins.

use std::time::Duration;

use reqwest::Method;
use serde_json::json;
use tuwunel_core::{Result, ruma::events::room::member::MembershipState};

use super::{
	AMY_TOKEN, GUEST_TOKEN, OUTSIDER_TOKEN, creator, creator_id, forbid, forbidden, make_rooms,
	membership::room_path, membership_of, poll_until, register, run, run_with, sign_in_with,
};

/// The refusal of an external account's invite to or join of a listed room.
pub(super) const KEPT_OUT: &str = "External accounts may not be in this room.";

/// An external account is invited to no listed room, whether the setting
/// names it by alias or by ID, while it is invited to a room not listed. A
/// staff account is unaffected: it is invited to a listed room, and a new one
/// joins its rule's listed room. Emptied, the setting refuses nothing.
#[test]
fn external_account_is_not_invited_to_listed_rooms() -> Result {
	run("sso-forbidden-rooms-invite", async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let creator = creator(services, base);
		let (guest_id, _) = sign_in_with(services, base, "guest", GUEST_TOKEN).await?;
		let outsider_id = register(services, "outsider", OUTSIDER_TOKEN).await?;

		forbid(services, &["#staff:localhost", rooms.public.as_str()])?;

		for room_id in [&rooms.staff, &rooms.public] {
			let invite = creator
				.post_url(
					&creator.url(&room_path(room_id, "invite")),
					&json!({ "user_id": guest_id }),
				)
				.await?;

			forbidden(invite, KEPT_OUT).await?;
			assert_eq!(membership_of(services, &guest_id, room_id).await, None, "{room_id}");
		}

		// A member event sent as state meets the same refusal.
		let state_invite = creator
			.request(
				Method::PUT,
				&creator
					.url(&room_path(&rooms.staff, &format!("state/m.room.member/{guest_id}"))),
				&json!({ "membership": "invite" }),
			)
			.await?;

		forbidden(state_invite, KEPT_OUT).await?;
		assert_eq!(membership_of(services, &guest_id, &rooms.staff).await, None);

		creator
			.act_on(&rooms.open, "invite", &guest_id)
			.await?;
		creator
			.act_on(&rooms.staff, "invite", &outsider_id)
			.await?;

		let (amy_id, _) = sign_in_with(services, base, "amy", AMY_TOKEN).await?;

		assert_eq!(
			membership_of(services, &amy_id, &rooms.staff).await,
			Some(MembershipState::Join)
		);

		forbid(services, &[])?;
		creator
			.act_on(&rooms.staff, "invite", &guest_id)
			.await?;

		Ok(())
	})
}

/// An external account joins no listed room: not with an invite it had before
/// the room was listed, and not through the admin join API, whose invite is
/// refused, admin or not. A listed room it is already in keeps it, and its
/// profile change still reaches that room.
#[test]
fn external_account_joins_no_listed_room() -> Result {
	let options = ["registration.grant_admin_to_first_user=false"];

	run_with("sso-forbidden-rooms-join", &options, async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let creator = creator(services, base);
		let (guest_id, guest) = sign_in_with(services, base, "guest", GUEST_TOKEN).await?;

		services
			.admin
			.make_user_admin(&creator_id(services)?)
			.await?;
		creator
			.act_on(&rooms.open, "invite", &guest_id)
			.await?;

		// The class by ID: the guest's external rule names it by alias, which the
		// config check refuses only when listed the same way.
		forbid(services, &["#open:localhost", rooms.class.as_str(), "#staff:localhost"])?;

		let join = guest
			.post_url(&guest.url(&room_path(&rooms.open, "join")), &json!({}))
			.await?;

		forbidden(join, KEPT_OUT).await?;
		assert_eq!(
			membership_of(services, &guest_id, &rooms.open).await,
			Some(MembershipState::Invite)
		);

		let refusal = creator
			.admin_api(
				Method::POST,
				&format!("v1/join/{}", rooms.staff),
				&json!({ "user_id": guest_id }),
				403,
			)
			.await?;

		assert!(refusal.contains(KEPT_OUT), "{refusal}");
		assert_eq!(membership_of(services, &guest_id, &rooms.staff).await, None);
		assert_eq!(
			membership_of(services, &guest_id, &rooms.class).await,
			Some(MembershipState::Join)
		);

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

		Ok(())
	})
}

/// `createRoom` registers its alias only after the room's events, so a request
/// for a listed alias that names an external account is refused before the
/// room exists, whether in `invite` or as a member event in `initial_state`.
/// With staff alone the room is created, and its alias then protects it.
#[test]
fn room_with_a_listed_alias_is_not_created_with_an_external_account() -> Result {
	run("sso-forbidden-rooms-create", async |services, base| {
		make_rooms(services, base).await?;
		let creator = creator(services, base);
		let (guest_id, _) = sign_in_with(services, base, "guest", GUEST_TOKEN).await?;
		let outsider_id = register(services, "outsider", OUTSIDER_TOKEN).await?;

		forbid(services, &["#later:localhost"])?;

		let invited = json!({
			"preset": "private_chat",
			"room_alias_name": "later",
			"invite": [outsider_id, guest_id],
		});
		let as_state = json!({
			"preset": "private_chat",
			"room_alias_name": "later",
			"initial_state": [
				{
					"type": "m.room.canonical_alias",
					"state_key": "",
					"content": { "alias": null },
				},
				{
					"type": "m.room.member",
					"state_key": guest_id,
					"content": { "membership": "invite" },
				},
			],
		});

		for body in [invited, as_state] {
			let created = creator
				.post_url(&creator.url("createRoom"), &body)
				.await?;

			forbidden(created, KEPT_OUT).await?;
		}

		// The alias is still free, so the refused requests made no room.
		let staff_only = json!({
			"preset": "private_chat",
			"room_alias_name": "later",
			"invite": [outsider_id],
		});
		let room_id = creator.create_room(&staff_only).await?;

		assert!(
			membership_of(services, &outsider_id, &room_id)
				.await
				.is_some(),
			"the staff account is invited"
		);

		let invite = creator
			.post_url(
				&creator.url(&room_path(&room_id, "invite")),
				&json!({ "user_id": guest_id }),
			)
			.await?;

		forbidden(invite, KEPT_OUT).await?;

		// An alias the setting does not list is created with the account.
		let unlisted = json!({
			"preset": "private_chat",
			"room_alias_name": "elsewhere",
			"invite": [guest_id],
		});
		let room_id = creator.create_room(&unlisted).await?;

		assert!(
			membership_of(services, &guest_id, &room_id)
				.await
				.is_some()
		);

		Ok(())
	})
}
