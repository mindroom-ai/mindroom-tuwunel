use ruma::{api::client::sync::sync_events::v5::request::ListConfig, room_id};
use serde_json::{Value, from_value, json};

use super::{Connection, Request, check};

fn list(mut body: Value) -> Request {
	let mut request = Request::new();
	if body["ranges"].is_null() {
		body["ranges"] = json!([]);
	}
	body["timeline_limit"] = json!(0);
	request
		.lists
		.insert("main".into(), from_value(body).expect("valid list"));
	request
}

fn selectors(n: usize) -> Vec<Value> {
	(0..n)
		.map(|i| json!(["m.room.member", i.to_string()]))
		.collect()
}

#[test]
fn list_count_includes_retained_ids_but_not_replacements() {
	let mut request = Request::new();
	for i in 0..64 {
		request
			.lists
			.insert(i.to_string().as_str().into(), Default::default());
	}
	let mut conn = Connection::default();
	conn.update_cache(&request);
	check(&request, Some(&conn)).expect("updates fit a full connection");
	let one = list(json!({}));
	check(&one, None).expect("one list fits an empty connection");
	assert!(check(&one, Some(&conn)).is_err());
	request.lists.extend(one.lists);
	assert!(check(&request, None).is_err());
}

#[test]
fn list_fields_accept_the_boundary_and_reject_the_next_entry() {
	for n in [16, 17] {
		let tags: Vec<_> = (0..n).map(|i| format!("u.tag{i}")).collect();
		let spaces: Vec<_> = (0..n)
			.map(|i| format!("!space{i}:example.com"))
			.collect();
		for (field, entries) in [
			("spaces", &spaces),
			("tags", &tags),
			("not_tags", &tags),
			("room_types", &tags),
			("not_room_types", &tags),
		] {
			assert_eq!(
				check(&list(json!({"filters": { (field): entries }})), None).is_ok(),
				n == 16,
				"{field}"
			);
		}
		let ranges: Vec<_> = (0..n).map(|i| [i, i]).collect();
		assert_eq!(check(&list(json!({"ranges": ranges})), None).is_ok(), n == 16);
	}
	let mut request = Request::new();
	for (id, valid) in [("é".repeat(32), true), (format!("{}x", "é".repeat(32)), false)] {
		request.lists.clear();
		request
			.lists
			.insert(id.as_str().into(), Default::default());
		assert_eq!(check(&request, None).is_ok(), valid);
	}
}

#[test]
fn required_state_accounts_for_sticky_lists_and_each_subscription() {
	let keys = selectors(256);
	let at_limit = list(json!({"required_state": keys}));
	check(&at_limit, None).expect("256 selectors fit");
	assert!(check(&list(json!({"required_state": selectors(257)})), None).is_err());
	let mut conn = Connection::default();
	conn.update_cache(&at_limit);
	let room = room_id!("!sub:example.com").to_owned();
	let mut request = list(json!({"required_state": []})); // An empty selection inherits.
	request.room_subscriptions.insert(
		room.clone(),
		from_value(json!({"required_state": keys, "timeline_limit": 0})).expect("valid config"),
	);
	check(&request, Some(&conn)).expect("shared selectors count once");
	request
		.room_subscriptions
		.get_mut(&room)
		.expect("subscription")
		.required_state = from_value(json!([["m.room.name", ""]])).expect("valid selectors");
	assert!(check(&request, Some(&conn)).is_err());
	// Replacing the list selection frees the old keys.
	request.lists = list(json!({"required_state": [["m.room.topic", ""]]})).lists;
	check(&request, Some(&conn)).expect("replacement releases prior selectors");
	request
		.room_subscriptions
		.get_mut(&room)
		.expect("subscription")
		.required_state = from_value(json!(selectors(257))).expect("valid selectors");
	assert!(check(&request, None).is_err());
}

#[test]
fn overlapping_list_budgets_do_not_double_count_and_subscriptions_are_separate() {
	let mut request = list(json!({"required_state": selectors(256)}));
	let mut conn = Connection::default();
	conn.update_cache(&request);
	let mut second = request
		.lists
		.pop_first()
		.map(|(_, list)| list)
		.expect("main list");
	request
		.lists
		.insert("second".into(), second.clone());
	check(&request, Some(&conn)).expect("overlapping list selections fit");
	second.room_details.required_state =
		from_value(json!([["m.room.name", ""]])).expect("selector");
	request.lists.insert("second".into(), second);
	assert!(check(&request, Some(&conn)).is_err());

	let mut separate = Request::new();
	for room in [room_id!("!a:example.com"), room_id!("!b:example.com")] {
		let keys: Vec<_> = (0..256)
			.map(|i| json!(["m.room.member", format!("{room}{i}")]))
			.collect();
		separate.room_subscriptions.insert(
			room.to_owned(),
			from_value(json!({"required_state": keys, "timeline_limit": 0}))
				.expect("subscription"),
		);
	}
	check(&separate, None).expect("subscriptions are independent");
}

#[test]
fn resumption_checks_inherited_scopes_but_allows_replacing_them() {
	let mut old = Request::new();
	old.extensions =
		from_value(json!({"typing": {"lists": ["overlong".repeat(10)]}})).expect("valid config");
	let mut conn = Connection::default();
	conn.update_cache(&old);
	let mut request = Request::new();
	assert!(check(&request, Some(&conn)).is_err());
	request.extensions = from_value(json!({"typing": {"lists": []}})).expect("valid config");
	check(&request, Some(&conn)).expect("an empty scope replaces the invalid one");
}

