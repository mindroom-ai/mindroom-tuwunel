//! Planning for in-place user ID rewrites: matching, map classes and the
//! decision for each database record.

use ruma::{ServerName, UserId};
use serde::Serialize;
use tuwunel_core::{Err, Result};

/// How a database map is treated when one local user ID is renamed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(super) enum MapClass {
	/// Identity and index data: replace the ID in keys and values.
	RawReplace,
	/// The renamed user's own session or device state: delete matching
	/// records, matching in key or value.
	Clear,
	/// Signed key material: delete only records keyed by the renamed user; a
	/// value-only match is another user's record and blocks the rewrite.
	ClearOwnKeysOnly,
	/// Historical events: replace only with an explicit opt-in.
	EventGraph,
	/// Not reviewed for this Tuwunel version: any match blocks the rewrite.
	Unknown,
}

#[derive(Debug)]
pub(super) enum RecordAction {
	Skip,
	Rewrite {
		key: Vec<u8>,
		value: Vec<u8>,
	},
	Clear,
	Block(&'static str),
}

pub(super) fn map_class(map: &str) -> MapClass {
	match map {
		| "alias_roomid"
		| "alias_userid"
		| "aliasid_alias"
		| "mediaid_file"
		| "mediaid_user"
		| "presenceid_presence"
		| "profilechangeid_userid"
		| "publicroomids"
		| "readreceiptid_readreceipt"
		| "roomid_invitedcount"
		| "roomid_joinedcount"
		| "roomid_knockedcount"
		| "roomid_shortroomid"
		| "roomid_shortstatehash"
		| "roomserverids"
		| "roomuserdataid_accountdata"
		| "roomuserid_invitecount"
		| "roomuserid_joined"
		| "roomuserid_knockedcount"
		| "roomuserid_lastnotificationread"
		| "roomuserid_lastprivatereadupdate"
		| "roomuserid_leftcount"
		| "roomuserid_privateread"
		| "roomuserid_privatereadsync"
		| "roomusertype_roomuserdataid"
		| "roomuseroncejoinedids"
		| "serverroomids"
		| "shortstatekey_statekey"
		| "statekey_shortstatekey"
		| "threadid_userids"
		| "userid_avatarurl"
		| "userid_displayname"
		| "userid_origin"
		| "userid_password"
		| "userid_presenceid"
		| "useridcount_notification"
		| "useridprofilekey_value"
		| "userroomid_highlightcount"
		| "userroomid_invitestate"
		| "userroomid_joined"
		| "userroomid_knockedstate"
		| "userroomid_leftstate"
		| "userroomid_notificationcount" => MapClass::RawReplace,

		| "keychangeid_devicechange"
		| "keychangeid_userid"
		| "lazyloadedids"
		| "logintoken_expiresatuserid"
		| "oauthid_session"
		| "onetimekeyid4225_otk"
		| "onetimekeyid_onetimekeys"
		| "openidtoken_expiresatuserid"
		| "pushkey_deviceid"
		| "senderkey_pusher"
		| "spentrefresh_userdeviceid"
		| "todeviceid_events"
		| "token_userdeviceid"
		| "userdeviceconnid_conn"
		| "userdeviceid_metadata"
		| "userdeviceid_refresh"
		| "userdeviceid_spentrefresh"
		| "userdeviceid_token"
		| "userdeviceidalgorithm_fallback"
		| "userdeviceidtoken_index"
		| "userdevicesessionid_uiaainfo"
		| "userdevicetxnid_response"
		| "userfilterid_filter"
		| "userid_dehydrateddevice"
		| "userid_devicelistversion"
		| "userid_lastonetimekeyupdate"
		| "userid_masterkeyid"
		| "userid_oauthid"
		| "userid_selfsigningkeyid"
		| "userid_usersigningkeyid" => MapClass::Clear,

		| "backupid_algorithm" | "backupid_etag" | "backupkeyid_backup" | "keyid_key" =>
			MapClass::ClearOwnKeysOnly,

		| "eventid_originalpdu"
		| "eventid_outlierpdu"
		| "pduid_pdu"
		| "referencedevents"
		| "roomid_pduleaves" => MapClass::EventGraph,

		| _ => MapClass::Unknown,
	}
}

/// Offsets of `user_id` in `haystack` where the next byte cannot continue a
/// server name, so `@a:example.org` never matches inside `@a:example.org.uk`
/// or `@a:example.org:8448`.
pub(super) fn find_user_id(haystack: &[u8], user_id: &[u8]) -> Vec<usize> {
	if user_id.is_empty() || haystack.len() < user_id.len() {
		return Vec::new();
	}

	(0..=haystack.len() - user_id.len())
		.filter(|&i| haystack[i..].starts_with(user_id))
		.filter(|&i| {
			haystack
				.get(i + user_id.len())
				.is_none_or(|&byte| !continues_server_name(byte))
		})
		.collect()
}

fn continues_server_name(byte: u8) -> bool {
	byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':')
}

