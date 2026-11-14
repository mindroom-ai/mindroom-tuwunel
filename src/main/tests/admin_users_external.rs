#![cfg(test)]

use std::{pin::pin, time::Duration};

use futures::future::{Either, select};
use reqwest::Method;
use serde_json::{Value, json};
use tokio::time::sleep;
use tuwunel_core::{
	Err, Result, err,
	ruma::{
		UserId,
		events::room::member::{MembershipState, RoomMemberEventContent},
	},
};
use tuwunel_matrix::pdu::PduBuilder;
use tuwunel_service::{Services, users::DeactivationReason};

use self::{
	client::{Client, register},
	fixture::boot,
};

#[expect(dead_code)] // Only the user admin API is requested through the client harness.
mod client;
mod fixture;

#[path = "client/admin_api.rs"]
mod admin_api;

const ADMIN_TOKEN: &str = "admin-users-external-admin-access-token";
const OTHER_TOKEN: &str = "admin-users-external-other-admin-access-token";
const MEMBER_TOKEN: &str = "admin-users-external-member-access-token";
const EXTERNAL_TOKEN: &str = "admin-users-external-external-access-token";

/// Every test boots a server, so none takes the process-wide log subscriber.
const OPTIONS: [&str; 3] = [
	"log_enable=false",
	"admin.room.create=true",
	"registration.grant_admin_to_first_user=false",
];

/// How long a deactivation must stay waiting on the locked admins room.
const LOCK_WAIT: Duration = Duration::from_millis(500);

/// The user admin API marks an external account with the `user_type`
/// "external", in both lists and in the account's details, and gives any other
/// account none. `not_user_type` leaves the accounts of a type out of a list
/// and out of its total.
#[test]
fn user_admin_api_marks_external_accounts() -> Result {
	boot("admin-users-external", OPTIONS, marks_external_accounts)
}

async fn marks_external_accounts(services: &Services, base: &str) -> Result {
	let admin_id = register(services, "admin", ADMIN_TOKEN).await?;
	let member_id = register(services, "member", MEMBER_TOKEN).await?;
	let external_id = register(services, "external", EXTERNAL_TOKEN).await?;

	services.admin.make_user_admin(&admin_id).await?;
	services.users.set_external(&external_id);

	let admin = Client { services, base, token: ADMIN_TOKEN };

	for version in ["v2", "v3"] {
		let list = get(&admin, &format!("{version}/users")).await?;

		assert_eq!(listed(&list, &external_id)?["user_type"], "external", "{version}");
		assert_eq!(listed(&list, &member_id)?["user_type"], Value::Null, "{version}");
		assert_eq!(listed(&list, &admin_id)?["user_type"], Value::Null, "{version}");

		let total = list["total"]
			.as_u64()
			.ok_or_else(|| err!("{version}: no total in {list}"))?;

		let untyped = get(&admin, &format!("{version}/users?not_user_type=external")).await?;

		assert!(listed(&untyped, &external_id).is_err(), "{version}: {untyped}");
		assert_eq!(listed(&untyped, &member_id)?["user_type"], Value::Null, "{version}");
		assert_eq!(untyped["total"], total.saturating_sub(1), "{version}");

		// As in Synapse, an empty type names the accounts that have none.
		let typed = get(&admin, &format!("{version}/users?not_user_type=")).await?;

		assert_eq!(listed(&typed, &external_id)?["user_type"], "external", "{version}");
		assert!(listed(&typed, &member_id).is_err(), "{version}: {typed}");
		assert_eq!(typed["total"], 1, "{version}");

		let neither = format!("{version}/users?not_user_type=&not_user_type=external");
		let neither = get(&admin, &neither).await?;

		assert_eq!(neither["total"], 0, "{version}: {neither}");
	}

	let details = get(&admin, &format!("v2/users/{external_id}")).await?;

	assert_eq!(details["user_type"], "external");

	let details = get(&admin, &format!("v2/users/{member_id}")).await?;

	assert_eq!(details["user_type"], Value::Null);

	Ok(())
}

/// `POST /_synapse/admin/v1/deactivate/{user_id}` refuses another active server
/// admin and the admin who asks, as locking and suspending do, after the
/// answers it already gave, and still deactivates any other account. An admin
/// who was deactivated without leaving the admins room is no active admin, and
/// is erased as any deactivated account is.
#[test]
fn deactivate_refuses_admins_and_the_caller() -> Result {
	boot("admin-users-deactivate-refusal", OPTIONS, refuses_admins_and_the_caller)
}

