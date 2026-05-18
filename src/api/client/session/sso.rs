mod uiaa;

use std::{
	borrow::Cow,
	collections::{BTreeMap, BTreeSet},
	fmt::Write as _,
	future::Future,
	net::IpAddr,
	sync::OnceLock,
	time::{Duration, Instant},
};

use axum::{Json, extract::State, response::IntoResponse};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as b64};
use futures::{FutureExt, StreamExt, TryFutureExt, future::try_join};
use http::StatusCode;
use reqwest::header::{CONTENT_TYPE, HeaderValue};
use ruma::{
	Mxc, OwnedMxcUri, OwnedRoomId, OwnedUserId, ServerName, UserId,
	api::client::{
		session::{SsoRedirectAction, sso_callback, sso_login, sso_login_with_provider},
		uiaa::AuthType,
	},
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tokio::sync::RwLock;
use tuwunel_core::{
	Err, Result, at,
	config::IdentityProvider,
	debug::INFO_SPAN_LEVEL,
	debug_info, debug_warn, err, info, is_not_equal_to,
	itertools::Itertools,
	jwt::{Algorithm, DecodingKey, Header, Validation, decode, decode_header},
	utils,
	utils::{
		OptionExt,
		content_disposition::make_content_disposition,
		hash::sha256,
		result::{FlatOk, LogErr},
		string::{EMPTY, truncate_deterministic},
		timepoint_from_now, timepoint_has_passed,
	},
	warn,
};
use tuwunel_service::{
	Services,
	client::read_response_capped,
	media::MXC_LENGTH,
	oauth::{
		CODE_VERIFIER_LENGTH, Provider, SESSION_ID_LENGTH, Session, TokenResponse, UserInfo,
		unique_id_sub,
	},
	users::{PASSWORD_SENTINEL, Register, propagation_default},
};
use url::Url;

pub(crate) use self::uiaa::sso_fallback_route;
use super::TOKEN_LENGTH;
use crate::{ClientIp, Ruma};

/// Grant phase query string.
#[derive(Debug, Serialize)]
struct GrantQuery<'a> {
	client_id: &'a str,
	state: &'a str,
	nonce: &'a str,
	scope: &'a str,
	response_type: &'a str,
	access_type: &'a str,
	code_challenge_method: &'a str,
	code_challenge: &'a str,
	redirect_uri: Option<&'a str>,
	#[serde(skip_serializing_if = "Option::is_none")]
	prompt: Option<&'a str>,
}

#[derive(Debug, Deserialize, Serialize)]
struct GrantCookie<'a> {
	client_id: Cow<'a, str>,
	state: Cow<'a, str>,
	nonce: Cow<'a, str>,
	redirect_uri: Cow<'a, str>,
}

static GRANT_SESSION_COOKIE: &str = "tuwunel_grant_session";
static APPLE_ISSUER: &str = "https://appleid.apple.com";
static APPLE_JWKS_URL: &str = "https://appleid.apple.com/auth/keys";
static APPLE_JWKS_CACHE: OnceLock<RwLock<Option<CachedAppleJwks>>> = OnceLock::new();

const APPLE_JWKS_CACHE_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeAppleLoginRequest {
	identity_token: String,
	// Kept for auditability and future code-exchange/revocation checks; the
	// current exchange validates the identity token directly.
	authorization_code: Option<String>,
	nonce: Option<String>,
	provider_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeAppleLoginResponse {
	login_token: String,
	expires_in_ms: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct AppleJwks {
	keys: Vec<AppleJwk>,
}

#[derive(Clone, Debug, Deserialize)]
struct AppleJwk {
	alg: Option<String>,
	e: String,
	kid: String,
	kty: String,
	n: String,
}

#[derive(Debug, Deserialize)]
struct AppleIdTokenClaims {
	iss: String,
	aud: String,
	#[expect(
		dead_code,
		reason = "jsonwebtoken validates this registered claim"
	)]
	exp: u64,
	sub: String,
	email: Option<String>,
	name: Option<String>,
	given_name: Option<String>,
	family_name: Option<String>,
	nonce: Option<String>,
}

#[derive(Clone, Debug)]
struct CachedAppleJwks {
	jwks: AppleJwks,
	fetched_at: Instant,
}

fn apple_native_audiences(provider: &Provider) -> BTreeSet<String> {
	let mut audiences = provider.native_client_ids.clone();
	audiences.insert(provider.client_id.clone());
	audiences
}

fn sha256_hex(value: &str) -> String {
	let digest = sha256::hash(value.as_bytes());
	let mut output = String::new();

	for byte in digest {
		write!(&mut output, "{byte:02x}").expect("write to string");
	}

	output
}

fn apple_userinfo_from_validated_claims(
	provider: &Provider,
	claims: AppleIdTokenClaims,
	raw_nonce: Option<&str>,
) -> Result<UserInfo> {
	if claims.iss != APPLE_ISSUER {
		return Err!(Request(Unauthorized("Apple id_token issuer is not trusted.")));
	}

	if !apple_native_audiences(provider).contains(&claims.aud) {
		return Err!(Request(Unauthorized(
			"Apple id_token audience is not configured for this provider."
		)));
	}

	match (claims.nonce.as_deref(), raw_nonce) {
		| (Some(token_nonce), Some(raw_nonce)) => {
			let expected_nonce = sha256_hex(raw_nonce);
			if token_nonce != expected_nonce.as_str() {
				return Err!(Request(Unauthorized("Apple id_token nonce does not match.")));
			}
		},
		| (Some(_), None) => {
			return Err!(Request(Unauthorized(
				"Apple id_token nonce is present but no request nonce was supplied."
			)));
		},
		| (None, Some(_)) => {
			return Err!(Request(Unauthorized("Apple id_token nonce is missing.")));
		},
		| (None, None) => {},
	}

	Ok(apple_userinfo_from_claim_values(
		claims.sub,
		claims.email,
		claims.name,
		claims.given_name,
		claims.family_name,
	))
}

fn apple_userinfo_from_claim_values(
	sub: String,
	email: Option<String>,
	name: Option<String>,
	given_name: Option<String>,
	family_name: Option<String>,
) -> UserInfo {
	let preferred_username = email
		.as_deref()
		.and_then(|value| value.split_once('@'))
		.map(at!(0))
		.map(ToOwned::to_owned);

	UserInfo {
		sub,
		preferred_username: preferred_username.clone(),
		username: preferred_username,
		nickname: None,
		name,
		given_name,
		family_name,
		email,
		avatar_url: None,
		picture: None,
	}
}