pub(super) fn replace_user_id(input: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
	let mut out = Vec::with_capacity(input.len());
	let mut copied = 0;
	for offset in find_user_id(input, from) {
		out.extend_from_slice(&input[copied..offset]);
		out.extend_from_slice(to);
		copied = offset + from.len();
	}
	out.extend_from_slice(&input[copied..]);
	out
}

/// Occurrences of the user's localpart outside bounded full-ID matches. They
/// are reported, never rewritten.
pub(super) fn localpart_only_hits(haystack: &[u8], user_id: &[u8]) -> usize {
	let localpart = localpart(user_id);
	if localpart.is_empty() || haystack.len() < localpart.len() {
		return 0;
	}

	let full = find_user_id(haystack, user_id);
	(0..=haystack.len() - localpart.len())
		.filter(|&i| haystack[i..].starts_with(localpart))
		.filter(|&i| i == 0 || full.binary_search(&(i - 1)).is_err())
		.count()
}

/// Occurrences of `%40<localpart>%3A<server>`, as in `matrix.to` links. They
/// are reported, never rewritten.
pub(super) fn url_encoded_hits(haystack: &[u8], user_id: &[u8]) -> usize {
	let localpart = localpart(user_id);
	let server = user_id
		.get(localpart.len().saturating_add(2)..)
		.unwrap_or_default();
	let encoded = [b"%40".as_slice(), localpart, b"%3a", server].concat();
	if haystack.len() < encoded.len() {
		return 0;
	}

	haystack
		.windows(encoded.len())
		.filter(|window| window.eq_ignore_ascii_case(&encoded))
		.count()
}

fn localpart(user_id: &[u8]) -> &[u8] {
	let end = user_id
		.iter()
		.position(|&byte| byte == b':')
		.unwrap_or(user_id.len());

	user_id.get(1..end).unwrap_or_default()
}

pub(super) fn plan_record(
	class: MapClass,
	key: &[u8],
	value: &[u8],
	from: &[u8],
	to: &[u8],
	rewrite_event_graph: bool,
) -> RecordAction {
	let key_match = !find_user_id(key, from).is_empty();
	if !key_match && find_user_id(value, from).is_empty() {
		return RecordAction::Skip;
	}

	let rewrite = || RecordAction::Rewrite {
		key: replace_user_id(key, from, to),
		value: replace_user_id(value, from, to),
	};
	match class {
		| MapClass::RawReplace => rewrite(),
		| MapClass::Clear => RecordAction::Clear,
		| MapClass::ClearOwnKeysOnly if key_match => RecordAction::Clear,
		| MapClass::ClearOwnKeysOnly => RecordAction::Block("value-only match in own-keys map"),
		| MapClass::EventGraph if rewrite_event_graph => rewrite(),
		| MapClass::EventGraph => RecordAction::Block("event graph needs --rewrite-event-graph"),
		| MapClass::Unknown => RecordAction::Block("unclassified map"),
	}
}

pub(super) fn validate_ids(from: &UserId, to: &UserId, server_name: &ServerName) -> Result {
	if from == to {
		return Err!("source and target user IDs must differ");
	}
	if from.server_name() != server_name || to.server_name() != server_name {
		return Err!("both user IDs must be local to {server_name}");
	}

	Ok(())
}

#[cfg(test)]
mod tests {
	use ruma::{ServerName, UserId};

	use super::{
		MapClass, RecordAction, find_user_id, localpart_only_hits, map_class, plan_record,
		replace_user_id, url_encoded_hits, validate_ids,
	};

	#[test]
	fn match_requires_server_name_boundary() {
		let id = b"@a:example.org";
		assert_eq!(find_user_id(b"x@a:example.org\xffy", id), vec![1]);
		assert_eq!(find_user_id(b"\"@a:example.org\"", id), vec![1]);
		assert!(find_user_id(b"@a:example.org.uk", id).is_empty());
		assert!(find_user_id(b"@a:example.org:8448", id).is_empty());
		assert!(find_user_id(b"@a:example.org-x", id).is_empty());
		assert!(find_user_id(b"@ab:example.org", id).is_empty());
	}

