#![cfg(test)]

use hmac::{Hmac, Mac};
use reqwest::Method;
use serde_json::{Value, json};
use sha1::Sha1;
use tuwunel_core::{
	Result,
	ruma::{UserId, events::room::message::RoomMessageEventContent, user_id},
};
use tuwunel_service::Services;

use self::{
	appservice::{Bridge, register_appservice},
	client::{Client, field, register},
	fixture::boot,
	timeline::append_message,
};

#[path = "client/admin_api.rs"]
mod admin_api;
mod appservice;
mod client;
#[path = "client/get.rs"]
mod client_get;
mod fixture;
mod timeline;

const MAS_SECRET: &str = "config-admin-test-mas-secret";
const REGISTRATION_SECRET: &str = "config-admin-test-shared-secret";

#[test]
fn configured_admins_have_http_authority_but_not_room_membership() -> Result {
	boot(
		"config-server-admins",
		[
			"admin.room.create=true",
			"admin.escape_commands=true",
			r#"admin.execute=["users create-user helper test-password"]"#,
			&format!("authentication.mas_secret={MAS_SECRET:?}"),
			"registration.enable=true",
			"registration.grant_admin_to_first_user=true",
			"registration.\
			 yes_i_am_very_very_sure_i_want_an_open_registration_server_prone_to_abuse=true",
			&format!("registration.shared_secret={REGISTRATION_SECRET:?}"),
			r#"server_admins=["@helper:localhost","@visitor:localhost","@reserved:localhost","@provisioned:localhost"]"#,
		],
		exercise,
	)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let token = "config-admin-service-token-0123456789";
	let admin = &services.admin;
	let cache = &services.state_cache;
	// The startup command provisions the listed account before any human signs up.
	let helper = user_id!("@helper:localhost").to_owned();
	let admin_room = admin.get_admin_room().await?;
	assert!(services.users.exists(&helper).await);
	assert!(!cache.is_joined(&helper, &admin_room).await, "listed first user must not join");
	services
		.users
		.create_device(&helper, None, (Some(token), None), None, None, None)
		.await?;
	let visitor = register(services, "visitor", "config-admin-visitor-token-0123456789").await?;
	services.users.set_external(&visitor);

	let service = Client { services, base, token };
	let signup = Client { token: "", ..service };
	let response = signup
		.post("register", &signup_form("operator"))
		.await?;
	let human = Client {
		token: field(&response, "access_token")?,
		..service
	};
	let operator = user_id!("@operator:localhost");
	let ordinary =
		register(services, "ordinary", "config-admin-ordinary-token-0123456789").await?;
	assert!(
		cache.is_joined(operator, &admin_room).await,
		"unlisted user must still bootstrap"
	);
	assert!(admin.user_is_last_admin(operator).await);
	assert!(!admin.user_is_last_admin(&helper).await);
	assert!(!cache.is_joined(&helper, &admin_room).await);

	check_availability(&human, &signup).await?;
	check_admin_updates(&human, &service, &helper).await?;
	check_external_promotion(&human, &visitor).await?;
	check_shared_secret_provisioning(&signup).await?;
	let reserved = user_id!("@reserved:localhost");
	for (user, expected) in
		[(&*helper, true), (&*ordinary, false), (&*visitor, false), (reserved, false)]
	{
		let status = human
			.admin_api(Method::GET, &format!("v1/users/{user}/admin"), &json!({}), 200)
			.await?;
		assert_eq!(serde_json::from_str::<Value>(&status)?["admin"], expected, "{user}");
	}

	let room = service
		.create_room(&json!({ "preset": "private_chat" }))
		.await?;
	service
		.admin_api(Method::POST, &format!("v1/join/{room}"), &json!({ "user_id": ordinary }), 200)
		.await?;
	assert!(cache.is_joined(&ordinary, &room).await);
	let content = RoomMessageEventContent::text_plain("Hello.");
	let id = append_message(services, &helper, &room, &content).await?;
	let event = services.timeline.get_pdu(&id).await?;
	assert!(!admin.is_admin_command(&event, "\\!admin").await);

	admin.make_user_admin(&helper).await?;
	assert!(admin.is_admin_command(&event, "\\!admin").await);
	let revoke = human
		.admin_api(Method::PUT, &format!("v2/users/{helper}"), &json!({ "admin": false }), 400)
		.await?;
	assert_eq!(serde_json::from_str::<Value>(&revoke)?["errcode"], "M_INVALID_PARAM");
	assert!(revoke.contains("server_admins"), "{revoke}");
	assert!(cache.is_joined(&helper, &admin_room).await);

	check_reserved_signup(&signup, &helper).await?;
	human
		.admin_api(Method::PUT, &format!("v2/users/{reserved}"), &json!({"admin": true}), 200)
		.await?;
	assert!(admin.user_is_admin(reserved).await);
	assert!(!cache.is_joined(reserved, &admin_room).await);
	Ok(())
}