fn decode_apple_userinfo_from_id_token(session: &Session) -> Result<UserInfo> {
	let id_token = session.id_token.as_deref().ok_or_else(|| {
		err!(Request(Unauthorized("Missing Apple id_token in token response.")))
	})?;

	let payload_b64 = id_token
		.split('.')
		.nth(1)
		.ok_or_else(|| err!(Request(Unauthorized("Apple id_token is malformed."))))?;

	let payload = b64
		.decode(payload_b64)
		.map_err(|_| err!(Request(Unauthorized("Apple id_token payload is invalid base64."))))?;

	let payload: JsonValue = serde_json::from_slice(&payload)
		.map_err(|_| err!(Request(Unauthorized("Apple id_token payload is not valid JSON."))))?;

	let sub = payload
		.get("sub")
		.and_then(JsonValue::as_str)
		.ok_or_else(|| {
			err!(Request(Unauthorized("Apple id_token missing required sub claim.")))
		})?;

	let email = payload
		.get("email")
		.and_then(JsonValue::as_str)
		.map(ToOwned::to_owned);

	Ok(apple_userinfo_from_claim_values(
		sub.to_owned(),
		email,
		payload
			.get("name")
			.and_then(JsonValue::as_str)
			.map(ToOwned::to_owned),
		payload
			.get("given_name")
			.and_then(JsonValue::as_str)
			.map(ToOwned::to_owned),
		payload
			.get("family_name")
			.and_then(JsonValue::as_str)
			.map(ToOwned::to_owned),
	))
}

fn apple_id_token_header(token: &str) -> Result<Header> {
	let header = decode_header(token)
		.map_err(|e| err!(Request(Unauthorized("Apple id_token header is invalid: {e}"))))?;

	if header.alg != Algorithm::RS256 {
		return Err!(Request(Unauthorized("Apple id_token uses unsupported signing algorithm.")));
	}

	if header.kid.is_none() {
		return Err!(Request(Unauthorized("Apple id_token missing key id.")));
	}

	Ok(header)
}

fn apple_decoding_key_for_kid(kid: &str, jwks: &AppleJwks) -> Result<Option<DecodingKey>> {
	let Some(jwk) = jwks.keys.iter().find(|key| key.kid == kid) else {
		return Ok(None);
	};

	if jwk.kty != "RSA"
		|| jwk
			.alg
			.as_deref()
			.is_some_and(|alg| alg != "RS256")
	{
		return Err!(Request(Unauthorized("Apple id_token key is not an RSA signing key.")));
	}

	DecodingKey::from_rsa_components(&jwk.n, &jwk.e)
		.map(Some)
		.map_err(|e| err!(Request(Unauthorized("Apple id_token signing key is invalid: {e}"))))
}

fn apple_jwks_contains_kid(jwks: &AppleJwks, kid: &str) -> bool {
	jwks.keys.iter().any(|key| key.kid == kid)
}

fn cached_apple_jwks_is_fresh(cached: &CachedAppleJwks) -> bool {
	cached.fetched_at.elapsed() < APPLE_JWKS_CACHE_TTL
}

fn apple_id_token_validation(provider: &Provider) -> Validation {
	let audiences = apple_native_audiences(provider)
		.into_iter()
		.collect::<Vec<_>>();
	let issuers = [APPLE_ISSUER.to_owned()];
	let required_spec_claims: Vec<_> = ["iss", "aud", "exp", "sub"].into();
	let mut validation = Validation::new(Algorithm::RS256);

	validation.set_audience(&audiences);
	validation.set_issuer(&issuers);
	validation.set_required_spec_claims(&required_spec_claims);

	validation
}

fn native_apple_provider_id(
	requested_provider_id: Option<&str>,
	identity_providers: &BTreeMap<String, IdentityProvider>,
) -> Result<String> {
	if let Some(provider_id) = requested_provider_id {
		return Ok(provider_id.to_owned());
	}

	let mut apple_providers = identity_providers
		.values()
		.filter(|provider| provider.brand == "appleoidc")
		.map(IdentityProvider::id);
	let Some(provider_id) = apple_providers.next() else {
		return Err!(Request(NotFound(
			"No AppleOIDC identity provider is configured for native Apple login."
		)));
	};

	if apple_providers.next().is_some() {
		return Err!(Request(InvalidParam(
			"Native Apple login requires provider_id when multiple AppleOIDC identity providers \
			 are configured."
		)));
	}

	Ok(provider_id.to_owned())
}

async fn fetch_apple_jwks(services: &Services) -> Result<AppleJwks> {
	services
		.client
		.oauth
		.get(APPLE_JWKS_URL)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await
		.map_err(Into::into)
}

async fn cached_apple_jwks(services: &Services) -> Result<AppleJwks> {
	let cache = APPLE_JWKS_CACHE.get_or_init(|| RwLock::new(None));

	{
		let cached = cache.read().await;
		if let Some(cached) = cached
			.as_ref()
			.filter(|cached| cached_apple_jwks_is_fresh(cached))
		{
			return Ok(cached.jwks.clone());
		}
	}

	let mut cached = cache.write().await;
	if let Some(cached) = cached
		.as_ref()
		.filter(|cached| cached_apple_jwks_is_fresh(cached))
	{
		return Ok(cached.jwks.clone());
	}

	let jwks = fetch_apple_jwks(services).await?;
	*cached = Some(CachedAppleJwks {
		jwks: jwks.clone(),
		fetched_at: Instant::now(),
	});

	Ok(jwks)
}

async fn refresh_apple_jwks_from_cache(
	cache: &RwLock<Option<CachedAppleJwks>>,
	kid: &str,
	fetch: impl Future<Output = Result<AppleJwks>>,
) -> Result<AppleJwks> {
	let mut cached = cache.write().await;

	if let Some(cached) = cached
		.as_ref()
		.filter(|cached| cached_apple_jwks_is_fresh(cached))
		.filter(|cached| apple_jwks_contains_kid(&cached.jwks, kid))
	{
		return Ok(cached.jwks.clone());
	}

	let jwks = fetch.await?;
	*cached = Some(CachedAppleJwks {
		jwks: jwks.clone(),
		fetched_at: Instant::now(),
	});

	Ok(jwks)
}

async fn refresh_apple_jwks(services: &Services, kid: &str) -> Result<AppleJwks> {
	let cache = APPLE_JWKS_CACHE.get_or_init(|| RwLock::new(None));

	refresh_apple_jwks_from_cache(cache, kid, fetch_apple_jwks(services)).await
}

