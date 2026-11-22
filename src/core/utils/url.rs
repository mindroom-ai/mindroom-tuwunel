//! Hostname and URL matching utilities.
//!
//! These helpers provide reusable matching rules that the URL parser does not
//! expose directly.

use std::{
	fmt::{self, Display, Write},
	net::{Ipv4Addr, Ipv6Addr},
};

use http::Uri;
use url::{Host, Url};

use super::string::is_unreserved_byte;

/// Percent-encodes a string for use in a URL component.
///
/// ASCII letters, digits and unreserved punctuation remain unchanged.
/// Other UTF-8 bytes are encoded with uppercase hexadecimal digits.
#[must_use]
pub fn url_encode(s: &str) -> String {
	s.bytes()
		.fold(String::with_capacity(s.len()), |mut out, b| {
			if is_unreserved_byte(b) {
				out.push(b.into());
			} else {
				write!(&mut out, "%{b:02X}").ok();
			}

			out
		})
}

/// Whether a redirect URI matches an entry in `allowed`.
///
/// Entries take two distinct forms so hosts and private-use URI schemes never
/// alias each other:
///
/// - A bare host entry (for example `element.io`, `localhost`, `127.0.0.1`, or
///   `::1` / `[::1]`) matches only an `https` URI with that exact host, or an
///   `http` URI whose host is loopback ([RFC 8252
///   §7.3](https://www.rfc-editor.org/rfc/rfc8252#section-7.3)).
/// - A colon-suffixed scheme entry (for example `io.element.android:`) matches
///   only a hostless private-use URI with no authority whose scheme contains a
///   dot ([RFC 8252 §7.1](https://www.rfc-editor.org/rfc/rfc8252#section-7.1),
///   as in `io.element.android:/callback`).
///
/// Comparisons ignore ASCII case. Unparsable URIs and URIs carrying userinfo
/// never match.
#[must_use]
pub fn redirect_allowlisted(allowed: &[String], uri: &str) -> bool {
	let Ok(url) = Url::parse(uri) else {
		return false;
	};

	if !url.username().is_empty() || url.password().is_some() {
		return false;
	}

	match url.host() {
		| Some(host) => {
			let transport_allowed =
				url.scheme() == "https" || (url.scheme() == "http" && is_loopback(&url));

			transport_allowed
				&& allowed
					.iter()
					.any(|entry| host_entry_matches(entry, &host))
		},
		| None => {
			if url.has_authority() || !url.scheme().contains('.') {
				return false;
			}

			allowed.iter().any(|entry| {
				entry
					.strip_suffix(':')
					.is_some_and(|scheme| scheme.eq_ignore_ascii_case(url.scheme()))
			})
		},
	}
}

fn host_entry_matches(entry: &str, host: &Host<&str>) -> bool {
	match host {
		| Host::Domain(domain) => entry.eq_ignore_ascii_case(domain),
		| Host::Ipv4(ip) => entry
			.parse::<Ipv4Addr>()
			.is_ok_and(|addr| addr == *ip),
		| Host::Ipv6(ip) => {
			let raw = entry
				.strip_prefix('[')
				.and_then(|s| s.strip_suffix(']'))
				.unwrap_or(entry);

			raw.parse::<Ipv6Addr>()
				.is_ok_and(|addr| addr == *ip)
		},
	}
}

/// Reports whether a hostname is equal to or beneath a domain name.
///
/// Matching is ASCII case-insensitive and accepts a domain with an optional
/// leading dot. A suffix only matches at a DNS label boundary. A single dot
/// matches only a hostname with a trailing dot.
#[must_use]
pub fn hostname_matches_domain(hostname: &str, domain: &str) -> bool {
	if domain == "." {
		return hostname.ends_with('.');
	}

	let domain = domain.strip_prefix('.').unwrap_or(domain);

	if domain.is_empty() {
		return false;
	}

	if hostname.eq_ignore_ascii_case(domain) {
		return true;
	}

	let Some(separator) = hostname
		.len()
		.checked_sub(domain.len())
		.and_then(|index| index.checked_sub(1))
	else {
		return false;
	};

	let Some(suffix_start) = separator.checked_add(1) else {
		return false;
	};

	hostname.as_bytes().get(separator) == Some(&b'.')
		&& hostname
			.get(suffix_start..)
			.is_some_and(|suffix| suffix.eq_ignore_ascii_case(domain))
}

/// Whether a URL's host names the loopback interface.
///
/// That is any IPv4 or IPv6 loopback address (RFC 8252 §7.3) or the domain
/// `localhost` (§8.3), compared without regard to case.
#[must_use]
pub fn is_loopback(url: &Url) -> bool {
	match url.host() {
		| Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
		| Some(Host::Ipv4(ip)) => ip.is_loopback(),
		| Some(Host::Ipv6(ip)) => ip.is_loopback(),
		| None => false,
	}
}

/// Formats a request URI without exposing sensitive components.
///
/// Query values are always replaced. A matched route template can replace
/// concrete path parameters when the caller has one.
pub struct SanitizedUri<'a> {
	uri: &'a Uri,
	path: Option<&'a str>,
}