async fn refuses_admins_and_the_caller(services: &Services, base: &str) -> Result {
	let admin_id = register(services, "admin", ADMIN_TOKEN).await?;
	let other_id = register(services, "other", OTHER_TOKEN).await?;
	let member_id = register(services, "member", MEMBER_TOKEN).await?;

	for user_id in [&admin_id, &other_id] {
		services.admin.make_user_admin(user_id).await?;
	}

	let admin = Client { services, base, token: ADMIN_TOKEN };

	for user_id in [&other_id, &admin_id] {
		let refusal = deactivate(&admin, user_id, &json!({}), 403).await?;

		assert_eq!(refusal["errcode"], "M_FORBIDDEN", "{user_id}");
		assert!(!services.users.is_deactivated(user_id).await?, "{user_id} stays active");
	}

	// The server user and an unknown account keep their own answers, though the
	// server user counts as an admin.
	let unknown = UserId::parse_with_server_name("unknown", services.globals.server_name())?;

	deactivate(&admin, &services.globals.server_user, &json!({}), 400).await?;
	deactivate(&admin, &unknown, &json!({}), 404).await?;

	deactivate(&admin, &member_id, &json!({}), 200).await?;

	assert!(services.users.is_deactivated(&member_id).await?, "a member is deactivated");

	// Deactivated without leaving its rooms, the other admin stays in the admins
	// room, where nothing is left to protect.
	services
		.users
		.deactivate_account(&other_id, DeactivationReason::Admin)
		.await?;

	assert!(services.admin.user_is_admin(&other_id).await, "still in the admins room");

	deactivate(&admin, &other_id, &json!({"erase": true}), 200).await?;

	assert!(services.users.is_erased(&other_id).await, "a deactivated admin is erased");

	Ok(())
}

/// The refusal holds against a promotion made while the request runs. The
/// deactivation looks at the account under the admins room's lock, which a
/// promotion holds, so it never takes an account that became an admin after an
/// earlier look.
#[test]
fn deactivate_refuses_an_admin_promoted_meanwhile() -> Result {
	boot("admin-users-deactivate-promotion", OPTIONS, refuses_an_admin_promoted_meanwhile)
}

async fn refuses_an_admin_promoted_meanwhile(services: &Services, base: &str) -> Result {
	let admin_id = register(services, "admin", ADMIN_TOKEN).await?;
	let member_id = register(services, "member", MEMBER_TOKEN).await?;

	services.admin.make_user_admin(&admin_id).await?;

	let admin = Client { services, base, token: ADMIN_TOKEN };
	let member = Client { services, base, token: MEMBER_TOKEN };
	let admin_room = services.admin.get_admin_room().await?;

	// The request finds a member and waits for the admins room, held here as a
	// promotion holds it.
	let admin_lock = services.state.mutex.lock(&admin_room).await;
	let body = json!({});
	let request = pin!(deactivate(&admin, &member_id, &body, 403));
	let waiting = pin!(sleep(LOCK_WAIT));

	let request = match select(request, waiting).await {
		| Either::Right(((), request)) => request,
		| Either::Left((answer, _)) => {
			return Err!("the deactivation did not wait for the admins room: {answer:?}");
		},
	};

	// The promotion joins the member to the admins room, then lets go of it.
	for (membership, sender) in [
		(MembershipState::Invite, &*services.globals.server_user),
		(MembershipState::Join, &*member_id),
	] {
		let event =
			PduBuilder::state(member_id.as_str(), &RoomMemberEventContent::new(membership));

		services
			.timeline
			.build_and_append_pdu(event, sender, &admin_room, &admin_lock)
			.await?;
	}

	drop(admin_lock);

	let refusal = request.await?;

	assert_eq!(refusal["errcode"], "M_FORBIDDEN");
	assert!(services.admin.user_is_admin(&member_id).await, "the promotion went through");
	assert!(!services.users.is_deactivated(&member_id).await?, "the password stays");

	let whoami = member
		.request(Method::GET, &member.url("account/whoami"), &json!({}))
		.await?;

	assert_eq!(whoami.status().as_u16(), 200, "the device and its token stay");

	Ok(())
}

/// The JSON one user admin API path answers a `GET` with.
async fn get(admin: &Client<'_>, path: &str) -> Result<Value> {
	let response = admin
		.admin_api(Method::GET, path, &json!({}), 200)
		.await?;

	Ok(serde_json::from_str(&response)?)
}

/// The entry of an account in a list of users.
fn listed<'a>(list: &'a Value, user_id: &UserId) -> Result<&'a Value> {
	list["users"]
		.as_array()
		.into_iter()
		.flatten()
		.find(|user| user["name"] == user_id.as_str())
		.ok_or_else(|| err!("{user_id} is not listed: {list}"))
}

/// Ask to deactivate an account, and return what the server answers with the
/// `expected` status.
async fn deactivate(
	admin: &Client<'_>,
	user_id: &UserId,
	body: &Value,
	expected: u16,
) -> Result<Value> {
	let response = admin
		.admin_api(Method::POST, &format!("v1/deactivate/{user_id}"), body, expected)
		.await?;

	Ok(serde_json::from_str(&response)?)
}