async fn validate_apple_identity_token(
	services: &Services,
	provider: &Provider,
	identity_token: &str,
) -> Result<AppleIdTokenClaims> {
	let header = apple_id_token_header(identity_token)?;
	let kid = header
		.kid
		.as_deref()
		.expect("apple_id_token_header validates kid");
	let jwks = cached_apple_jwks(services).await?;
	let decoding_key = if let Some(decoding_key) = apple_decoding_key_for_kid(kid, &jwks)? {
		decoding_key
	} else {
		let jwks = refresh_apple_jwks(services, kid).await?;
		apple_decoding_key_for_kid(kid, &jwks)?
			.ok_or_else(|| err!(Request(Unauthorized("Apple id_token key id is not trusted."))))?
	};
	let validation = apple_id_token_validation(provider);

	decode::<AppleIdTokenClaims>(identity_token, &decoding_key, &validation)
		.map(|decoded| decoded.claims)
		.map_err(|e| err!(Request(Unauthorized("Apple id_token is invalid: {e}"))))
}

#[tracing::instrument(name = "native_apple_login", level = "info", skip_all)]
pub(crate) async fn native_apple_login_route(
	State(services): State<crate::State>,
	Json(body): Json<NativeAppleLoginRequest>,
) -> Result<impl IntoResponse> {
	let provider_id = native_apple_provider_id(
		body.provider_id.as_deref(),
		&services.config.identity_provider,
	)?;
	let provider = services.oauth.providers.get(&provider_id).await?;

	if provider.brand != "appleoidc" {
		return Err!(Request(InvalidParam(
			"Native Apple login requires an AppleOIDC identity provider."
		)));
	}

	let claims =
		validate_apple_identity_token(&services, &provider, &body.identity_token).await?;
	let userinfo =
		apple_userinfo_from_validated_claims(&provider, claims, body.nonce.as_deref())?;
	let sess_id = utils::random_string(SESSION_ID_LENGTH);
	let session = Session {
		idp_id: Some(provider.id().to_owned()),
		sess_id: Some(sess_id),
		id_token: Some(body.identity_token),
		..Default::default()
	};

	if let Some(authorization_code) = body.authorization_code.as_deref() {
		debug_info!(
			code_len = authorization_code.len(),
			provider = provider.id(),
			"Received native Apple authorization code alongside id_token.",
		);
	}

	let (user_id, _) = complete_sso_session(&services, &provider, session, userinfo).await?;
	let login_token = utils::random_string(TOKEN_LENGTH);
	let expires_in_ms = services
		.users
		.create_login_token(&user_id, &login_token);

	Ok((StatusCode::OK, Json(NativeAppleLoginResponse { login_token, expires_in_ms })))
}

/// # `GET /_matrix/client/v3/login/sso/redirect`
///
/// A web-based Matrix client should instruct the user’s browser to navigate to
/// this endpoint in order to log in via SSO.
#[tracing::instrument(
	name = "sso_login",
	level = "debug",
	skip_all,
	fields(%client),
)]
pub(crate) async fn sso_login_route(
	State(services): State<crate::State>,
	ClientIp(client): ClientIp,
	body: Ruma<sso_login::v3::Request>,
) -> Result<sso_login::v3::Response> {
	if services.config.sso_custom_providers_page {
		return Err!(Request(NotImplemented(
			"sso_custom_providers_page has been enabled but this URL has not been overridden \
			 with any custom page listing the available providers..."
		)));
	}

	let redirect_url = body.body.redirect_url;
	let action = body.body.action;
	let default_idp_id = services
		.oauth
		.providers
		.get_default_id()
		.unwrap_or_default();

	handle_sso_login(&services, &client, default_idp_id, redirect_url, None, action)
		.map_ok(|response| sso_login::v3::Response {
			location: response.location,
			cookie: response.cookie,
		})
		.await
}

/// # `GET /_matrix/client/v3/login/sso/redirect/{idpId}`
///
/// This endpoint is the same as /login/sso/redirect, though with an IdP ID from
/// the original identity_providers array to inform the server of which IdP the
/// client/user would like to continue with.
#[tracing::instrument(
	name = "sso_login_with_provider",
	level = "info",
	skip_all,
	ret(level = "debug")
	fields(
		%client,
		idp_id = body.body.idp_id,
	),
)]
pub(crate) async fn sso_login_with_provider_route(
	State(services): State<crate::State>,
	ClientIp(client): ClientIp,
	body: Ruma<sso_login_with_provider::v3::Request>,
) -> Result<sso_login_with_provider::v3::Response> {
	let idp_id = body.body.idp_id;
	let redirect_url = body.body.redirect_url;
	let login_token = body.body.login_token;
	let action = body.body.action;

	handle_sso_login(&services, &client, idp_id, redirect_url, login_token, action).await
}