async fn check_availability(human: &Client<'_>, signup: &Client<'_>) -> Result {
	let mas = Client { token: MAS_SECRET, ..*signup };
	for username in ["unused", "reserved"] {
		let probes = [
			(signup, signup.url(&format!("register/available?username={username}"))),
			(
				human,
				format!(
					"{}/_synapse/admin/v1/username_available?username={username}",
					human.base
				),
			),
			(
				&mas,
				format!("{}/_synapse/mas/is_localpart_available?localpart={username}", mas.base),
			),
		];
		for (client, url) in probes {
			let response = client.get_url(&url).await?;
			let status = response.status();
			let body: Value = response.json().await?;
			if username == "reserved" {
				assert!(status.is_client_error(), "{url}: {status} {body}");
				assert_eq!(body["errcode"], "M_USER_IN_USE", "{url}");
			} else {
				assert!(status.is_success(), "{url}: {status} {body}");
			}
		}
	}
	Ok(())
}

async fn check_admin_updates(
	human: &Client<'_>,
	service: &Client<'_>,
	helper: &UserId,
) -> Result {
	let services = human.services;
	let helper_path = format!("v2/users/{helper}");
	let reserved = user_id!("@reserved:localhost");
	let demote =
		json!({ "admin": false, "password": "different-password", "logout_devices": true });
	let password = services.users.password_hash(helper).await?;
	let device = services
		.users
		.find_from_token(service.token)
		.await?;

	human
		.admin_api(Method::PUT, &helper_path, &demote, 400)
		.await?;
	assert_eq!(services.users.password_hash(helper).await?, password);
	assert_eq!(
		services
			.users
			.find_from_token(service.token)
			.await?,
		device
	);
	human
		.admin_api(Method::PUT, &format!("v2/users/{reserved}"), &demote, 400)
		.await?;
	assert!(!services.users.exists(reserved).await);

	let body = human
		.admin_api(Method::PUT, &helper_path, &json!({ "admin": true }), 200)
		.await?;
	assert_eq!(serde_json::from_str::<Value>(&body)?["admin"], true);
	let room = services.admin.get_admin_room().await?;
	assert!(
		!services
			.state_cache
			.is_joined(helper, &room)
			.await
	);
	Ok(())
}

async fn check_external_promotion(human: &Client<'_>, visitor: &UserId) -> Result {
	let profile = &human.services.profile;
	profile
		.set_displayname(visitor, Some("Visitor"), None)
		.await?;
	human
		.admin_api(
			Method::PUT,
			&format!("v2/users/{visitor}"),
			&json!({ "admin": true, "displayname": "Changed" }),
			403,
		)
		.await?;
	assert_eq!(profile.displayname(visitor).await?, "Visitor");
	Ok(())
}

async fn check_shared_secret_provisioning(signup: &Client<'_>) -> Result {
	let username = "provisioned";
	let password = "test-password";
	let response = signup
		.admin_api(Method::GET, "v1/register", &json!({}), 200)
		.await?;
	let response: Value = serde_json::from_str(&response)?;
	let nonce = field(&response, "nonce")?;
	let mut mac = Hmac::<Sha1>::new_from_slice(REGISTRATION_SECRET.as_bytes())
		.expect("HMAC accepts any key size");
	mac.update(format!("{nonce}\0{username}\0{password}\0admin").as_bytes());
	let mac = format!("{:x}", mac.finalize().into_bytes());
	let response = signup
		.admin_api(
			Method::POST,
			"v1/register",
			&json!({ "nonce": nonce, "username": username, "password": password,
				"admin": true, "mac": mac, "inhibit_login": true }),
			200,
		)
		.await?;
	let user = user_id!("@provisioned:localhost");
	assert_eq!(serde_json::from_str::<Value>(&response)?["user_id"], user.as_str());
	let services = signup.services;
	assert!(services.admin.user_is_admin(user).await);
	let room = services.admin.get_admin_room().await?;
	assert!(!services.state_cache.is_joined(user, &room).await);
	Ok(())
}

