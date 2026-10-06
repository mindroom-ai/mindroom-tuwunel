use std::str::FromStr;

use ruma::{
	RoomId, UInt,
	api::federation::space::SpaceHierarchyParentSummary,
	events::room::member::{MembershipState, RoomMemberEventContent},
	owned_room_id, owned_server_name,
	room::{JoinRuleSummary, RoomSummary},
	room_id, user_id,
};
use tuwunel_core::{Result, config::Figment, matrix::PduCount};

use crate::{
	rooms::{
		spaces::{PaginationToken, get_parent_children_via},
		state_cache::MembershipUpdate,
	},
	test_utils::fixture,
};

#[test]
fn get_summary_children() {
	let summary: SpaceHierarchyParentSummary = SpaceHierarchyParentSummary {
		summary: RoomSummary::new(
			owned_room_id!("!root:example.org"),
			JoinRuleSummary::Public,
			true,
			UInt::from(1_u32),
			true,
		),
		children_state: vec![
			serde_json::from_str(
				r#"{
                      "content": {
                        "via": [
                          "example.org"
                        ],
                        "suggested": false
                      },
                      "origin_server_ts": 1629413349153,
                      "sender": "@alice:example.org",
                      "state_key": "!foo:example.org",
                      "type": "m.space.child"
                    }"#,
			)
			.unwrap(),
			serde_json::from_str(
				r#"{
                      "content": {
                        "via": [
                          "example.org"
                        ],
                        "suggested": true
                      },
                      "origin_server_ts": 1629413349157,
                      "sender": "@alice:example.org",
                      "state_key": "!bar:example.org",
                      "type": "m.space.child"
                    }"#,
			)
			.unwrap(),
			serde_json::from_str(
				r#"{
                      "content": {
                        "via": [
                          "example.org"
                        ]
                      },
                      "origin_server_ts": 1629413349160,
                      "sender": "@alice:example.org",
                      "state_key": "!baz:example.org",
                      "type": "m.space.child"
                    }"#,
			)
			.unwrap(),
		],
	};

	assert_eq!(
		get_parent_children_via(&summary, false)
			.map(|(k, v)| (k, v.collect::<Vec<_>>()))
			.collect::<Vec<_>>(),
		vec![
			(owned_room_id!("!foo:example.org"), vec![owned_server_name!("example.org")]),
			(owned_room_id!("!bar:example.org"), vec![owned_server_name!("example.org")]),
			(owned_room_id!("!baz:example.org"), vec![owned_server_name!("example.org")])
		]
	);
	assert_eq!(
		get_parent_children_via(&summary, true)
			.map(|(k, v)| (k, v.collect::<Vec<_>>()))
			.collect::<Vec<_>>(),
		vec![(owned_room_id!("!bar:example.org"), vec![owned_server_name!("example.org")])]
	);
}

#[test]
fn invalid_pagination_tokens() {
	fn token_is_err(token: &str) { PaginationToken::from_str(token).unwrap_err(); }

	token_is_err("231_2_noabool");
	token_is_err("");
	token_is_err("111_3_");
	token_is_err("foo_not_int");
	token_is_err("11_4_true_");
	token_is_err("___");
	token_is_err("__false");
}

#[test]
fn valid_pagination_tokens() {
	assert_eq!(
		PaginationToken {
			short_room_ids: vec![5383, 42934, 283, 423],
			limit: UInt::from(20_u32),
			max_depth: UInt::from(1_u32),
			suggested_only: true
		},
		PaginationToken::from_str("5383,42934,283,423_20_1_true").unwrap()
	);

	assert_eq!(
		PaginationToken {
			short_room_ids: vec![740],
			limit: UInt::from(97_u32),
			max_depth: UInt::from(10539_u32),
			suggested_only: false
		},
		PaginationToken::from_str("740_97_10539_false").unwrap()
	);
}

#[test]
fn pagination_token_to_string() {
	assert_eq!(
		PaginationToken {
			short_room_ids: vec![740],
			limit: UInt::from(97_u32),
			max_depth: UInt::from(10539_u32),
			suggested_only: false
		}
		.to_string(),
		"740_97_10539_false"
	);

	assert_eq!(
		PaginationToken {
			short_room_ids: vec![9, 34],
			limit: UInt::from(3_u32),
			max_depth: UInt::from(1_u32),
			suggested_only: true
		}
		.to_string(),
		"9,34_3_1_true"
	);
}

/// A remote hierarchy answer does not cache summaries of rooms this server is
/// in, which come from local state; its other rooms are still cached.
#[tokio::test]
async fn remote_children_skip_resident_rooms() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let local = room_id!("!local:localhost");
	let remote = room_id!("!remote:remote.invalid");
	let alice = user_id!("@alice:localhost");

	services
		.state_cache
		.update_membership(MembershipUpdate {
			room_id: local,
			user_id: alice,
			membership_event: RoomMemberEventContent::new(MembershipState::Join),
			sender: alice,
			last_state: None,
			invite_via: None,
			update_joined_count: true,
			count: PduCount::Normal(1),
		})
		.await?;

	let summary = |room_id: &RoomId, name: &str| {
		let mut summary = RoomSummary::new(
			room_id.to_owned(),
			JoinRuleSummary::Public,
			false,
			UInt::from(1_u32),
			false,
		);
		summary.name = Some(name.to_owned());
		summary
	};

	services
		.spaces
		.cache_children(vec![summary(local, "Forged"), summary(remote, "Remote")], Vec::new())
		.await;

	assert!(
		services
			.spaces
			.cache_get(local)
			.await
			.is_err_and(|error| error.is_not_found())
	);

	let cached = services.spaces.cache_get(remote).await?;
	let name = cached
		.summary
		.and_then(|parent| parent.summary.name);
	assert_eq!(name.as_deref(), Some("Remote"));

	Ok(())
}