async fn handle_sso_login(
	services: &Services,
	_client: &IpAddr,
	idp_id: String,
	redirect_url: String,
	login_token: Option<String>,
	action: Option<SsoRedirectAction>,
) -> Result<sso_login_with_provider::v3::Response> {
	let redirect_url: Url = redirect_url.parse().map_err(|e| {
		err!(Request(InvalidParam(debug_warn!(
			?e,
			?redirect_url,
			"Failed to parse redirect_url.",
		))))
	})?;

	let provider = services.oauth.providers.get(&idp_id).await?;
	let sess_id = utils::random_string(SESSION_ID_LENGTH);
	let query_nonce = utils::random_string(CODE_VERIFIER_LENGTH);
	let cookie_nonce = utils::random_string(CODE_VERIFIER_LENGTH);
	let code_verifier = utils::random_string(CODE_VERIFIER_LENGTH);
	let code_challenge = b64.encode(sha256::hash(code_verifier.as_bytes()));
	let callback_uri = provider.callback_url.as_ref().map(Url::as_str);
	let scope = provider.scope.iter().join(" ");
	let prompt = action
		.filter(|_| provider.forward_action_prompt)
		.and_then(|action| matches!(action, SsoRedirectAction::Register).then_some("create"));

	let query = GrantQuery {
		client_id: &provider.client_id,
		state: &sess_id,
		nonce: &query_nonce,
		access_type: "online",
		response_type: "code",
		code_challenge_method: "S256",
		code_challenge: &code_challenge,
		redirect_uri: callback_uri,
		prompt,
		scope: scope
			.is_empty()
			.then_some("openid email profile")
			.unwrap_or(scope.as_str()),
	};

	let location = provider
		.authorization_url
		.clone()
		.map(|mut location| {
			let query = serde_html_form::to_string(&query).ok();
			location.set_query(query.as_deref());
			if !provider.extra_authorization_parameters.is_empty() {
				// Base wins on key collision so extras cannot disable CSRF/PKCE.
				let merged: BTreeMap<String, String> = provider
					.extra_authorization_parameters
					.clone()
					.into_iter()
					.chain(
						location
							.query_pairs()
							.map(|(k, v)| (k.into_owned(), v.into_owned())),
					)
					.collect();

				location.set_query(None);
				location.query_pairs_mut().extend_pairs(&merged);
			}
			location
		})
		.ok_or_else(|| {
			err!(Config("authorization_url", "Missing required IdentityProvider config"))
		})?;

	let cookie_val = GrantCookie {
		client_id: query.client_id.into(),
		state: query.state.into(),
		nonce: cookie_nonce.as_str().into(),
		redirect_uri: redirect_url.as_str().into(),
	};

	let cookie_path = provider
		.callback_url
		.as_ref()
		.map(Url::path)
		.unwrap_or("/");

	let cookie_max_age = provider
		.grant_session_duration
		.map(Duration::from_secs)
		.expect("Defaulted to Some value during configure_idp()")
		.try_into()
		.expect("std::time::Duration to time::Duration conversion failure");

	let cookie = Cookie::build((GRANT_SESSION_COOKIE, serde_html_form::to_string(&cookie_val)?))
		.path(cookie_path)
		.max_age(cookie_max_age)
		.same_site(SameSite::None)
		.secure(true)
		.http_only(true)
		.build()
		.to_string()
		.into();

	let session = Session {
		idp_id: Some(idp_id),
		sess_id: Some(sess_id.clone()),
		redirect_url: Some(redirect_url),
		code_verifier: Some(code_verifier),
		query_nonce: Some(query_nonce),
		cookie_nonce: Some(cookie_nonce),
		authorize_expires_at: provider
			.grant_session_duration
			.map(Duration::from_secs)
			.map(timepoint_from_now)
			.transpose()?,

		user_id: login_token
			.as_deref()
			.map_async(|token| services.users.find_from_login_token(token))
			.map(FlatOk::flat_ok)
			.await,

		..Default::default()
	};

	services.oauth.sessions.put(&session).await;

	Ok(sso_login_with_provider::v3::Response {
		location: location.into(),
		cookie: Some(cookie),
	})
}

#[tracing::instrument(
	name = "sso_callback"
	level = "debug",
	skip_all,
	fields(
		%client,
		cookie = ?body.cookie,
		body = ?body.body,
	),
)]
pub(crate) async fn sso_callback_route(
	State(services): State<crate::State>,
	ClientIp(client): ClientIp,
	body: Ruma<sso_callback::unstable::Request>,
) -> Result<sso_callback::unstable::Response> {
	let sess_id = body
		.body
		.state
		.as_deref()
		.ok_or_else(|| err!(Request(Forbidden("Missing sess_id in callback."))))?;

	let code = body
		.body
		.code
		.as_deref()
		.ok_or_else(|| err!(Request(Forbidden("Missing code in callback."))))?;

	let session = services
		.oauth
		.sessions
		.get(sess_id)
		.map_err(|_| err!(Request(Forbidden("Invalid state in callback"))));

	let provider = services
		.oauth
		.providers
		.get(body.body.idp_id.as_str());

	let (provider, session) = try_join(provider, session).await.log_err()?;
	let idp_id = provider.id();

	if session.sess_id.as_deref() != Some(sess_id) {
		return Err!(Request(Unauthorized("Session ID {sess_id:?} not recognized.")));
	}

	if session.idp_id.as_deref() != Some(idp_id) {
		return Err!(Request(Unauthorized(
			"Identity Provider {idp_id:?} session not recognized."
		)));
	}

	if session
		.authorize_expires_at
		.is_some_and(timepoint_has_passed)
	{
		return Err!(Request(Unauthorized("Authorization grant session has expired.")));
	}

	if provider.check_cookie {
		validate_session_cookie(&body.cookie, &provider, &session, sess_id)?;
	}

	let token_response = services
		.oauth
		.request_token((&provider, &session), code)
		.await?;

	let session = apply_token_response(session, token_response)?;

	let userinfo = services
		.oauth
		.request_userinfo((&provider, &session))
		.await
		.or_else(|error| {
			if provider.brand != "appleoidc" {
				return Err(error);
			}

			debug_warn!(
				?error,
				idp_id = provider.id(),
				"Failed to fetch Apple userinfo endpoint; falling back to id_token claims.",
			);

			decode_apple_userinfo_from_id_token(&session).map_err(|decode_error| {
				debug_warn!(
					?decode_error,
					idp_id = provider.id(),
					"Failed to decode Apple id_token fallback.",
				);
				error
			})
		})?;

	let (user_id, session) =
		complete_sso_session(&services, &provider, session, userinfo).await?;

	let cookie = Cookie::build((GRANT_SESSION_COOKIE, EMPTY))
		.removal()
		.build()
		.to_string()
		.into();

	if let Some(redirect_url) = session
		.redirect_url
		.as_ref()
		.filter(|url| url.scheme() == "uiaa")
	{
		return handle_uiaa(&services, &user_id, cookie, redirect_url).await;
	}

	let next_idp_url = chain_next_idp_url(&services, &provider, &session, idp_id);

	let location = finalize_login_redirect(&services, &session, next_idp_url, &user_id)?;

	Ok(sso_callback::unstable::Response { location, cookie: Some(cookie) })
}