fn signup_form(username: &str) -> Value {
	json!({ "username": username, "password": "test-password", "auth": { "type": "m.login.dummy" } })
}

async fn check_reserved_signup(signup: &Client<'_>, helper: &UserId) -> Result {
	let url = signup.url("register");
	let reserved = user_id!("@reserved:localhost");
	let response: Value = signup
		.post_url(&url, &signup_form("reserved"))
		.await?
		.json()
		.await?;
	assert_eq!(response["errcode"], "M_USER_IN_USE");

	let bridge = Bridge {
		id: "example-bridge",
		token: "config-admin-bridge-token",
		sender_localpart: "bridge",
		users: "^@(reserved|unlisted):localhost$",
		aliases: None,
	};
	let services = signup.services;
	let collision = Bridge { sender_localpart: "reserved", ..bridge };
	let refusal = register_appservice(services, &collision)
		.await
		.expect_err("an appservice must not provision a reserved sender");
	assert!(refusal.to_string().contains("server_admins"), "{refusal}");
	assert!(!services.users.exists(reserved).await);

	register_appservice(services, &bridge).await?;
	assert!(
		services
			.users
			.exists(user_id!("@bridge:localhost"))
			.await
	);
	let appservice = Client { token: bridge.token, ..*signup };
	let unlisted = user_id!("@unlisted:localhost");
	assert!(!services.users.exists(unlisted).await);
	let whoami = appservice
		.get(&format!("account/whoami?user_id={unlisted}"))
		.await?;
	assert_eq!(whoami["user_id"], unlisted.as_str());
	let refusal = appservice
		.get_url(&appservice.url(&format!("account/whoami?user_id={reserved}")))
		.await?;
	assert_eq!(refusal.status().as_u16(), 403);
	assert_eq!(refusal.json::<Value>().await?["errcode"], "M_FORBIDDEN");
	check_appservice_target_writes(&appservice, unlisted, reserved, helper).await?;
	let form = json!({ "username": "reserved", "type": "m.login.application_service" });
	let response: Value = appservice
		.post_url(&url, &form)
		.await?
		.json()
		.await?;
	assert_eq!(response["errcode"], "M_USER_IN_USE");
	assert!(!services.users.exists(reserved).await);

	let provisioned = Bridge {
		sender_localpart: helper.localpart(),
		..bridge
	};
	register_appservice(services, &provisioned).await?;
	Ok(())
}

async fn check_appservice_target_writes(
	appservice: &Client<'_>,
	unlisted: &UserId,
	reserved: &UserId,
	outside_namespace: &UserId,
) -> Result {
	const KIND: &str = "org.example.settings";
	let services = appservice.services;
	assert!(!services.users.exists(unlisted).await);
	assert!(!services.users.exists(reserved).await);
	for (user, expected) in [
		(user_id!("@bridge:localhost"), 200),
		(unlisted, 200),
		(reserved, 403),
		(outside_namespace, 403),
	] {
		let account_data = format!("user/{user}/account_data/{KIND}");
		let profile = format!("profile/{user}/displayname");
		for (path, body) in [
			(&account_data, json!({"enabled": true})),
			(&profile, json!({"displayname": "Example"})),
		] {
			let reply = appservice
				.request(Method::PUT, &appservice.url(path), &body)
				.await?;
			assert_eq!(reply.status().as_u16(), expected, "{path}");
		}
	}
	assert_eq!(
		appservice
			.get(&format!("user/{unlisted}/account_data/{KIND}"))
			.await?,
		json!({"enabled": true})
	);
	assert_eq!(services.profile.displayname(unlisted).await?, "Example");
	assert!(
		services
			.account_data
			.get_raw(None, reserved, KIND)
			.await
			.is_err_and(|e| e.is_not_found())
	);
	assert!(
		services
			.profile
			.displayname(reserved)
			.await
			.is_err_and(|e| e.is_not_found())
	);
	Ok(())
}
