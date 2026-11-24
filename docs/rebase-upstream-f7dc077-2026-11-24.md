# Rebase on upstream main `f7dc0770b` - 2026-11-24

## Inputs and scope

The starting fork main was `fdae72d2ac8d69febf83b80c5a4852b685823e09`
(`v2.0.0-mindroom.14`), 120 commits on upstream `v2.0.0`
(`a3feae1a26ec764ee88606c2c68b659a9b5fba8d`). The new base is upstream `main`
at `f7dc0770b5c492fcc8f8de855a8c4094f67d6b7b` ("core, matrix, service: Box
stack-only firewalls under debug assertions only.", 2026-11-23), not a release
tag; it is 218 commits past `v2.0.0`, which is its ancestor.

`origin/main` was re-fetched before writing this record and remains at
`fdae72d2ac8d69febf83b80c5a4852b685823e09` (fork main did not move during the
rebase). Open fork PR #150 (`pdu-size-without-unsigned`) is out of scope.

The starting fork main `fdae72d2a` (`v2.0.0-mindroom.14`) is preserved locally
as `backup/main-fdae72d2a-before-upstream-f7dc077-20261124`. No upstream commit
or published release tag is rewritten. The workspace version stays `2.0.0`, so
the release tooling previews `v2.0.0-mindroom.15`.

The result is 103 commits on `f7dc0770b`: 102 of the 120 fork commits carried
over (18 dropped as owned by upstream, 4 reduced to remainders), plus this
documentation commit.

## What upstream changed

The 218 upstream commits merge 11 of the fork's upstream PRs, add upstream's
own replacements for 5 more fork fixes plus a backport, and bring several
structural changes that meet fork code:

- **MSC4354 sticky events and MSC4480 sliding sync `sticky_events`**
  (`443a6dd30`): adds `sticky` fields on `PduEvent`, sticky unindexing on
  redaction, delayed sticky `org.matrix.msc4143.rtc.member` events, and a new
  `sticky_events` sliding sync extension scope.
- **Unfolded serving and stored-aggregation stripping** (`0dbe24238`,
  `563c670b5`, `7752984d2`): adds `served_unfolded_to` / `without_aggregations`
  on `/initialSync`, `/notifications`, `/events`, `/state`, `/search`, state
  sections, appservice transactions, and thread `latest_event`; adds
  `thread_summary_search_limit` and serves a sender their own `latest_event`
  with private fields while failing closed on reload failure.
- **Level-walked recursive `/relations`** (`4121ace12`, `cf063b67b`,
  `609143eb3`): excludes redacted events in `requester_relations` and switches
  recursive `/relations` to a 3-level `walk_level` (`RECURSION_DEPTH = 3`) that
  stops recursing through non-matching events.
- **`/sync` history visibility and departed-member state** (`7a2f1b8a5`,
  `718926f3b`, `ffb64e5d2`, `d2ec814fc`, `1c0069ac1`, `eb388c4e4`,
  `350e5a9df`, `d77ce8c05`): scan-budgeted `TimelineWindow` on sync timelines
  keeping a user's own leave/ban, fallible `departed_after`/`pdu_position`, and
  departure-anchored never-joined left-room state.
- **`Eval`-based federation event pipeline and unified provenance**
  (`78fddef7d`, `de7f47eda`, `daebf5a72`, `3b8fe4141`, `75cd3bd8a`..`03a78c7d7`,
  `1861ea127`, `abfb0d644`, `fe2ff1201`): unified local-join-sender and create
  provenance checks in `provenance.rs`, `send_join` authorization in
  `ingest_send_join_events`, and `/state_ids` + local-build room checks.
- **SSO callback path move and username validation** (`6e7510b88`,
  `f44d80137`..`1409d97dd`): moves the default SSO callback path to
  `/_tuwunel/sso/callback/{idp_id}` (keeping the legacy unstable path mounted
  until 3.0 when `callback_url` names it or when `callback_url` and
  `well_known.client` are unset) and moves username checks to
  `services.oauth.candidate_user_id`.
- **Federation proxy routing, LiveKit delegation, and auth** (`c40e8de15`,
  `b0ddca99c`, `d6af5010a`, `90bf04255`, `c6da9a11e`, `2a7fa2360`): adds
  `Root::Federation` in `router/proxy.rs`, requires auth on `/rtc/transports`,
  threads `execute_counted` / `execute_allow_self` in the federation fetcher,
  and filters server-key origins in `server_keys/get.rs`.
- **Search `SmallVec` `TokenId`, account-data split, and Rust 1.100 toolchain**
  (`db49225a9`, `d7a7985a4`, `251250f65`, `de1008cfc`, `c798ac4c8`, `flake.nix`
  / `flake.lock`): upgrades the locked Fenix toolchain to Rust 1.100 (`cargo
  1.100.0`, `rustfmt 1.12.0-nightly 6905905bb7 2026-11-19`), which enables new
  pedantic clippy lints (`clippy::assert_is_empty`,
  `closure-returning-async-block`, `clippy::future_not_send`).

## Outcome per fork commit

| Outcome | Count | Commits |
| --- | ---: | --- |
| Dropped, owned by upstream | 18 | Table below |
| Reduced to remainder | 4 | `#45` (`7119bc350`), `#119` (`9b4523f74`), `#129` (`3f03ac936`), `#183` (`a306c83bf`) |
| Adapted, with `Rebase note (upstream main f7dc0770b):` | 47 | Table below (incl. CI owner `06dd1fbbc`) |
| Carried with changed diff context only | 11 | harness `567099158`, native Apple `a12c10575`, e2ee `d81d2a72b`, `#28`, `#79`, `#93`, `#147`, `#149`, `#153`, `#167`, `#180` |
| Carried unchanged (`=` in `git range-diff`) | 40 | The other 40 fork commits |
| New in this rebase | 1 | This documentation commit |

### Dropped (18) — owned by upstream `f7dc0770b`

| Old SHA | Fork PR / Subject | Upstream owner commit(s) | Notes |
| --- | --- | --- | --- |
| `3f42d6e7d` | #55 sync: apply history visibility to sync timelines | `7a2f1b8a5`, `718926f3b`, `ffb64e5d2`, `d2ec814fc`, `1c0069ac1`, `eb388c4e4` | Upstream closed #728 and wrote its own scan-budgeted implementation + `sync_history_visibility.rs`. Verbatim fork test passed on tip. |
| `30795c4ce` | #61 sync: send a left room's state only to users who joined it | `350e5a9df` (PR #729 `c04db6cb3`), `d77ce8c05` | Merged upstream and extended with departure anchoring. |
| `3aa9345fc` | #67 event_handler: check the room of the events a PDU's state is taken from | `1861ea127` (PR #726 `f8c41dfc1`), `abfb0d644`, `fe2ff1201` | Exact checks in `fetch_state.rs` and `state_local_build.rs`. Verbatim fork test (incl. held-`/event` and peer `/state_ids`) passed on tip. |
| `2a13915d7` | #68 sync: compare sliding sync required_state selectors without a quadratic scan | `1c9868f4f` (PR #714 `57e9f8c91`) | Identical fingerprint indexing and tests. |
| `1ed9a2e67` | #69 client: bundle /initialSync and /notifications events for the requester | `0dbe24238` | Upstream strips stored `m.relations` (`served_unfolded_to` / `without_aggregations`); upstream unfolded policy wins (see Rulings). |
| `7a533e29f` | #88 membership: test that a send_join create event keeps its own room id | `0287db5d2` (PR #700 `dc862f0e0`) | Covered by `incoming_create_keeps_its_room_identity` in `src/matrix/pdu/tests.rs`. |
| `e19be4c8f` | #95 membership: authorize the events of a send_join response before storing them | `75cd3bd8a` (PR #723 `d2593f797`), `e998d0203`, `f88b1ad64`, `fde97b5bc`, `03a78c7d7` | Merged upstream with expanded tests in `join/tests/send_join/mod.rs`. |
| `08d84aa6c` | #100 event_handler: refuse a second m.room.create for an existing room | `de7f47eda` (PR #727 `d49f8c4b2`), `3b8fe4141`, `78fddef7d` | Replaced by unified `provenance.rs` checks across join, outlier, and upgrade paths. |
| `a760c0188` | #111 sso: skip usernames in an exclusive appservice namespace | `f44d80137` (PR #721 `5c4eb0477`), `809073b71`, `7d5d21c1b`, `229fa79b6`, `1409d97dd` | Moved to `services.oauth.candidate_user_id` with exclusive namespace, localpart `forbidden_usernames`, and foreign server checks. |
| `b8a416fed` | #112 event_handler: walk each event of a /state_ids answer once | `0454388a1` (PR #701 `33fc21542`), `0c8c425df` | `StateIdsResponse.pdu_ids: HashSet<OwnedEventId>` + `fetch_state/tests.rs`. |
| `00684193d` | #120 membership: refuse a local authoriser in the remote restricted join fallback | `8c18beecb` (PR #719 `d25757193`), `6102640df` | `fallback_authoriser_allowed` + tests. |
| `d002526e3` | #114 membership: refuse send_join for an old join of a user who has left | `b3f881c1a` (PR #718 `06fc4af54`), `dbc05ec23`, `0d74c0a5e` | `check_join_result` under room lock + tests. |
| `f2986cbc3` | #116 event_handler: refuse local users' joins sent by others and a second m.room.create | `de7f47eda`, `daebf5a72` (PR #727 `d49f8c4b2`), `3b8fe4141`, `78fddef7d` | Unified in `provenance.rs` (`check_local_join_sender` + `check_create_reference`). |
| `3760d9a6e` | #127 router: refuse a refresh token presented as an access token | `a6ce6d752` (PR #720 `7aba380b7`), `e0e5a2d18`, `7ee25c1f3` | `find_from_access_token` + `find_from_unexpired_token` + tests. |
| `2fcc0dd8e` | #141 relations: leave redacted events out of /relations and its recursive walk | `4121ace12`, `cf063b67b` | `requester_relations` filters `!pdu.is_redacted()` and `walk_level` descends only through matching children. Verbatim fork test passed on tip. |
| `52085ab7f` | #156 auth/uiaa: bind an untagged device's SSO stage to the account's linked provider | `c6e62ed34` (PR #702 `7555a2da8`), `d22cf3150`, `cef3ac100` | Exact completed-session and exact-ID-over-brand logic + tests + docs. |
| `c35b300dc` | tests: poll watch probes outside tokio's cooperative budget | `46aaa55a5` (PR #703 `76afd1389`) | Identical `src/service/test_utils.rs` change. |
| `44575971f` | #188 server_keys: count our own active key as held (upstream backport) | `c798ac4c8` | Upstream commit is now in base. |

### Reduced to remainders (4)

- **`#45` (`7119bc350` ← `46f492c04`) — edit-bundle visibility.** Upstream
  `563c670b5`/`7752984d2` now owns thread summary visibility (`walk_replies` /
  `apply_thread_view`), so `drop_unseen_thread` is dropped. The fork keeps only
  `user_can_see_event` and `erased_for` in `newest_replacement` (matching open
  upstream PR #707) and edit-visibility checks in
  `bundled_relations_after_leave.rs`.
- **`#119` (`9b4523f74` ← `f8a12611c`) — sliding sync range deduplication.**
  Upstream `928a4abd2` (PR #715) and `2726515ec` own `selector.rs` window room
  deduplication; the fork keeps distinct range deduplication in
  `src/service/sync/mod.rs` and `src/service/sync/tests.rs`.
- **`#129` (`3f03ac936` ← `0f6d36f49`) — bounded, deduplicated recursive
  `/relations`.** Built on upstream `cf063b67b`'s 3-level `walk_level`
  structure, restoring the three invariants upstream lacks: (a)
  `PduCount::into_normal` in `relation_keys` for backfilled `from` cursors,
  (b) dual-parent deduplication via `WalkState.seen`, and (c) the 32,768-event
  / 262,144-row walk budget via `Service::requester_relation_rows` (which
  `requester_relations` delegates to) with whole-level drops (`debug_warn!`)
  and depth-0 lazy fallback, plus `relations_recurse_order.rs` (3-level depth)
  and unit tests in `src/api/client/relations.rs` and `pdu_metadata/tests.rs`.
- **`#183` (`a306c83bf` ← `795e185bc`) — stored relations on embedded edits
  and previews.** Upstream `d4a8aae0c`, `0dbe24238`, and `7752984d2` now cover
  join/knock membership templates, `/initialSync`/`/notifications`, and
  `apply_thread_view` / `latest_event`. The fork keeps stripping stored
  `m.relations` on embedded `m.replace` edits (`bundle_aggregations`,
  `bundle_thread_latest_edit`) and `#143` previews (`bundle_thread_previews`)
  using upstream's `served_unfolded_to`/`remove_aggregations`, plus
  `thread_previews.rs` regression assertions.

### Adapted (47)

Each carries a `Rebase note (upstream main f7dc0770b):` in its commit message.

| Final SHA | Commit | Upstream change | Adaptation |
| --- | --- | --- | --- |
| `c60656c75` | Edits owner | `7a2f1b8a5`, `563c670b5`, `443a6dd30`, `7752984d2` | `collapse_superseded_edits` runs on `TimelineWindow.pdus`; `Relations::default` keeps `thread_summary_search_limit` with `bundle_edits: true`; import union in `event/relation.rs`; `sticky: None` on 3 `PduEvent` test literals; `own_latest_reload_failure_drops_the_summary` sets `rooms.relations.bundle_edits = false`. |
| `88a1c3e44` | SSO owner | `d22cf3150`, `443a6dd30`, `7752984d2` | Kept password-origin expectation on expanded `uiaa_password_flows`; passed `DeactivationReason::Admin` in `relations_parent.rs`, `sticky_events/served.rs`, `oidc_completion/browser_session.rs`. |
| `06dd1fbbc` | CI owner | `flake.lock` Fenix bump | Moved `.github/workflows/mindroom-ci.yml` rustfmt pin `nightly-2026-11-03` → `nightly-2026-11-20` (see Formatter). |
| `56dd0ced7` | #36 | `1df6f9374` | Import union in `sso.rs` beside `html::escape_template_value`. |
| `e05f8ea75` | #42 | `78fddef7d` | Placed `parse_fetched_pdu` and tests after `Eval`-based `admit_chain`/`admit_fetched`/`admit_outlier`. |
| `2bd52c803` | #59 | `718926f3b` | Adapted walk to fallible `departed_after` and `pdu_position`; omitted dropped `#55`/`#61` `FORK_CHANGES.md` entries. |
| `09259398a` | #66 | `4f9179228`, signing refactor | Export union in `src/api/client/mod.rs`; migrated `federation_receipt_edu.rs` from removed `ServerSignaturesInput` to `XMatrixSigningInput`. |
| `2cf645c75` | #64 | `78fddef7d` | Adapted `admit_chain`/`admit_fetched`/`admit_outlier` to `Vec<u8>` PDUs parsed inside `admit_outlier` (PR #704). |
| `4ddf70884` | #80 | `90bf04255`, `b0ddca99c` | Threaded `limit` through `execute_counted` and passed `max_response_size` in `livekit/get_token.rs`. |
| `1c6d7d9ca` | #82 | `6a609d254` | Moved edit-redaction loop into `append_redaction_effects`; import union in `bundling.rs`; omitted dropped `#68` entry. |
| `11b643064` | #81 | `718926f3b` | Applied PR #731 retained-departure walk onto `user_can.rs` and updated upstream `user_can` tests. |
| `c49b3a583` | #84 | `de1008cfc` | Kept ruma 0.18 `msc3814` path in `put_dehydrated_device.rs`; omitted dropped `#68` entry. |
| `e4b8c3f28` | #89 | `#68` drop | Docs-conflict only: omitted dropped `#68` `FORK_CHANGES.md` entry. |
| `39f1ac8be` | #91 | `78fddef7d`, `d49f8c4b2` | Threaded `held_bytes` through `Eval`-based pipeline and `provenance/tests.rs`, keeping `with_built_unsigned`. |
| `e28dc9ff4` | #90 | `c6da9a11e`, `90bf04255` | Passed `max_response_size` in `execute_allow_self` and forwarded `limit` through `execute_on_allow_self` → `execute_counted` → `perform`. |
| `ec2b9b7be` | #94 | `78fddef7d` | Import union in `fetch_prev.rs` beside `Eval`. |
| `d1fa9602a` | #96 | `345127053`, `78fddef7d`, `c40e8de15` | Import/export unions in `pdu.rs` and `fetch_prev.rs`; resolved `origin_key` before `parse_json` in `router/proxy.rs` `Root::Federation`. |
| `1c9c6dc86` | #92 | `c798ac4c8` | Merged `stored_signing_keys_stay_bounded` beside `only_the_active_local_key_is_held_without_a_stored_copy`. |
| `e5cfafec9` | #97 | `d2593f797`, `fde97b5bc` | Adapted other-local-join filter to `ingest_send_join_events` (destructuring `state_key: Some(state_key)` without `Default` on `[u8; 48]`) and moved tests to `join/tests/send_join/mod.rs`. |
| `8950eec40` | #98 | `443a6dd30` | Generalized `some_or_sticky_lists` for MSC4480 `sticky_events` string list IDs. |
| `a95019de1` | #99 | `c40e8de15` | Inserted resolved `origin_key` into `request.parts.extensions` in `router/proxy.rs` `Root::Federation` so `auth_server` does not panic. |
| `3006de828` | #103 | `d2593f797`, `f88b1ad64`, `fde97b5bc` | Applied PR #724 retained-local-member tracking onto `join.rs` and `join/tests/send_join/mod.rs`. |
| `37719b724` | #105 | `b0ddca99c` | Chained `io.mindroom.openid_audience` beside LiveKit MSC4195 flag in `versions.rs`. |
| `e68024517` | #110 | `cf063b67b`, `609143eb3` | Applied PR #713 `next_batch` logic onto `relations.rs` and updated `bundle_requester_view.rs`. |
| `97864fdbc` | #125 | `d77ce8c05`, `350e5a9df` | Applied PR #731 `left.rs` and `sync_v5_rescinded_invite.rs`; dropped deleted `sync_left_room_state.rs` hunk. |
| `9ae388c0e` | #124 | `c40e8de15` | Passed upstream `route` argument to `make_args` while keeping anonymous token cap and auth-before-parse order. |
| `00c4612e1` | #117 | `78fddef7d` | Adapted `fetch_prev.rs` fetch closure to `Eval` (retaining `futures::FutureExt`); omitted dropped `#116` hunks from `auth_chain_fetch_budget.rs`. |
| `26b0407ea` | #123 | `443a6dd30`, `6a609d254` | Preserved `sticky.unindex` and `adopts` return in `timeline/redact.rs`; omitted dropped `#120` entry. |
| `02db6dc13` | #128 | `#127` drop | Docs-conflict only: omitted dropped `#127` `FORK_CHANGES.md` entry. |
| `23dfb8a1f` | #130 | Rust 1.100 lint | Changed `ephemeral/tests.rs` empty check to `assert_eq!(..., Vec::<u64>::new())` for `clippy::assert_is_empty`. |
| `f6e6f71b5` | #139 | `443a6dd30` | Kept `install_room_state` beside `read_replaced` doc update in `timeline/append.rs`; omitted dropped `#61`/`#55`/`#95` entries. |
| `a64342939` | #138 | `f44d80137`, `7d5d21c1b` | Kept `candidate_user_id` in `try_user_id`; merged `users/tests.rs` imports; preserved `providers.md` wording; omitted dropped `#111` entry. |
| `46c8eb8c7` | #143 | `7752984d2` | Merged `bundling.rs` imports (dropping redundant `Value` in `mod tests`) and kept upstream `unsigned_of` helper. |
| `84b6f62a6` | 2026-11-08 record | drops | Docs-conflict only: omitted dropped `#67`/`#69`/`#100`/`#114`/`#116` `FORK_CHANGES.md` entries. |
| `e51faf898` | #161 | `6e7510b88` | Replaced upstream's updated `grant_session_cookie_path` tests with `__Host-` `Path=/` tests while keeping `/_tuwunel/sso/callback/...` URLs. |
| `57eccbab7` | #162 | `6e7510b88`, `b0ddca99c` | Merged imports in `sso.rs`/`check.rs`; extracted `check_registration_rules` from `check_identity_providers` (`clippy::too_many_lines`); kept `unique_id_fallbacks` wording. |
| `a829e84f2` | #164 | `251250f65` | Passed `params` beside pre-lowercased filters to `user_minor_details` in `list_users.rs`; placed `DeactivationGuard::Admins` before `end_browser_sessions`. |
| `f608caa2e` | #165 | `6e7510b88`, `#162` helper | Preserved `providers.md` `userid_claims`/`trusted` wording; called `unknown_claim` inside `check_registration_rules`. |
| `cd8c9cd8f` | #166 | `#162` helper | Placed `external_forbidden_rooms` rule check inside extracted `check_registration_rules`. |
| `f10098499` | #169 | `msc.md` totals | Recomputed MSC2477 totals from `f7dc0770b` `msc.md` (299/57/442 → 299/58/441, open 68/24/345 → 68/25/344). |
| `22ce877c0` | delayed scope | `443a6dd30`, `b0ddca99c` | Extended `is_own_call_leave` to allow a sender's own delayed MSC4354 sticky `m.rtc.member`/`org.matrix.msc4143.rtc.member` leave; updated `docs/calls/matrix_rtc.md`, `msc.md`, and 4 upstream boot tests (`livekit_delegation`, `livekit_webhook`, `state_member_invite`, `sticky_events`). |
| `5201dac69` | #177 | `db49225a9`, Rust 1.100 | Kept upstream `SmallVec` imports in `search/mod.rs`; merged `lowercase_expansion_*` and `#177` tests in `search/tests.rs` using `assert_eq!(..., ids(&[]))`. |
| `2c39227df` | #179 | `db49225a9` | Kept upstream `SmallVec` `TokenId` instead of `KeyBuf` in `search/mod.rs`; merged tests in `search/tests.rs`. |
| `bd6c242a6` | #178 | `f44d80137`, `#162` helper | Replaced removed `parse_user_id` in `claimed_account` with `UserId::parse_with_server_name` + `validate_strict`; kept `check_claim_accounts_of_appservice` beside `check_registration_rules`. |
| `863e37d57` | #184 | `d7a7985a4`, `6e7510b88` | Kept `sha1` beside `similar 3.2.1` in `Cargo.lock`; preserved `assert_account_data_owner`, per-route messages, and `account_data/tests.rs` under `require_owner`; kept MSC4502/MSC4512 helpers in `appservice/mod.rs`; updated `sso_fallback_account.rs` to pass `CALLBACK` and unused subject `"riley"`. |
| `5a074995a` | #187 | `url.rs` imports | Merged `borrow::Cow` beside `net::{Ipv4Addr, Ipv6Addr}` in `core/utils/url.rs`. |
| `8fc04089e` | #175 | `443a6dd30`, `2a7fa2360` | Extended sliding sync `limits::check_scopes` and `update_cache_sticky_events` to cover MSC4480 `sticky_events`; kept upstream origin filter & borrowed `extract_key` in `server_keys/get.rs` with wanted-key retention. |

### Changed test expectations

1. **`src/service/rooms/threads/tests/redact.rs`** (`c60656c75`, edits owner):
   `own_latest_reload_failure_drops_the_summary` explicitly sets
   `rooms.relations.bundle_edits = false`, because upstream's test assumes its
   default (`false`), whereas the fork defaults `bundle_edits = true` whose
   edit-fold reload also fails closed on a missing `eventid_pduid`.
2. **`src/main/tests/uiaa_password_flows.rs`** (`88a1c3e44`, SSO owner): keeps
   fork policy expecting password-only UIAA (no SSO stage) for password-origin
   accounts on upstream's expanded flow table (`d22cf3150`).
3. **`src/main/tests/bundled_relations_after_leave.rs`** (`7119bc350`, `#45`):
   asserts upstream `563c670b5`'s behavior — after leaving, `m.thread` still
   includes the visible pre-leave reply (`count: 1`, `"before-leave"`), while
   post-leave replies and `m.replace` edits remain excluded.
4. **`src/service/rooms/state_accessor/user_can/tests.rs` &
   `unreadable_state/mod.rs`** (`11b643064`, `#81` / PR #731): seeds
   predecessor joins where former-member retention is tested and expects
   unreadable departure walks to fail closed.
5. **`src/service/membership/join/tests/send_join/mod.rs`** (`3006de828`,
   `#103` / PR #724): updates `a_send_join_member_event_installs_over_a_local_departure`
   so unanchored remote bans without an auth chain back to the local departure
   keep the local leave.
6. **`src/main/tests/bundle_requester_view.rs`** (`e68024517`, `#110` / PR
   #713): updates `redacted_at` (`?dir=b&limit=1`) to take the first backward
   page before redaction rather than relying on a final-page `next_batch`
   token.
7. **`src/main/tests/sync_v5_rescinded_invite.rs`** (`97864fdbc`, `#125` / PR
   #731): updates `left_history` and `missing_snapshot_keeps_the_notification`
   for unreadable departures and adds `unban_after_ban`; drops the hunk on
   deleted `sync_left_room_state.rs`.
8. **`src/main/tests/relations_recurse_order.rs`** (`3f03ac936`, `#129`):
   adapts `chain` length from `0..4` (`take(3)`) to `0..3` (`take(2)`) to match
   upstream `cf063b67b`'s 3-level `RECURSION_DEPTH = 3`.
9. **`src/api/client/session/sso.rs` unit tests** (`e51faf898`, `#161`):
   replaces upstream's `grant_session_cookie_path` tests with `__Host-`
   `Path=/` tests while keeping `authentik_provider`'s
   `/_tuwunel/sso/callback/...` URL.
10. **`src/main/tests/livekit_delegation.rs` & `livekit_webhook.rs`**
    (`22ce877c0`, delayed scope): updates `Case::schedule` to use the sender's
    own slot state key (`_{user_id}_{member}` / `_{owner_id}_{member}`) so all
    LiveKit cases exercise the full flow under the call-leave-only rule.
11. **`src/main/tests/state_member_invite.rs`** (`22ce877c0`, delayed scope):
    replaces `delayed_invite`, `delayed_refusal`, and `delayed_stalled_invite`
    with `delayed_invite_refused` asserting 403 `M_FORBIDDEN` (keeping direct
    `stalled_invite`).
12. **`src/main/tests/sticky_events.rs`** (`22ce877c0`, delayed scope):
    `refused_durations` expects 403 `M_FORBIDDEN` on delayed `m.room.topic`
    (the own-leave guard runs before the sticky+state check) and adds an
    own-slot delayed `org.matrix.msc3401.call.member` empty state leave with
    `sticky_duration_ms` asserting 400 `M_INVALID_PARAM`; restores upstream's
    positive `delayed_sticky_events_fire` (query & body) and
    `delayed_sticky_leave_fires_by_timer` for `org.matrix.msc4143.rtc.member`
    leaves while asserting 403 for legacy `call.member` messages and
    `rtc.member` joins.
13. **`src/main/tests/sso_fallback_account.rs`** (`863e37d57`, `#184`): uses
    unused subject `"riley"` instead of `"erin"` for the `server_admins` skip
    case because upstream `f7dc0770b` already links `"erin"` in
    `reauthenticate`.

## Policy differences where upstream wins

- **#69 (`/initialSync` and `/notifications` aggregations):** upstream
  `0dbe24238` strips stored aggregations (`served_unfolded_to` /
  `without_aggregations`) rather than calling `bundle_aggregations`. Both stop
  the stored-thread-summary leak, and MindRoom clients/backend do not rely on
  bundled aggregations from `/initialSync` or `/notifications`.
- **#129 (`/relations` recursive walk):** upstream `cf063b67b` owns the
  3-level filter-stops-recursion architecture; the fork layers only walk
  budgets, dual-parent dedup, and backfilled `from` normalization on top.
- **#55 (`/sync` history visibility):** upstream `7a2f1b8a5` adds a scan
  budget (`max(50, 10*limit)`) and `TimelineWindow.resume` while keeping the
  user's own leave/ban.
- **#61 (left room state):** upstream `d77ce8c05` additionally anchors
  never-joined left-room state on the departure.
- **#100 / #116 (event provenance):** upstream `de7f47eda` + `daebf5a72` +
  `3b8fe4141` enforces local-join-sender and create binding uniformly across
  join, outlier, and upgrade paths, plus create references in pre-v12 rooms.
- **#111 (SSO username validation):** upstream `f44d80137` + `7d5d21c1b`
  matches `forbidden_usernames` on localpart and rejects foreign server names
  in `candidate_user_id`.
- **#114 (`send_join` retry membership):** upstream `b3f881c1a` checks
  `try_is_joined` under the room lock (stronger than fork).
- **#127 (refresh token as access token):** upstream `a6ce6d752` rejects
  refresh tokens in both `find_from_access_token` and
  `find_from_unexpired_token` (stronger than fork).
- **MatrixRTC `/rtc/transports` auth (`d6af5010a`):** upstream requires an
  access token; MindRoom Chat already authenticates `/rtc/transports`.
- **SSO callback path (`6e7510b88`):** upstream moves the default callback to
  `/_tuwunel/sso/callback/{idp_id}` and keeps the legacy unstable path mounted
  until 3.0 when configured or when `callback_url` and `well_known.client` are
  unset.

## Rulings

Decisions taken during this rebase. The fork's owner explicitly approved 1, 2,
3, and 5; for 4 (`leave_reason` kept), the conflict with upstream's restored
`sticky_events.rs` test and MSC4143 was reported and the fork's owner proceeded
to Phase 2–4 without requesting its removal (accepted default, not a separate
ruling).

1. **#69 dropped** in favor of upstream `0dbe24238`'s unfolded `/initialSync`
   and `/notifications` serving.
2. **#129 restored as a remainder** (`3f03ac936`) on upstream's 3-level
   `walk_level` for the 32,768-event / 262,144-row walk budget with depth-0
   fallback, dual-parent dedup, and `PduCount::into_normal`.
3. **Delayed MSC4354 sticky `rtc.member` leaves permitted** (`22ce877c0`): a
   client may delay its own MSC4354 sticky `m.rtc.member` /
   `org.matrix.msc4143.rtc.member` leave (requiring `sticky_duration_ms`,
   non-empty `slot_id` and sticky key, `member.membership == "leave"` with
   non-empty `member.id`, own `member.user_id` if present, optional
   `member.device_id` and `leave_reason`, or legacy `{slot_id, sticky_key}`
   shape, no extra fields). Delayed sticky `rtc.member` joins, legacy
   `call.member` messages, and all other delayed events remain 403
   `M_FORBIDDEN`.
4. **`leave_reason` kept** in `is_own_sticky_rtc_leave` (`22ce877c0`), because
   upstream's restored `sticky_events.rs` (`member_content`) sends
   `"leave_reason": { "code": "delayed_leave" }` and MSC4143
   `RtcLeftMembershipData` defines it.
5. **Left-room `timeline.limit=1` ban+unban overshoot (`97864fdbc` / PR #731)
   kept identical to upstream PR #731** rather than diverging in this rebase.

## Formatter

Upstream `f7dc0770b`'s `flake.lock` pins Fenix nightly rustfmt
`rustfmt 1.12.0-nightly (6905905bb7 2026-11-19)` (`2026-11-20` channel,
`cargo 1.100.0`). Both unmodified `f7dc0770b` and all 102 fork commits pass
`cargo fmt --all -- --check` with that formatter. The CI pin in
`.github/workflows/mindroom-ci.yml` (`06dd1fbbc`) moves from
`nightly-2026-11-03` to `nightly-2026-11-20` to match. `flake.lock` is
unchanged from upstream `f7dc0770b`.

## Semantic review

The implementer was Gemini 4 Neon. Independent read-only reviews were performed
by Claude Opus 5.5 (drops and invariants) and GPT-6.2 Astra (adaptations and
conflicts).

- **Round 1 findings (fixed before Phase 2):**
  - Federation proxy panic in `src/api/router/proxy.rs` (`Root::Federation`
    must resolve `origin_key` before `parse_json` in `#96` and insert it into
    `request.parts.extensions` in `#99`, matching `router/args.rs`).
  - Missing `futures::FutureExt` import in `src/service/rooms/event_handler/fetch_prev.rs`
    (`#117`).
  - Missing `sticky: None` on three `PduEvent` test literals (`edits` owner)
    and removed `ServerSignaturesInput` in `federation_receipt_edu.rs` (`#66`,
    migrated to `XMatrixSigningInput`).
  - Restoring `#129` as a remainder on upstream's `walk_level`, and extending
    delayed-events scope for own MSC4354 sticky `rtc.member` leaves (restoring
    upstream's positive `sticky_events.rs` timer/fire tests).
- **Post-approval cleanups (from Opus 5.5, applied into `#129`):** removed
  redundant `WalkState.kept` (uses `state.seen.len().saturating_sub(1)`),
  delegated `Service::requester_relations` to `requester_relation_rows(...,
  usize::MAX)`, and lowered the over-budget log in `walk_relations` from
  `warn!` to `debug_warn!`.
- **Both Claude Opus 5.5 and GPT-6.2 Astra approved** the static replay before
  Phase 2 builds.

## Verification

All builds, clippy checks, tests, and the upgrade probe ran on an aarch64
Linux VM inside `nix develop .#dynamic` (`cargo 1.100.0`, `rustfmt
1.12.0-nightly 6905905bb7 2026-11-19`) in a loopback-only user and network
namespace with `--offline --locked`, `TUWUNEL_DATABASE_PATH` unset, and
`percpu_arena:disabled`.

**Per-commit formatting and strict Clippy (`f7dc0770b..04b2fc9fb`).**
- `cargo fmt --all -- --check`: **102/102 PASS, 0 FAIL** (and PASS on
  unmodified `f7dc0770b`).
- `cargo clippy --offline --locked --workspace --all-targets --all-features -- -D warnings`:
  the initial per-commit sweep found 4 consecutive failures (`#38` `#97` through
  `#41` `#101`) caused by one `pdu.state_key.unwrap_or_default()` error in
  `#97` (`join.rs:852`, `[u8; 48]: Default` unsatisfied) that `#42` (`#103`)
  had overwritten. Fixing `#97` (`e5cfafec9`) and re-gating `#38`..`#42` left
  the tip tree byte-identical and achieved **102/102 PASS, 0 FAIL**.
- Earlier tip-level fixes rolled into their owners before the final sweep:
  `clippy::too_many_lines` in `check_identity_providers` (`#162`/`#165`/`#166`/
  `#178`), redundant `Value` import (`#143`), `clippy::assert_is_empty`
  (`#130`, `#177`), `closure-returning-async-block` and `clippy::future_not_send`
  (`#129`), `own_latest_reload_failure_drops_the_summary` (`edits` owner),
  `sso_fallback_account.rs` `"riley"` subject and restored
  `assert_account_data_owner` + `src/api/client/account_data/tests.rs` (`#184`).

**No default features.**
`cargo check --offline --locked --workspace --no-default-features`: **PASS**
(`rc=0`) on final code tip `04b2fc9fb`.

**Full suite** (`taskset -c 0-7 cargo test --offline --locked --workspace
--all-targets --all-features --no-fail-fast`; `-- --list` collected with
`--lib --bins --tests --examples` plus `--doc` because `--bench main` rejects
`--list`). Passes are derived from `running N` minus ignored with exit code 0
and zero `FAILED` test lines.

| Tree | Listed (`--list`) | Targets | Top-level passed | All `running N` minus ignored (incl. sub-harness) | Failed | Ignored |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Unmodified upstream `f7dc0770b` (baseline) | 2,185 | 178 | 2,181 | 2,506 | 0 | 4 |
| Rebased fork code tip `04b2fc9fb` | 2,490 | 231 | 2,486 | 2,820 | 0 | 4 |

Zero baseline aarch64 failures occurred on `f7dc0770b`. Comparing `--list`
between baseline and fork shows 311 fork-only tests and exactly 6 baseline
unit tests absent from the fork — the same 6 documented replacements as on
`v2.0.0` (4 `decode_apple_userinfo_from_id_token_*` from the native Apple owner
and 2 `grant_session_*cookie_path*` from `#161`).

**Dropped-commit tests against upstream replacements (measured on throwaway
branch from tip, deleted afterwards):**
- `#55` (`3f42d6e7d` `sync_history_visibility.rs`, incl. banned-invitee and
  rejoin topic): **PASS** verbatim (`0.45s`), no policy gap.
- `#141` (`2fcc0dd8e` `relations_redacted.rs`): **PASS** verbatim (`0.48s`), no
  policy gap.
- `#67` (`3aa9345fc` `federation_prev_event_room.rs`, incl. peer `/state_ids`
  and held-`/event`): **PASS** verbatim (`2.02s`), no policy gap.

**Release builds and upgrade probe (`v2.0.0-mindroom.14` → `04b2fc9fb`).**
Both release binaries were built on the aarch64 Linux VM with
`cargo build --offline --locked --release -p tuwunel`:
- Old release (`v2.0.0-mindroom.14`, `fdae72d2a`): `115M` (`tuwunel 1.5.0-2715
  (fdae72d2ac)`).
- New tip (`04b2fc9fb`): `120M` (`tuwunel 1.5.0-2915 (04b2fc9fb7)`, sha256
  `4d38ae7907812110`).

The old release seeded a fresh database (`77M`, 82 files, schema version 17:
three users including self-deactivated `carol`, private room, messages, three
edits with edit purge of the two superseded edits, thread with three replies
and one redaction, device identity keys, display name, 16 KiB media upload,
MatrixRTC call membership with a persisted delayed leave) and shut down cleanly
(`0 ERROR`, `0 panics`). Serving a copy of that database with the new tip
release binary across a restart passed **65 of 65 checks**:
- **Before restart (51/51):** versions/MSC4140, pre-upgrade tokens/devices,
  password login, deactivated user refused, display name, room state/members,
  purged edits gone and newest edit bundled, thread count/latest/relations and
  redacted reply, `/messages`, initial and incremental `/sync` from pre-upgrade
  positions, device identity keys kept (exact retry accepted, replacement
  refused with 403), media download byte-identical and auth-gated, call
  membership intact, pre-upgrade `delay_id` survived the upgrade (listed,
  restarted, cancelled), non-leave delayed events refused (403), own delayed
  MSC4354 sticky `org.matrix.msc4143.rtc.member` leave allowed and cancelled
  while sticky `rtc.member` join refused (403), own legacy delayed call leave
  lifecycle + 404/409 finished-delay behavior, and post-upgrade writes arriving
  on pre-upgrade sync positions.
- **After restart (14/14):** second scheduled delayed leave survived restart,
  tokens, purged edits, bundled edit, members, identity keys, media, thread
  state, and incremental `/sync`.
- **Startup logs:** both starts loaded RocksDB schema version 17 with **0
  unexpected migrations**, **0 ERROR**, **0 panics**, and clean `rc=0`
  shutdown (the only `WARN` is `version_extra` from the test shell's
  `TUWUNEL_VERSION_EXTRA`).

**Release tooling.** `scripts/fork_release_tag.py` read-only preview outputs
`base_version=2.0.0`, `release_iteration=15`, `release_tag=v2.0.0-mindroom.15`,
`reused_tag_at_head=false` and creates no tag.

**Measured vs inferred / not verified.**
- Measured: every gate, test count, dropped-commit test run, release build,
  upgrade probe check, and `fork_release_tag.py` preview above on code tip
  `04b2fc9fb`.
- Inferred without re-running builds: this final commit changes only
  `docs/rebase-upstream-f7dc077-2026-11-24.md` and `FORK_CHANGES.md` (`.md`
  files only), so code gates and suite results carry over from its parent
  `04b2fc9fb`.
- Not verified: live MatrixRTC clients/Element Call, live federation, real
  external OIDC providers, production database snapshot/scale, x86_64 release
  build, remote GitHub Actions CI run.

## Next rebase

Follow the established procedure (replay first, gate after, fix forward into
owners, run dropped tests and upgrade probe), plus these new lessons:

1. **Always run per-commit clippy across the whole stack**, not only the tip.
   A later commit (`#103`) overwrote `#97`'s invalid
   `pdu.state_key.unwrap_or_default()`, hiding a 4-commit compile break until
   the full 102-commit sweep caught it.
2. **Diff `-- --list` against the unmodified upstream baseline.** Comparing the
   exact test-name lists caught `account_data/tests.rs` being accidentally
   dropped when `#184` collided with upstream's `account_data.rs` →
   `account_data/{mod,tests}.rs` split. Pass `--lib --bins --tests --examples`
   (plus `--doc`) for `-- --list` because `--bench main` rejects `--list`.
3. **Watch for new Rust/Clippy lints and function-length creep on toolchain
   bumps.** Moving to Rust 1.100 introduced `clippy::assert_is_empty`
   (`#130`, `#177`), `closure-returning-async-block` and
   `clippy::future_not_send` (`#129`), and upstream additions to
   `check_identity_providers` pushed it over `clippy::too_many_lines` (172/150)
   until `check_registration_rules` was extracted at `#162`.
4. **Check fork config defaults and test subject names on new upstream tests.**
   The fork defaults `rooms.relations.bundle_edits = true`, which causes
   upstream tests assuming `false` (like
   `own_latest_reload_failure_drops_the_summary`) to enter edit-fold reload
   unless configured explicitly; and upstream boot tests reuse common subject
   names (`"erin"` in `sso_fallback_account.rs`), so fork additions should
   pick distinct names.
5. **Run dropped-commit tests verbatim on a throwaway branch** (`#55`, `#141`,
   `#67`) before signing off on drops; and keep the upgrade probe aligned with
   current persistence (from `v2.0.0` onward, `delay_id` persists in RocksDB
   and should survive the upgrade rather than returning `M_NOT_FOUND`).
6. **Reclaim `target/.../debug/build/tuwunel` between major build phases** on
   disk-constrained VMs: integration test binaries dominate target size (~60+
   GB per slot) while compiled dependencies stay small (~6 GB).