fn validate_session_cookie(
	cookies: &CookieJar,
	provider: &Provider,
	session: &Session,
	sess_id: &str,
) -> Result {
	let client_id = &provider.client_id;
	let cookie = cookies
		.get(GRANT_SESSION_COOKIE)
		.map(Cookie::value)
		.map(serde_html_form::from_str::<GrantCookie<'_>>)
		.transpose()?
		.ok_or_else(|| err!(Request(Unauthorized("Missing cookie {GRANT_SESSION_COOKIE:?}"))))?;

	if cookie.client_id.as_ref() != client_id.as_str() {
		return Err!(Request(Unauthorized("Client ID {client_id:?} cookie mismatch.")));
	}

	if Some(cookie.nonce.as_ref()) != session.cookie_nonce.as_deref() {
		return Err!(Request(Unauthorized("Cookie nonce does not match session state.")));
	}

	if cookie.state.as_ref() != sess_id {
		return Err!(Request(Unauthorized("Session ID {sess_id:?} cookie mismatch.")));
	}

	Ok(())
}

fn apply_token_response(session: Session, token: TokenResponse) -> Result<Session> {
	let expires_at = token
		.expires_in
		.map(Duration::from_secs)
		.map(timepoint_from_now)
		.transpose()?;

	let refresh_token_expires_at = token
		.refresh_token_expires_in
		.map(Duration::from_secs)
		.map(timepoint_from_now)
		.transpose()?;

	Ok(Session {
		scope: token.scope,
		token_type: token.token_type,
		access_token: token.access_token,
		id_token: token.id_token,
		expires_at,
		refresh_token: token.refresh_token,
		refresh_token_expires_at,
		..session
	})
}

/// Locate any prior session bound to the same upstream identity, to preserve
/// one session and its `user_id` association per identity.
async fn existing_identity_session(
	services: &Services,
	unique_id: &str,
) -> Result<(Option<OwnedUserId>, Option<String>)> {
	match services
		.oauth
		.sessions
		.get_by_unique_id(unique_id)
		.await
	{
		| Ok(session) => Ok((session.user_id, session.sess_id)),
		| Err(error) if !error.is_not_found() => Err(error),
		| Err(_) => Ok((None, None)),
	}
}

fn chain_next_idp_url(
	services: &Services,
	provider: &Provider,
	session: &Session,
	idp_id: &str,
) -> Option<Url> {
	services
		.config
		.identity_provider
		.values()
		.filter(|idp| idp.default || services.config.single_sso)
		.skip_while(|idp| idp.id() != idp_id)
		.nth(1)
		.map(IdentityProvider::id)
		.and_then(|next_idp| {
			provider.callback_url.clone().map(|mut url| {
				let path = format!("/_matrix/client/v3/login/sso/redirect/{next_idp}");
				url.set_path(&path);

				if let Some(redirect_url) = session.redirect_url.as_ref() {
					url.query_pairs_mut()
						.append_pair("redirectUrl", redirect_url.as_str());
				}

				url
			})
		})
}

fn finalize_login_redirect(
	services: &Services,
	session: &Session,
	next_idp_url: Option<Url>,
	user_id: &UserId,
) -> Result<String> {
	let login_token = utils::random_string(TOKEN_LENGTH);
	let _login_token_expires_in = services
		.users
		.create_login_token(user_id, &login_token);

	let location = next_idp_url
		.or_else(|| session.redirect_url.clone())
		.ok_or_else(|| err!(Request(InvalidParam("Missing redirect URL in session data"))))?
		.query_pairs_mut()
		.append_pair("loginToken", &login_token)
		.finish()
		.to_string();

	Ok(location)
}

async fn complete_sso_session(
	services: &Services,
	provider: &Provider,
	mut session: Session,
	userinfo: UserInfo,
) -> Result<(OwnedUserId, Session)> {
	let sess_id = session
		.sess_id
		.clone()
		.ok_or_else(|| err!(Request(InvalidParam("Missing SSO session id."))))?;
	let unique_id = unique_id_sub((provider, &userinfo.sub))?;

	// Check for an existing session from this identity. We want to maintain one
	// session for each identity and keep the newer one which has up-to-date state
	// and access.
	let (old_user_id, old_sess_id) = existing_identity_session(services, &unique_id).await?;

	session.user_info = Some(userinfo.clone());

	// Keep the user_id from the old session as best as possible.
	let user_id = match (session.user_id.take(), old_user_id) {
		| (Some(user_id), ..) | (None, Some(user_id)) => user_id,
		| (None, None) => decide_user_id(services, provider, &userinfo, &unique_id).await?,
	};

	session.user_id = Some(user_id.clone());

	// Attempt to register a non-existing user.
	if !services.users.exists(&user_id).await {
		if !provider.registration {
			return Err!(Request(Forbidden("Registration from this provider is disabled")));
		}

		register_user(services, provider, &session, &userinfo, &user_id).await?;
	}

	// Commit the updated session.
	services.oauth.sessions.put(&session).await;
	if services
		.users
		.maybe_repair_legacy_sso_origin(&user_id)
		.await
	{
		info!("Repaired legacy SSO-origin metadata for {user_id}");
	}

	// Delete any old session.
	if let Some(old_sess_id) = old_sess_id
		.as_deref()
		.filter(is_not_equal_to!(&sess_id))
	{
		services.oauth.sessions.delete(old_sess_id).await;
	}

	if services
		.users
		.maybe_reactivate_deactivated_sso(&user_id)
		.await?
	{
		info!("Reactivated deactivated SSO account {user_id}");
	}

	if !services.users.is_active_local(&user_id).await {
		return Err!(Request(UserDeactivated("This user has been deactivated.")));
	}

	Ok((user_id, session))
}

async fn handle_uiaa(
	services: &Services,
	user_id: &UserId,
	cookie: Cow<'static, str>,
	redirect_url: &Url,
) -> Result<sso_callback::unstable::Response> {
	let uiaa_session_id = redirect_url.path();

	// Find the UIAA session by its ID. SECURITY: Ensure the user authenticating via
	// SSO is the owner of the UIAA session
	let (user_id, device_id, mut uiaainfo) = services
		.uiaa
		.get_uiaa_session_by_session_id(uiaa_session_id)
		.await
		.filter(|(db_user_id, ..)| user_id.eq(db_user_id))
		.ok_or_else(|| err!(Request(Forbidden("UIAA session not found."))))?;

	// MSC4312 m.oauth flow → mark OAuth.
	let has_oauth_flow = uiaainfo
		.flows
		.iter()
		.any(|f| f.stages.contains(&AuthType::OAuth));

	// Mark the completed step based on the UIAA session's flow.
	if has_oauth_flow && !uiaainfo.completed.contains(&AuthType::OAuth) {
		// Grant 10-minute bypass for cross-signing key replacement (like Synapse).
		services
			.users
			.allow_cross_signing_replacement(&user_id);

		uiaainfo.completed.push(AuthType::OAuth);
	}

	// Legacy m.login.sso flow → mark Sso.
	let has_sso_flow = uiaainfo
		.flows
		.iter()
		.any(|f| f.stages.contains(&AuthType::Sso));

	if has_sso_flow && !uiaainfo.completed.contains(&AuthType::Sso) {
		uiaainfo.completed.push(AuthType::Sso);
	}

	services
		.uiaa
		.update_uiaa_session(&user_id, &device_id, uiaa_session_id, Some(&uiaainfo));

	// Redirect back to the fallback page to render the success HTML
	let location =
		format!("/_matrix/client/v3/auth/m.login.sso/fallback/web?session={uiaa_session_id}");

	Ok(sso_callback::unstable::Response { location, cookie: Some(cookie) })
}

#[tracing::instrument(
	name = "register",
	level = INFO_SPAN_LEVEL,
	skip_all,
	fields(user_id, userinfo)
)]
async fn register_user(
	services: &Services,
	provider: &Provider,
	session: &Session,
	userinfo: &UserInfo,
	user_id: &UserId,
) -> Result {
	debug_info!(%user_id, "Creating new user account...");

	services
		.users
		.full_register(Register {
			user_id: Some(user_id),
			password: Some(PASSWORD_SENTINEL),
			origin: Some("sso"),
			displayname: userinfo.name.as_deref(),
			grant_first_user_admin: true,
			..Default::default()
		})
		.await?;

	if let Some(avatar_url) = userinfo
		.avatar_url
		.as_deref()
		.or(userinfo.picture.as_deref())
	{
		set_avatar(services, provider, session, userinfo, user_id, avatar_url)
			.await
			.ok();
	}

	let idp_id = provider.id();
	let idp_name = provider
		.name
		.as_deref()
		.unwrap_or(provider.brand.as_str());

	// log in conduit admin channel if a non-guest user registered
	let notice =
		format!("New user \"{user_id}\" registered on this server via {idp_name} ({idp_id})");

	info!("{notice}");
	if services.server.config.admin_room_notices {
		services.admin.notice(&notice).await;
	}

	Ok(())
}

