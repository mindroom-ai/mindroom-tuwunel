#![cfg(test)]

use reqwest::Method;
use serde_json::{Value, json};
use tuwunel_core::{Result, err, ruma::UserId};
use tuwunel_service::Services;

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

/// Both tests boot a server, so neither takes the process-wide log subscriber.
const OPTIONS: [&str; 3] = [
	"log_enable=false",
	"admin.room.create=true",
	"registration.grant_admin_to_first_user=false",
];

/// The user admin API marks an external account with the `user_type`
/// "external", in both lists and in the account's details, and gives any other
/// account none.
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
	}

	let details = get(&admin, &format!("v2/users/{external_id}")).await?;

	assert_eq!(details["user_type"], "external");

	let details = get(&admin, &format!("v2/users/{member_id}")).await?;

	assert_eq!(details["user_type"], Value::Null);

	Ok(())
}

/// `POST /_synapse/admin/v1/deactivate/{user_id}` refuses another server admin
/// and the admin who asks, as locking and suspending do, and still deactivates
/// any other account.
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
		let refusal = deactivate(&admin, user_id, 403).await?;

		assert_eq!(refusal["errcode"], "M_FORBIDDEN", "{user_id}");
		assert!(!services.users.is_deactivated(user_id).await?, "{user_id} stays active");
	}

	deactivate(&admin, &member_id, 200).await?;

	assert!(services.users.is_deactivated(&member_id).await?, "a member is deactivated");

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
async fn deactivate(admin: &Client<'_>, user_id: &UserId, expected: u16) -> Result<Value> {
	let response = admin
		.admin_api(Method::POST, &format!("v1/deactivate/{user_id}"), &json!({}), expected)
		.await?;

	Ok(serde_json::from_str(&response)?)
}