#[test]
fn extension_scopes_have_the_same_limits() {
	for extension in ["account_data", "receipts", "typing", "org.matrix.msc4262.profiles"] {
		let config = |lists: Vec<String>, rooms: usize| {
			let mut request = Request::new();
			request.extensions = from_value(
				json!({(extension): {"lists": lists, "rooms": vec!["!a:example.com"; rooms]}}),
			)
			.expect("valid config");
			request
		};
		let ids = vec!["x".repeat(64); 64];
		check(&config(ids.clone(), 256), None).expect("extension scope at both limits");
		assert!(check(&config(ids, 257), None).is_err(), "{extension}: rooms");
		assert!(check(&config(vec!["x".into(); 65], 0), None).is_err(), "{extension}: lists");
		assert!(check(&config(vec!["x".repeat(65)], 0), None).is_err(), "{extension}: list ID");
	}
}

#[test]
fn repeated_entries_count_once() {
	let tags = vec!["u.tag"; 17];
	for field in ["spaces", "tags", "not_tags", "room_types", "not_room_types"] {
		let entries = if field == "spaces" {
			vec!["!space:example.com"; 17]
		} else {
			tags.clone()
		};
		check(&list(json!({"filters": { (field): entries }})), None).expect(field);
	}
	check(&list(json!({"ranges": vec![[0, 9]; 17]})), None).expect("repeated ranges");
}

/// Lists whose filters no one room can match are budgeted apart, on open and
/// on resume, while lists that can share a room still share one budget.
#[test]
fn required_state_budgets_lists_that_can_share_a_room() {
	let named = |prefix: &str, n: usize| -> Vec<Value> {
		(0..n)
			.map(|i| json!(["m.room.member", format!("{prefix}{i}")]))
			.collect()
	};
	let lists = |lists: Vec<(Value, Vec<Value>)>| {
		let mut request = Request::new();
		for (i, (filters, keys)) in lists.into_iter().enumerate() {
			let list = json!({
				"ranges": [], "timeline_limit": 0, "filters": filters, "required_state": keys,
			});
			request
				.lists
				.insert(i.to_string().as_str().into(), from_value(list).expect("valid list"));
		}
		request
	};
	let dm = json!({"is_dm": true});
	let not_dm = json!({"is_dm": false});
	let favourite = json!({"tags": ["m.favourite"]});
	let not_favourite = json!({"not_tags": ["m.favourite"]});
	let accepted = [
		vec![(dm.clone(), named("d", 256)), (not_dm.clone(), named("n", 1))],
		vec![
			(json!({}), named("u", 100)),
			(dm.clone(), named("d", 156)),
			(not_dm.clone(), named("n", 156)),
		],
		vec![(favourite.clone(), named("f", 256)), (not_favourite, named("n", 256))],
	];
	for request in accepted.map(lists) {
		check(&request, None).expect("exclusive lists fit on open");
		let mut conn = Connection::default();
		conn.update_cache(&request);
		check(&Request::new(), Some(&conn)).expect("exclusive lists fit on resume");
	}

	let rejected = [
		vec![(dm.clone(), named("d", 256)), (json!({}), named("u", 1))],
		vec![(dm.clone(), named("d", 200)), (favourite, named("f", 57))],
		vec![(not_dm.clone(), named("n", 256)), (json!({"is_invite": true}), named("i", 1))],
	];
	for request in rejected.map(lists) {
		assert!(check(&request, None).is_err());
		let mut conn = Connection::default();
		conn.update_cache(&request);
		assert!(check(&Request::new(), Some(&conn)).is_err());
	}

	// A subscription is checked against each group its room might be in.
	let room = room_id!("!sub:example.com").to_owned();
	for (n, fits) in [(56, true), (57, false)] {
		let mut request =
			lists(vec![(dm.clone(), named("d", 200)), (not_dm.clone(), named("n", 200))]);
		let subscription = json!({"required_state": named("s", n), "timeline_limit": 0});
		request
			.room_subscriptions
			.insert(room.clone(), from_value(subscription).expect("subscription"));
		assert_eq!(check(&request, None).is_ok(), fits);
	}
}

/// Subscriptions with independent selectors are counted on their own, and a
/// subscription still counts the group selectors it does not repeat.
#[test]
fn subscriptions_count_their_own_selectors_against_each_group() {
	let named = |prefix: String, n: usize| -> Vec<Value> {
		(0..n)
			.map(|i| json!(["m.room.member", format!("{prefix}{i}")]))
			.collect()
	};
	let mut request = Request::new();
	for room in 0..512 {
		let subscription =
			json!({"required_state": named(format!("s{room}x"), 256), "timeline_limit": 0});
		request.room_subscriptions.insert(
			format!("!r{room}:example.com")
				.try_into()
				.expect("room ID"),
			from_value(subscription).expect("subscription"),
		);
	}
	check(&request, None).expect("independent subscriptions fit");

	let mut request = list(json!({"required_state": named("l".into(), 200)}));
	let shared = named("l".into(), 200)
		.into_iter()
		.chain(named("s".into(), 56));
	let room = room_id!("!sub:example.com").to_owned();
	let subscribe = |keys: Vec<Value>| -> ListConfig {
		from_value(json!({"required_state": keys, "timeline_limit": 0})).expect("subscription")
	};
	request
		.room_subscriptions
		.insert(room.clone(), subscribe(shared.collect()));
	check(&request, None).expect("repeated group selectors count once");
	request
		.room_subscriptions
		.insert(room, subscribe(named("s".into(), 57)));
	assert!(check(&request, None).is_err());
}