#[tracing::instrument(level = "debug", skip_all, fields(user_id, avatar_url))]
async fn set_avatar(
	services: &Services,
	_provider: &Provider,
	_session: &Session,
	_userinfo: &UserInfo,
	user_id: &UserId,
	avatar_url: &str,
) -> Result {
	use reqwest::Response;

	let response = services
		.client
		.default
		.get(avatar_url)
		.send()
		.await
		.and_then(Response::error_for_status)?;

	let content_type = response
		.headers()
		.get(CONTENT_TYPE)
		.map(HeaderValue::to_str)
		.flat_ok()
		.map(ToOwned::to_owned);

	let mxc = Mxc {
		server_name: services.globals.server_name(),
		media_id: &utils::random_string(MXC_LENGTH),
	};

	let content_disposition = make_content_disposition(None, content_type.as_deref(), None);
	let limit = services.server.config.max_response_size;
	let bytes = read_response_capped(response, limit).await?;
	services
		.media
		.create(&mxc, Some(user_id), Some(&content_disposition), content_type.as_deref(), &bytes)
		.await?;

	let all_joined_rooms: Vec<OwnedRoomId> = services
		.state_cache
		.rooms_joined(user_id)
		.map(ToOwned::to_owned)
		.collect()
		.await;

	let mxc_uri: OwnedMxcUri = mxc.to_string().into();
	services
		.users
		.update_avatar_url(
			user_id,
			Some(&mxc_uri),
			None,
			&all_joined_rooms,
			propagation_default(
				services
					.server
					.config
					.preserve_room_profile_overrides,
			),
		)
		.await;

	Ok(())
}

#[tracing::instrument(
	level = "debug",
	ret(level = "debug")
	skip_all,
	fields(user),
)]
async fn decide_user_id(
	services: &Services,
	provider: &Provider,
	userinfo: &UserInfo,
	unique_id: &str,
) -> Result<OwnedUserId> {
	if let Some(user_id) = services
		.oauth
		.sessions
		.find_user_association_pending(provider.id(), userinfo)
	{
		debug_info!(
			provider = ?provider.id(),
			?user_id,
			?userinfo,
			"Matched pending association"
		);

		return Ok(user_id);
	}

	let explicit = |claim: &str| provider.userid_claims.contains(claim);

	let allowed = |claim: &str| provider.userid_claims.is_empty() || explicit(claim);

	let choices = [
		explicit("sub")
			.then_some(userinfo.sub.as_str())
			.map(str::to_lowercase),
		userinfo
			.preferred_username
			.as_deref()
			.map(str::to_lowercase)
			.filter(|_| allowed("preferred_username")),
		userinfo
			.username
			.as_deref()
			.map(str::to_lowercase)
			.filter(|_| allowed("username")),
		userinfo
			.nickname
			.as_deref()
			.map(str::to_lowercase)
			.filter(|_| allowed("nickname")),
		provider
			.brand
			.eq(&"github")
			.then_some(userinfo.sub.as_str())
			.map(str::to_lowercase)
			.filter(|_| allowed("login")),
		userinfo
			.email
			.as_deref()
			.and_then(|email| email.split_once('@'))
			.map(at!(0))
			.map(str::to_lowercase)
			.filter(|_| allowed("email")),
	];

	for choice in choices.into_iter().flatten() {
		if let Some(user_id) = try_user_id(services, provider, &choice, false).await {
			return Ok(user_id);
		}
	}

	let length = Some(15..23);
	let unique_id = truncate_deterministic(unique_id, length).to_lowercase();
	if let Some(user_id) = try_user_id(services, provider, &unique_id, true).await {
		return Ok(user_id);
	}

	Err!(Request(UserInUse("User ID is not available.")))
}

#[tracing::instrument(level = "debug", skip_all, fields(username))]
async fn try_user_id(
	services: &Services,
	provider: &Provider,
	username: &str,
	unique_id: bool,
) -> Option<OwnedUserId> {
	let server_name = services.globals.server_name();
	let user_id = parse_user_id(server_name, username)
		.inspect_err(|e| warn!(?username, "Username invalid: {e}"))
		.ok()?;

	if services
		.config
		.forbidden_usernames
		.is_match(username)
	{
		warn!(?username, "Username forbidden.");
		return None;
	}

	if services.users.exists(&user_id).await {
		if provider.trusted {
			info!(
				?username,
				provider = ?provider.brand,
				"Authorizing trusted provider access to existing account."
			);

			return Some(user_id);
		}

		if services
			.users
			.origin(&user_id)
			.await
			.ok()
			.is_none_or(|origin| origin != "sso")
		{
			debug_warn!(?username, "Existing username has non-sso origin.");
			return None;
		}

		if !unique_id {
			debug_warn!(?username, "Username exists.");
			return None;
		}
	} else if unique_id && !provider.unique_id_fallbacks {
		debug_warn!(
			?username,
			provider = ?provider.brand,
			"Unique ID fallbacks disabled.",
		);

		return None;
	}

	Some(user_id)
}

fn parse_user_id(server_name: &ServerName, username: &str) -> Result<OwnedUserId> {
	match UserId::parse_with_server_name(username, server_name) {
		| Err(e) => {
			Err!(Request(InvalidUsername(debug_error!("Username {username} is not valid: {e}"))))
		},
		| Ok(user_id) => match user_id.validate_strict() {
			| Ok(()) => Ok(user_id),
			| Err(e) => Err!(Request(InvalidUsername(debug_error!(
				"Username {username} contains disallowed characters or spaces: {e}"
			)))),
		},
	}
}