impl<'a> SanitizedUri<'a> {
	/// Creates a redacted display wrapper for a request URI.
	///
	/// The original path is retained, while any query value is replaced.
	#[must_use]
	pub const fn new(uri: &'a Uri) -> Self { Self { uri, path: None } }

	/// Uses a route template instead of concrete path parameters.
	///
	/// Query values remain replaced when the URI contains a query.
	#[must_use]
	pub const fn with_path(uri: &'a Uri, path: &'a str) -> Self { Self { uri, path: Some(path) } }
}

impl Display for SanitizedUri<'_> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let path = self.path.unwrap_or_else(|| self.uri.path());

		match self.uri.query() {
			| Some(_) => write!(f, "{path}?<redacted>"),
			| None => f.write_str(path),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn redacts_query() {
		let uri: Uri = "/_matrix/client/v3/sync?access_token=syt_realtoken"
			.parse()
			.expect("valid uri");

		let out = SanitizedUri::new(&uri).to_string();

		assert_eq!(out, "/_matrix/client/v3/sync?<redacted>");
		assert!(!out.contains("access_token"));
		assert!(!out.contains("syt_realtoken"));
	}

	#[test]
	fn preserves_path_without_query() {
		let uri: Uri = "/_matrix/client/v3/sync"
			.parse()
			.expect("valid uri");

		assert_eq!(SanitizedUri::new(&uri).to_string(), "/_matrix/client/v3/sync");
	}

	#[test]
	fn replaces_concrete_path_parameters() {
		let uri = Uri::builder()
			.path_and_query(
				"/_matrix/client/unstable/org.matrix.msc4140/delayed_events/secret?action=send",
			)
			.build()
			.expect("valid uri");

		let out = SanitizedUri::with_path(
			&uri,
			"/_matrix/client/unstable/org.matrix.msc4140/delayed_events/{delay_id}",
		)
		.to_string();

		assert_eq!(
			out,
			"/_matrix/client/unstable/org.matrix.msc4140/delayed_events/{delay_id}?<redacted>"
		);
		assert!(!out.contains("secret"));
	}

	#[test]
	fn redirect_allowlisted_separates_hosts_and_private_use_schemes() {
		let allowed = [
			"element.io".to_owned(),
			"io.element.android:".to_owned(),
			"localhost".to_owned(),
			"127.0.0.1".to_owned(),
			"::1".to_owned(),
		];

		// HTTPS hosts match bare entries case-insensitively.
		assert!(redirect_allowlisted(&allowed, "https://element.io/oauth/ios/x"));
		assert!(redirect_allowlisted(&["Element.IO".to_owned()], "https://element.io/cb"));

		// Loopback hosts allow http and https (with any port) and both IPv6 forms.
		assert!(redirect_allowlisted(&allowed, "http://localhost:8080/cb"));
		assert!(redirect_allowlisted(&allowed, "http://127.0.0.1:49152/cb"));
		assert!(redirect_allowlisted(&allowed, "http://[::1]:8080/cb"));
		assert!(redirect_allowlisted(&allowed, "https://[::1]/cb"));
		assert!(redirect_allowlisted(&["[::1]".to_owned()], "http://[::1]:8080/cb"));

		// Colon-suffixed entries match dotted hostless private-use schemes only.
		assert!(redirect_allowlisted(&allowed, "io.element.android:/callback"));
		assert!(redirect_allowlisted(
			&["IO.Element.Android:".to_owned()],
			"io.element.android:/callback"
		));

		// Bare host entries must never match private-use schemes, non-loopback http,
		// or arbitrary schemes carrying that host.
		assert!(!redirect_allowlisted(&allowed, "element.io:/callback"));
		assert!(!redirect_allowlisted(&allowed, "http://element.io/callback"));
		assert!(!redirect_allowlisted(&allowed, "evil://element.io/callback"));
		assert!(!redirect_allowlisted(&allowed, "evil://localhost/callback"));
		assert!(!redirect_allowlisted(&allowed, "https://user@element.io/callback"));
		assert!(!redirect_allowlisted(&allowed, "http://user:pass@localhost/callback"));

		// Colon-suffixed scheme entries must never match HTTPS hosts or URIs with an
		// authority, and undotted schemes never match.
		assert!(!redirect_allowlisted(&allowed, "https://io.element.android/callback"));
		assert!(!redirect_allowlisted(&allowed, "io.element.android://callback"));
		assert!(!redirect_allowlisted(&allowed, "io.element.android:///callback"));
		assert!(!redirect_allowlisted(&["element:".to_owned()], "element:/callback"));
		assert!(!redirect_allowlisted(&["http:".to_owned()], "http:/callback"));
		assert!(!redirect_allowlisted(&["https:".to_owned()], "https:/callback"));

		// Subdomains, empty lists, and malformed URIs never match.
		assert!(!redirect_allowlisted(&allowed, "https://app.element.io/cb"));
		assert!(!redirect_allowlisted(&[], "https://element.io/cb"));
		assert!(!redirect_allowlisted(&allowed, "not a uri"));
	}
}