	#[test]
	fn replaces_every_bounded_occurrence_when_lengths_differ() {
		let out = replace_user_id(
			b"@a:example.org\xff@a:example.org.uk\xff@a:example.org",
			b"@a:example.org",
			b"@bbb:example.org",
		);
		assert_eq!(out, b"@bbb:example.org\xff@a:example.org.uk\xff@bbb:example.org".to_vec());
	}

	#[test]
	fn own_keys_map_blocks_value_only_match() {
		let action = plan_record(
			MapClass::ClearOwnKeysOnly,
			b"@bob:example.org\xffmaster",
			br#"{"signatures":{"@a:example.org":{}}}"#,
			b"@a:example.org",
			b"@b:example.org",
			true,
		);
		assert!(matches!(action, RecordAction::Block(_)));
		let action = plan_record(
			MapClass::ClearOwnKeysOnly,
			b"@a:example.org\xffmaster",
			b"{}",
			b"@a:example.org",
			b"@b:example.org",
			true,
		);
		assert!(matches!(action, RecordAction::Clear));
	}

	#[test]
	fn clear_map_clears_value_only_match() {
		let action = plan_record(
			MapClass::Clear,
			b"token123",
			b"@a:example.org\xffDEVICE",
			b"@a:example.org",
			b"@b:example.org",
			false,
		);
		assert!(matches!(action, RecordAction::Clear));
	}

	#[test]
	fn unknown_map_with_match_blocks() {
		assert_eq!(map_class("some_future_map"), MapClass::Unknown);
		let action = plan_record(
			MapClass::Unknown,
			b"k",
			b"@a:example.org",
			b"@a:example.org",
			b"@b:example.org",
			true,
		);
		assert!(matches!(action, RecordAction::Block(_)));
	}

	#[test]
	fn event_graph_needs_flag() {
		let pdu = br#"{"sender":"@a:example.org"}"#;
		assert!(matches!(
			plan_record(
				MapClass::EventGraph,
				b"1",
				pdu,
				b"@a:example.org",
				b"@b:example.org",
				false
			),
			RecordAction::Block(_)
		));
		assert!(matches!(
			plan_record(MapClass::EventGraph, b"1", pdu, b"@a:example.org", b"@b:example.org", true),
			RecordAction::Rewrite { value, .. } if value == br#"{"sender":"@b:example.org"}"#.to_vec()
		));
	}

	#[test]
	fn records_without_match_are_skipped() {
		assert!(matches!(
			plan_record(
				MapClass::Unknown,
				b"k",
				b"v",
				b"@a:example.org",
				b"@b:example.org",
				false
			),
			RecordAction::Skip
		));
	}

	#[test]
	fn rejects_same_or_remote_ids() {
		let server: &ServerName = "example.org".try_into().unwrap();
		let a: &UserId = "@a:example.org".try_into().unwrap();
		let b: &UserId = "@b:example.org".try_into().unwrap();
		let remote: &UserId = "@b:other.org".try_into().unwrap();
		assert!(validate_ids(a, b, server).is_ok());
		assert!(validate_ids(a, a, server).is_err());
		assert!(validate_ids(a, remote, server).is_err());
	}

	#[test]
	fn classifies_known_maps() {
		assert_eq!(map_class("userid_password"), MapClass::RawReplace);
		assert_eq!(map_class("statekey_shortstatekey"), MapClass::RawReplace);
		assert_eq!(map_class("token_userdeviceid"), MapClass::Clear);
		assert_eq!(map_class("keyid_key"), MapClass::ClearOwnKeysOnly);
		assert_eq!(map_class("pduid_pdu"), MapClass::EventGraph);
		assert_eq!(map_class("eventid_outlierpdu"), MapClass::EventGraph);
	}

	#[test]
	fn counts_localpart_hits_outside_full_ids() {
		let id = b"@a_b:example.org";
		assert_eq!(localpart_only_hits(b"@a_b:example.org a_b x", id), 1);
		assert_eq!(localpart_only_hits(b"@a_b:example.org", id), 0);
		assert_eq!(localpart_only_hits(b"@a_b:example.org.uk", id), 1);
	}

	#[test]
	fn counts_url_encoded_ids_case_insensitively() {
		let id = b"@a_b:example.org";
		assert_eq!(url_encoded_hits(b"https://matrix.to/#/%40a_b%3Aexample.org", id), 1);
		assert_eq!(url_encoded_hits(b"%40a_b%3aexample.org and %40A_B%3AEXAMPLE.ORG", id), 2);
		assert_eq!(url_encoded_hits(b"@a_b:example.org", id), 0);
	}
}
