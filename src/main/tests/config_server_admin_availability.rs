#![cfg(test)]
#![cfg(feature = "mas")]
#![cfg(feature = "synapse_admin")]

use reqwest::Method;
use serde_json::{Value, json};
use tuwunel_core::Result;
use tuwunel_service::Services;

use self::{
	client::{Client, register},
	fixture::boot,
};

#[expect(dead_code)] // Only the request and registration helpers are needed.
mod client;
mod fixture;

const MAS_SECRET: &str = "config-admin-availability-mas-secret";
const ADMIN_TOKEN: &str = "config-admin-availability-admin-token";

#[test]
fn all_availability_probes_reserve_configured_admin_ids() -> Result {
	// MAS mode omits shared-secret registration, so it needs its own fixture.
	boot(
		"config-server-admin-availability",
		[
			"admin.room.create=true",
			&format!("authentication.mas_secret={MAS_SECRET:?}"),
			r#"server_admins=["@reserved:localhost"]"#,
		],
		exercise,
	)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let operator = register(services, "operator", ADMIN_TOKEN).await?;
	services.admin.make_user_admin(&operator).await?;

	for name in ["unused", "reserved"] {
		let probes = [
			("", format!("/_matrix/client/v3/register/available?username={name}")),
			(ADMIN_TOKEN, format!("/_synapse/admin/v1/username_available?username={name}")),
			(MAS_SECRET, format!("/_synapse/mas/is_localpart_available?localpart={name}")),
		];
		for (token, path) in probes {
			let client = Client { services, base, token };
			let response = client
				.request(Method::GET, &format!("{base}{path}"), &json!({}))
				.await?;
			let status = response.status();
			let body: Value = response.json().await?;
			if name == "reserved" {
				assert!(status.is_client_error(), "{path}: {status} {body}");
				assert_eq!(body["errcode"], "M_USER_IN_USE", "{path}");
			} else {
				assert!(status.is_success(), "{path}: {status} {body}");
			}
		}
	}
	Ok(())
}