#[cfg(test)]
mod tests {
	use std::{
		collections::{BTreeMap, BTreeSet},
		sync::atomic::{AtomicUsize, Ordering},
	};

	use serde_json::json;

	use super::*;

	fn apple_provider_with_native_clients(native_client_ids: &[&str]) -> Provider {
		Provider {
			brand: "appleoidc".to_owned(),
			client_id: "chat.mindroom.matrix.apple".to_owned(),
			client_secret: None,
			client_secret_file: None,
			issuer_url: Some(
				"https://appleid.apple.com"
					.parse()
					.expect("issuer URL"),
			),
			callback_url: None,
			default: false,
			name: Some("Apple".to_owned()),
			icon: None,
			scope: BTreeSet::new(),
			userid_claims: BTreeSet::new(),
			trusted: false,
			unique_id_fallbacks: true,
			registration: true,
			base_path: None,
			discovery_url: None,
			authorization_url: None,
			token_url: None,
			revocation_url: None,
			introspection_url: None,
			userinfo_url: None,
			discovery: true,
			grant_session_duration: Some(300),
			check_cookie: true,
			forward_action_prompt: false,
			extra_authorization_parameters: BTreeMap::new(),
			native_client_ids: native_client_ids
				.iter()
				.map(ToString::to_string)
				.collect::<BTreeSet<_>>(),
		}
	}

	fn apple_claims(audience: &str) -> AppleIdTokenClaims {
		AppleIdTokenClaims {
			iss: "https://appleid.apple.com".to_owned(),
			aud: audience.to_owned(),
			exp: 4_102_444_800,
			sub: "apple-user-123".to_owned(),
			email: Some("alice@example.com".to_owned()),
			name: None,
			given_name: None,
			family_name: None,
			nonce: Some(sha256_hex("native-nonce")),
		}
	}

	fn apple_session_with_claims(claims: &serde_json::Value) -> Session {
		let payload = b64.encode(serde_json::to_vec(claims).expect("serialize claims"));

		Session {
			id_token: Some(format!("header.{payload}.signature")),
			..Default::default()
		}
	}

	fn apple_test_jwk(kid: &str) -> AppleJwk {
		AppleJwk {
			alg: Some("RS256".to_owned()),
			e: "AQAB".to_owned(),
			kid: kid.to_owned(),
			kty: "RSA".to_owned(),
			n: "yRE6rHuNR0QbHO3H3Kt2pOKGVhQqGZXInOduQNxXzuKlvQTLUTv4l4sggh5_CYYi_cvI-SXVT9kPWSKXxJXBXd_4LkvcPuUakBoAkfh-eiFVMh2VrUyWyj3MFl0HTVF9KwRXLAcwkREiS3npThHRyIxuy0ZMeZfxVL5arMhw1SRELB8HoGfG_AtH89BIE9jDBHZ9dLelK9a184zAf8LwoPLxvJb3Il5nncqPcSfKDDodMFBIMc4lQzDKL5gvmiXLXB1AGLm8KBjfE8s3L5xqi-yUod-j8MtvIj812dkS4QMiRVN_by2h3ZY8LYVGrqZXZTcgn2ujn8uKjXLZVD5TdQ".to_owned(),
		}
	}

	fn apple_test_jwks(kids: &[&str]) -> AppleJwks {
		AppleJwks {
			keys: kids
				.iter()
				.map(|kid| apple_test_jwk(kid))
				.collect(),
		}
	}

	#[test]
	fn apple_decoding_key_lookup_allows_refresh_when_kid_is_unknown() {
		let cached_jwks = apple_test_jwks(&["cached-key"]);
		let fresh_jwks = apple_test_jwks(&["rotated-key"]);

		assert!(
			apple_decoding_key_for_kid("rotated-key", &cached_jwks)
				.expect("unknown kid should not be a hard failure before refresh")
				.is_none()
		);
		assert!(
			apple_decoding_key_for_kid("rotated-key", &fresh_jwks)
				.expect("refreshed JWKS should resolve rotated kid")
				.is_some()
		);
	}

	#[tokio::test]
	async fn refresh_apple_jwks_reuses_cache_when_waited_refresh_contains_kid() {
		let cache = RwLock::new(Some(CachedAppleJwks {
			jwks: apple_test_jwks(&["rotated-key"]),
			fetched_at: Instant::now(),
		}));
		let fetches = AtomicUsize::new(0);

		let jwks = refresh_apple_jwks_from_cache(&cache, "rotated-key", async {
			fetches.fetch_add(1, Ordering::SeqCst);
			Ok(apple_test_jwks(&["unused-network-key"]))
		})
		.await
		.expect("cached key should be reused without fetching");

		assert!(apple_jwks_contains_kid(&jwks, "rotated-key"));
		assert_eq!(fetches.load(Ordering::SeqCst), 0);
	}

	#[tokio::test]
	async fn refresh_apple_jwks_fetches_when_locked_cache_still_misses_kid() {
		let cache = RwLock::new(Some(CachedAppleJwks {
			jwks: apple_test_jwks(&["cached-key"]),
			fetched_at: Instant::now(),
		}));
		let fetches = AtomicUsize::new(0);

		let jwks = refresh_apple_jwks_from_cache(&cache, "rotated-key", async {
			fetches.fetch_add(1, Ordering::SeqCst);
			Ok(apple_test_jwks(&["rotated-key"]))
		})
		.await
		.expect("missing key should trigger one refresh");

		assert!(apple_jwks_contains_kid(&jwks, "rotated-key"));
		assert_eq!(fetches.load(Ordering::SeqCst), 1);

		let cached = cache.read().await;
		let cached = cached
			.as_ref()
			.expect("refreshed JWKS should be cached");
		assert!(apple_jwks_contains_kid(&cached.jwks, "rotated-key"));
	}

	#[test]
	fn decode_apple_userinfo_from_id_token_extracts_expected_claims() {
		let session = apple_session_with_claims(&json!({
			"sub": "apple-user-123",
			"email": "alice@example.com",
			"name": "Alice Example",
			"given_name": "Alice",
			"family_name": "Example"
		}));

		let userinfo =
			decode_apple_userinfo_from_id_token(&session).expect("decode Apple id_token claims");

		assert_eq!(userinfo.sub, "apple-user-123");
		assert_eq!(userinfo.email.as_deref(), Some("alice@example.com"));
		assert_eq!(userinfo.preferred_username.as_deref(), Some("alice"));
		assert_eq!(userinfo.username.as_deref(), Some("alice"));
		assert_eq!(userinfo.name.as_deref(), Some("Alice Example"));
		assert_eq!(userinfo.given_name.as_deref(), Some("Alice"));
		assert_eq!(userinfo.family_name.as_deref(), Some("Example"));
	}

