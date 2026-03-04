use axum::{
	extract::State,
	http::Uri,
	response::Html,
};
use axum_client_ip::InsecureClientIp;
use ruma::api::client::uiaa::{AuthType, get_uiaa_fallback_page};
use serde::Deserialize;
use tuwunel_core::{Result, err};

use crate::Ruma;

const SSO_FALLBACK_REDIRECT_HTML: &str = r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <title>Continue with SSO</title>
  </head>
  <body>
    <p>Redirecting to Single Sign-On...</p>
    <script>
      (function () {
        const current = new URL(window.location.href);
        const session = current.searchParams.get("session");
        if (!session) return;

        const complete = new URL("/_matrix/client/v3/auth/m.login.sso/fallback/web/complete", window.location.origin);
        complete.searchParams.set("session", session);

        const sso = new URL("/_matrix/client/v3/login/sso/redirect", window.location.origin);
        sso.searchParams.set("redirectUrl", complete.href);

        window.location.replace(sso.href);
      })();
    </script>
  </body>
</html>
"#;

const SSO_FALLBACK_DONE_HTML: &str = r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <title>Authentication complete</title>
  </head>
  <body>
    <p>Authentication complete. You can close this window.</p>
    <script>
      (function () {
        try {
          if (window.opener) {
            window.opener.postMessage("authDone", window.location.origin);
          }
        } catch (_) {}
        window.close();
      })();
    </script>
  </body>
</html>
"#;

const UNSUPPORTED_FALLBACK_HTML: &str = r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <title>Unsupported authentication stage</title>
  </head>
  <body>
    <p>This fallback stage is not supported by this homeserver.</p>
  </body>
</html>
"#;

/// # `GET /_matrix/client/v3/auth/{auth_type}/fallback/web`
///
/// Serves a UIAA fallback page for SSO stage and redirects through the normal
/// homeserver SSO login flow.
#[tracing::instrument(skip_all, fields(%client), name = "uiaa_fallback")]
pub(crate) async fn get_uiaa_fallback_page_route(
	State(_services): State<crate::State>,
	InsecureClientIp(client): InsecureClientIp,
	body: Ruma<get_uiaa_fallback_page::v3::Request>,
) -> Result<get_uiaa_fallback_page::v3::Response> {
	match body.auth_type {
		| AuthType::Sso => Ok(get_uiaa_fallback_page::v3::Response::html(
			SSO_FALLBACK_REDIRECT_HTML.as_bytes().to_vec(),
		)),
		| _ => Ok(get_uiaa_fallback_page::v3::Response::html(
			UNSUPPORTED_FALLBACK_HTML.as_bytes().to_vec(),
		)),
	}
}

#[derive(Debug, Deserialize)]
pub(crate) struct UiaaSsoFallbackCompleteQuery {
	session: String,
	#[serde(rename = "loginToken")]
	login_token: String,
}

/// Completion endpoint for SSO UIAA fallback.
///
/// This endpoint is reached via SSO callback redirect and marks the UIAA SSO
/// stage as complete for the target session.
#[tracing::instrument(skip_all, name = "uiaa_sso_fallback_complete")]
pub(crate) async fn complete_uiaa_sso_fallback_route(
	State(services): State<crate::State>,
	uri: Uri,
) -> Result<Html<String>> {
	let query = uri.query().unwrap_or_default();
	let query: UiaaSsoFallbackCompleteQuery = serde_html_form::from_str(query)
		.map_err(|_| err!(Request(InvalidParam("Missing or invalid UIAA fallback query parameters"))))?;

	let user_id = services
		.users
		.find_from_login_token(&query.login_token)
		.await?;

	services
		.users
		.maybe_repair_legacy_sso_origin(&user_id)
		.await;

	services
		.uiaa
		.complete_stage(&user_id, &query.session, AuthType::Sso)
		.await
		.map_err(|_| err!(Request(Forbidden("UIAA fallback session is invalid"))))?;

	Ok(Html(SSO_FALLBACK_DONE_HTML.to_owned()))
}