	#[test]
	fn decode_apple_userinfo_from_id_token_requires_sub_claim() {
		let session = apple_session_with_claims(&json!({
			"email": "alice@example.com"
		}));

		let error = decode_apple_userinfo_from_id_token(&session)
			.expect_err("missing sub claim should fail");

		let message = format!("{error}");
		assert!(message.contains("sub claim"), "unexpected error: {message}");
	}

	#[test]
	fn decode_apple_userinfo_from_id_token_requires_id_token() {
		let session = Session::default();

		let error = decode_apple_userinfo_from_id_token(&session)
			.expect_err("missing id_token should fail");

		let message = format!("{error}");
		assert!(message.contains("Missing Apple id_token"), "unexpected error: {message}");
	}

	#[test]
	fn decode_apple_userinfo_from_id_token_rejects_invalid_payload() {
		let session = Session {
			id_token: Some("header.!.signature".to_owned()),
			..Default::default()
		};

		let error = decode_apple_userinfo_from_id_token(&session)
			.expect_err("invalid id_token payload should fail");

		let message = format!("{error}");
		assert!(message.contains("invalid base64"), "unexpected error: {message}");
	}

	#[test]
	fn native_apple_claims_accept_configured_bundle_audience() {
		let provider = apple_provider_with_native_clients(&["chat.mindroom.app"]);
		let claims = apple_claims("chat.mindroom.app");

		let userinfo =
			apple_userinfo_from_validated_claims(&provider, claims, Some("native-nonce"))
				.expect("configured native bundle audience should be accepted");

		assert_eq!(userinfo.sub, "apple-user-123");
		assert_eq!(userinfo.email.as_deref(), Some("alice@example.com"));
		assert_eq!(userinfo.preferred_username.as_deref(), Some("alice"));
	}

	#[test]
	fn native_apple_claims_accept_web_services_audience_for_compatibility() {
		let provider = apple_provider_with_native_clients(&[]);
		let claims = apple_claims("chat.mindroom.matrix.apple");

		apple_userinfo_from_validated_claims(&provider, claims, Some("native-nonce"))
			.expect("provider client_id audience should remain accepted");
	}

	#[test]
	fn native_apple_claims_reject_unconfigured_audience() {
		let provider = apple_provider_with_native_clients(&[]);
		let claims = apple_claims("chat.mindroom.app");

		let error = apple_userinfo_from_validated_claims(&provider, claims, Some("native-nonce"))
			.expect_err("unconfigured native bundle audience should be rejected");

		let message = format!("{error}");
		assert!(message.contains("audience"), "unexpected error: {message}");
	}

	#[test]
	fn native_apple_claims_reject_wrong_issuer() {
		let provider = apple_provider_with_native_clients(&["chat.mindroom.app"]);
		let mut claims = apple_claims("chat.mindroom.app");
		claims.iss = "https://example.com".to_owned();

		let error = apple_userinfo_from_validated_claims(&provider, claims, Some("native-nonce"))
			.expect_err("wrong issuer should be rejected");

		let message = format!("{error}");
		assert!(message.contains("issuer"), "unexpected error: {message}");
	}

	#[test]
	fn native_apple_claims_reject_nonce_mismatch() {
		let provider = apple_provider_with_native_clients(&["chat.mindroom.app"]);
		let claims = apple_claims("chat.mindroom.app");

		let error =
			apple_userinfo_from_validated_claims(&provider, claims, Some("different-nonce"))
				.expect_err("nonce mismatch should be rejected");

		let message = format!("{error}");
		assert!(message.contains("nonce"), "unexpected error: {message}");
	}

	#[test]
	fn native_apple_claims_reject_token_nonce_without_request_nonce() {
		let provider = apple_provider_with_native_clients(&["chat.mindroom.app"]);
		let claims = apple_claims("chat.mindroom.app");

		let error = apple_userinfo_from_validated_claims(&provider, claims, None)
			.expect_err("token nonce without request nonce should be rejected");

		let message = format!("{error}");
		assert!(message.contains("nonce"), "unexpected error: {message}");
	}

	#[test]
	fn native_apple_claims_reject_request_nonce_without_token_nonce() {
		let provider = apple_provider_with_native_clients(&["chat.mindroom.app"]);
		let mut claims = apple_claims("chat.mindroom.app");
		claims.nonce = None;

		let error = apple_userinfo_from_validated_claims(&provider, claims, Some("native-nonce"))
			.expect_err("request nonce without token nonce should be rejected");

		let message = format!("{error}");
		assert!(message.contains("nonce"), "unexpected error: {message}");
	}

	#[test]
	fn native_apple_provider_id_uses_explicit_provider_id() {
		let providers =
			[("apple".to_owned(), apple_provider_with_native_clients(&["chat.mindroom.app"]))]
				.into();

		assert_eq!(
			native_apple_provider_id(Some("chat.mindroom.matrix.apple"), &providers)
				.expect("explicit provider id should pass through"),
			"chat.mindroom.matrix.apple"
		);
	}

	#[test]
	fn native_apple_provider_id_falls_back_to_single_apple_provider() {
		let providers =
			[("apple".to_owned(), apple_provider_with_native_clients(&["chat.mindroom.app"]))]
				.into();

		assert_eq!(
			native_apple_provider_id(None, &providers)
				.expect("single Apple provider should be selected"),
			"chat.mindroom.matrix.apple"
		);
	}

	#[test]
	fn native_apple_provider_id_rejects_missing_apple_provider() {
		let mut provider = apple_provider_with_native_clients(&["chat.mindroom.app"]);
		provider.brand = "google".to_owned();
		let providers = [("google".to_owned(), provider)].into();

		let error = native_apple_provider_id(None, &providers)
			.expect_err("missing Apple provider should fail");

		let message = format!("{error}");
		assert!(message.contains("AppleOIDC"), "unexpected error: {message}");
	}

	#[test]
	fn native_apple_provider_id_rejects_ambiguous_apple_providers() {
		let mut second_provider = apple_provider_with_native_clients(&["chat.mindroom.dev"]);
		second_provider.client_id = "chat.mindroom.matrix.apple.dev".to_owned();
		let providers = [
			("apple".to_owned(), apple_provider_with_native_clients(&["chat.mindroom.app"])),
			("apple-dev".to_owned(), second_provider),
		]
		.into();

		let error = native_apple_provider_id(None, &providers)
			.expect_err("multiple Apple providers should require provider_id");

		let message = format!("{error}");
		assert!(message.contains("provider_id"), "unexpected error: {message}");
	}
}
