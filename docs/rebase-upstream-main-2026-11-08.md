# Rebase on upstream main - 2026-11-08

## Inputs and scope

The starting fork main was `f727901a43f7d60460e0a091938b7e0fa09478c2`
(`v1.9.4-mindroom.38`), 104 commits on the upstream `v1.9.4` release commit
`f473d9a4a8342514302884041e0ccfea1ad503fc`. The new base is upstream `main` at
`8c6c4a21054c0de5f4e70315870372def951e9ae` ("sync: Keep extensions for
unforgotten departures. (fixes #677) (e43803c38a)", 2026-11-07), 294 commits
past `v1.9.4`. As in the 2026-10-06 rebase this targets upstream development,
not a release. Upstream main was fetched again at the end and had not
moved. Fork main gained one commit while the rebase ran, #156
(`e5593875e`, "auth/uiaa: bind an untagged device's SSO stage to the
account's linked provider"); it is replayed after #154 and applies and passes
unchanged. The open fork PR #150 ("pdu: leave unsigned out of the PDU size
check") is not part of this rebase and will need rebasing onto the new main.

The old fork tip is preserved locally as
`backup/main-before-upstream-main-20261108`, and fork main with #156 as
`backup/main-e5593875e-before-upstream-main-20261108`. No upstream commit or
published release tag is rewritten. The workspace version is still `1.9.4`, so
the release tooling previews `v1.9.4-mindroom.40` (`v1.9.4-mindroom.39` is the
#156 release, `e5593875e`).

## Upstream restructuring

Most of the 294 commits move code rather than change it, and every fork commit
that touches the moved code is carried onto the new layout:

- The Matrix types left `tuwunel_core::matrix` for the `tuwunel_matrix` crate
  in `src/matrix` (`a3a281942`); imports follow.
- The route table is one builder per endpoint crate (`79af565a8`,
  `658bdf4af`), and the client, federation and admin surfaces are their own
  crates (`06d0b0a6a`): `src/api/server` is now `tuwunel_api_federation` in
  `src/api/federation`, beside `tuwunel_api_client`, `_admin`, `_mas` and
  `_oidc`. Fork routes are registered in the owning crate's `routes.rs`, and
  the one helper the federation crate needs from the client crate
  (`room_event_pdu_id`, #66) is public. The `src/mindroom-tests` harness builds
  its router from the per-crate builders.
- The configuration is grouped into sections (`6d214a784`..`f3023520f`,
  `e7e767e9d`); every moved option keeps its old top-level name as a legacy
  key. The fork's own `mindroom_*` options stay top-level, so deployments keep
  their keys. The fork's changes to upstream options move with them:
  `bundle_edit_relations` is `rooms.relations.bundle_edits`, still on by
  default in the fork, and `push_everything` is `push.everything`.
- `src/api/client/sync/v3.rs` is a directory module (`fd9d9e376`).
- The boot test fixture takes `&str` options (`03d2baa33`).

## Changes now owned by upstream

Upstream contains 13 of the fork's commits. Each was checked against the
upstream code and tests, not by subject. Their fork code, tests and
`FORK_CHANGES.md` entries are dropped, and upstream's version is kept where it
differs.

| Fork | Change | Upstream main |
| --- | --- | --- |
| #26 | `/context` keeps current state from requesters who may not read it | `020b286f3` |
| #53 | `/state`, `/members` and `/initialSync` serve a former member the state from when they left | `750dbc0b9`, `ed0ec6157`, `020b286f3` |
| #54 | A device's tokens rotate under its device lock | `f300f24ad` (PR #629, merged as `204300f56`), `0dbeee78a` |
| #57 | send_join response events are checked before they are stored | `b24ce987f`, `182de7110` |
| #70 | The room summary stops serving former members the current summary | `caa56cb9c` |
| #71 | `/context` serves a former member the state from when they left | `020b286f3`, on `750dbc0b9` and `ed0ec6157` |
| #74 | send_join events replace knock state stored before the join | `182de7110`, `b24ce987f` |
| #85 | A refresh token is classified again under the device lock before rotation | `6e9a3d35b` (PR #629), `0dbeee78a`, `d4f37432f`, `d99977231` |
| #87 | Knock state events are checked before they are stored | `a2f26001b`, with `18e952c72`, `33c1c7cd5`, `0e7f9ddb6`, `c4d9375e1`, `c364960e2` |
| #106 | Sliding sync long polls keep waiting when only fallback key types are new | `77914e28c` (PR #658, merged as `139dfaddf`) |
| #107 | Rooms created in the same millisecond get their own room ids | `424e42b3b` |
| #113 | A room's aliases are not listed to former members | `750dbc0b9` (the fork carried it as a backport) |
| #144 | Profile updates keep the join's sync position | `b89011524` |

Where upstream chose a different policy, upstream's is kept:

- #26, #71: a requester who may not read the state gets 404 from `/context`
  instead of the event with no state, and an admin who left reads their
  departure snapshot rather than the current state.
- #53, #113: the departure snapshot is the state at the leave with the leave
  applied. A departed member reads it under every history visibility, and
  forgetting the room, or a pending invite or knock, ends that.
- #87: a remote knock stores no outliers, room state or memberships, and its
  stripped summary keeps only verified events of the room; legacy `Stripped`
  entries reach the client as hints, where the fork hid them.
- #106: a pass woken by the user's own watchers answers with the fallback key
  types alone, for key backup clients; a first pass still waits.
- #107: a v12 create event carries a random `uwu.nonce` in its content, where
  the fork moved `origin_server_ts`.
- #144: the join position survives every join-to-join append (`replaced_join`
  in `timeline/append.rs`), not only profile changes; a forced-state replay
  that brings in a new join for a joined user still resets it.
- #54, #85: `rotate_refresh_token` also rechecks the token's expiry and the
  account under the device lock.

Eleven commits keep only what upstream lacks: the device identity owner
(immutability on upstream's device lock), #28 (receipts capped at the departure
on upstream's departure delivery), #81 and #125 (on upstream's departure
snapshot), #95 and #97 (authorization on upstream's `verified_pdu` and
`add_verified_outlier`), #103 (the join half), #138 (without its lenient
`email_verified` reader), and #126 and #133 (tests only).

## Adaptations

Each is recorded in the owning commit's message.

The moves above account for most of the changed lines: imports, route
registration, configuration paths and test fixtures. The commits that change
behaviour or structure on top of them:

| Commit | Upstream change | Adaptation |
| --- | --- | --- |
| Edits owner | Configuration sections (`6d214a784`..`f3023520f`, `e7e767e9d`) | `mindroom_*` options stay top-level; `rooms.relations.bundle_edits` defaults to true through a manual `Default for Relations`, recorded in the frozen defaults baseline |
| SSO owner, Apple owner | `ed73ced05`, `d32387c82` (UIAA JWT validator and client address); `82e806e82` (a replaced session whose delete fails) | The SSO-origin refusal sits in front of upstream's checks; `complete_sso_session` keeps upstream's delete handling; the Apple route is registered in the client crate |
| Apple owner, #138 | `9863bc352`: `email_verified` read leniently | The native claims use upstream's `deserialize_lenient_bool`; the fork's `lenient_bool` and `user_info::email_verified` go |
| Push owner | `f3023520f`: MSC3664 relations resolved only while `push.msc3664_related_event_match` is on | The terminal-stream content is resolved inside that closure |
| Device identity owner | `f300f24ad`/`204300f56`, `0dbeee78a`, `6f1899dcb`: a device's writers under `lock_device` | `device_key_mutex` and the fork's `add_device_keys`/`remove_device` hunks go; `update_device_keys` compares and writes under upstream's lock |
| #28 | `e43803c38`, `8c6c4a210`: a left room's receipts on the departure sync | Receipts for a left or banned room are capped at the walked `departure_count` instead of withheld |
| #36 | `82e806e82`: `Sessions::delete` returns a result | The login-keyed session's delete warns unless the session is gone |
| #42, #79, #91 | `e604a16dd`: `federation.history.max_fetch_prev_events` | Doc text moves to `History`; the walks read the new path |
| #45, #143 | `e604a16dd`: the thread bundle moved into `bundle_thread_for` | The visibility checks and the title previews run there |
| #55, #61, #125 | `afe2bb529`: left-room history bounded by the accepted departure, with a test expecting later events and state for users the fork hides them from | The fork filters stay; the test's left-room history case expects nothing after the departure. `load_left_room` expects `too_many_lines`. The knock test reads the left room's name from its timeline too, which now ends at the kick |
| #66, #96 | `06d0b0a6a`: federation in its own crate | `room_event_pdu_id` is public; `/send` parses each PDU under its room lock and hands it to `handle_pushed_pdu` (`213904761`) |
| #67 | `916bccfdc`: a backdated-event case in `federation_prev_event_room.rs` | The peer case runs beside it |
| #68 | `4345148e1`: `fingerprints_cover` and unread fingerprints | The selector index covers both |
| #80, #90 | `47e6cb629`: `network.http.max_response_size`; `fe184a284` peer outcomes | Caps read the new path inside upstream's `execute_on` |
| #81, #126 | `750dbc0b9`, `ed0ec6157`, `020b286f3`, `afe2bb529`: departure snapshots and left-room bounds | `last_departure` feeds `departure_event_count`; a leave or ban without a snapshot ends the walk; a pending knock gets upstream's refusal (404 from `/state/{eventType}/{stateKey}`, 403 from `/initialSync`); #126 keeps only its test |
| #82 | `0177e2de8`: redacted events leave the unread counts | Edits are redacted before their target, while it still names its thread |
| #88, #95, #97, #103, #116 | `b24ce987f`, `182de7110`, `6f894f03a`, `a2f26001b`: verified send_join storage, knock summaries | `ingest_send_join_events` is rebuilt on `verified_pdu` and `add_verified_outlier`; upstream's send_join tests build on a create and join chain; the knock halves go |
| #98, #119 | `67a046278`, `e3f9f9c00`, `e58dfbaeb`: sliding sync list tests and window | The caps and the once-per-room copy sit in upstream's code |
| #123 | `2f36a45fd`, `0177e2de8`: `redaction_txn` stages the thread root | The successor's `prev_content` is staged after the root |
| #124 | `32e77efe1`, `2013e6db6`: MAS in an optional crate; `d5a6c094d` raw bodies | A `MasAuthenticated` marker exported from the router; parsing through `parse_json(&request, route)` |
| #129 | `d6870147d`: `relation_counts` and `related_pdu` | The recursive walk uses upstream's helpers |
| #130 | `fd9d9e376`, `a4ebaabc3` | Delivery in `v3/joined/mod.rs`; `await_join_aggregates` expects `fn_params_excessive_bools` |
| #133, #139 | `588a060ed`: every non-zero thread count with each room; `b89011524`: `read_replaced` | #133 keeps only its test; `read_replaced` reports the sender's own join for `own_read` |
| #140 | `0177e2de8`, `d6870147d`: redaction settlement with a separate highlight lock | Both counters settle under the notification lock; the highlight lock goes. Its recount removes a read thread's rows, so the #133 test expects the read thread left out, not sent as zero |

## Formatter

On `8c6c4a210` the locked Fenix formatter (rustfmt 1.10.0-nightly
`1ed2df61a1` 2026-08-04, the nightly-2026-08-05 toolchain) reports ten files,
and nightly-2026-10-04 reports none, so the CI pin is unchanged; its comment
and the CI commit now name upstream main. `flake.lock` is unchanged from
upstream.

## Semantic review

Two independent read-only reviews covered the drops and the adapted commits:
each dropped commit against the code at `8c6c4a210`, and every commit with a
rebase note against its old version and the final tree, including new upstream
code paths that reach behaviour a fork commit guards.

The drop review found no lost protection; the policy differences it found are
listed above. The adaptation review found three problems this rebase had
introduced, all fixed in their owning commits:

- #28: sliding sync capped a left room's receipts at the raw left count, so a
  member who was kicked, knocked and withdrew got the receipts sent after the
  kick. The cap is now the walked `departure_count`, which also gives a user who
  never joined none; `sync_v5_departed_ephemeral.rs` gains the case, which
  fails without the fix (#81).
- #81: the walk took any member event without a state snapshot as the
  departure; only a leave or ban now ends it, and anything else fails the read.
- #82: with the edits redacted first, an edit that failed to redact left the
  target unredacted. An edit's failure is now logged and does not fail the
  redaction, which is stored by then; only the target's redaction can.

A second review of these fixes and of the pusher and sync paths found no
high-severity problem. Fixed in their owners: #82's edit errors (above), a
backfilled departure that lifted #28's receipt cap (it now gives no receipts),
and two tests that could not tell a correct bound from a too-strict one or a
leak (#81 now also checks that a receipt from before the kick arrives, and
#125 that the later name appears nowhere in the left room).

Points not changed here:

- #140 meets upstream's redaction settlement (`0177e2de8`): after a receipt for
  an older event the recount keeps the later events counted but stamps the
  receipt's arrival, so redacting one of those events leaves the count one too
  high until the next receipt. A redaction racing the recount can also skip it.
  Both need the recount to record what it read up to; that is a design change
  beyond this rebase.
- OIDC `/userinfo` (`src/api/oidc/userinfo.rs`) accepts a refresh token as its
  bearer token; #127's refusal sits only in the client router. This predates
  the rebase.
- Redacting a message redacts every one of its edits while the redaction holds
  its sequence number, so a message with hundreds of edits in a busy room can
  hold back sync for a while (unconfirmed; the fork did this before too).
- The departure walk: a walk error refuses a former member the state a
  stranger could read in a world-readable room; sliding sync walks it on every
  sync of a left room; and forgetting a room does not stop a later walk from
  reaching back through it. Upstream's sliding sync room counts can also stay
  stale after a thread-only receipt.

These are tracked as fork issues #157 (the #140 recount), #158 (`/userinfo`),
#159 (edit redaction) and #160 (the departure walk).

## Verification

Checks used the project's Rust 1.95.0 inside the flake's dynamic development
shell, rustfmt nightly-2026-10-04, `TUWUNEL_DATABASE_PATH` unset and
`TUWUNEL_VERSION_EXTRA` pinned. Tests ran in a private user and network
namespace with only loopback.

**Every commit.** `cargo fmt --all -- --check` and
`cargo clippy --offline --locked --workspace --all-targets --all-features -- -D warnings`
passed on each of the 93 commit trees. The replay was gated commit by commit
up to #136; the remaining commits were replayed, the tip built, its breaks
fixed forward into their owners, and then every final tree whose content had
changed was checked again in four parallel worktrees on a second machine.

**Full suite** (`cargo test --offline --locked --workspace --all-targets
--all-features --no-fail-fast`):

| Tree | Passed | Failed | Ignored |
| --- | ---: | ---: | ---: |
| Unmodified upstream main `8c6c4a210` | 1970 | 0 | 4 |
| Rebased fork (code of the final tip) | 2204 | 0 | 4 |

Every upstream test that runs on the baseline also runs and passes on the
fork, except the four `decode_apple_userinfo_from_id_token_*` unit tests the
native Apple owner replaces, as before. A suite run on an intermediate snapshot
(through #104) found the first three expectations below early; the final runs
found the fourth and the #133 and knock-test expectations in the adaptations
table.

**Adapted expectations.** Four tests assert behaviour the fork deliberately
changes, and are adapted in the commit that changes it:

- `knock_withdrawal_history.rs`, the fork's own test, expected 403 from
  `/state/{eventType}/{stateKey}` for a knocking member; upstream refuses that
  route with 404 (and `/initialSync` with 403).
- `snapshotless_event_is_judged_by_current_state` (`020b286f3`) stores a leave
  with no state snapshot. The fork's departure walk failed on it; it now takes
  such a leave as the departure (#81).
- `rescinded_invite_carries_leave_member`'s left-room history case
  (`afe2bb529`) expected a user who never joined, or whose departure cannot be
  read, to receive what was sent after they left. Sync timelines follow history
  visibility (#55) and a left room's state goes only to a user who joined it
  (#61) and whose departure is found (#125), so the test now expects nothing.
- `rebuild_leaves_out_threads_with_no_replies` (`2f36a45fd`) assumes a
  backfilled root's live reply is not in the relation index. #143 indexes it,
  so the rebuilt list orders that root by its reply.

**Dropped tests against the new tip.** The last fork versions of the seven
integration tests the drops removed (#26/#71, #53/#70/#113, #54, #85, #106,
#107, #144) were added to the tree under new names and run; the #85 test
calls upstream's new `rotate_refresh_token` signature. Six pass. In
`context_snapshotless_state.rs` the owner and the kicked member get the
expected state, and the stranger gets upstream's 404 where the fork expected
200 with no state. The dropped unit tests (#57, #74, #87) were not run;
upstream tests the same functions.

**Upgrade probe.** A debug build of `v1.9.4-mindroom.38` (`f727901a4`)
seeded a fresh database (three users, a private room, an original message with
three edits, a thread root with three replies of which one is redacted,
uploaded device identity keys, a display name, and a self-deactivated user),
then restarted with the edit purge enabled until it purged the two superseded
edits. The rebased fork served a copy of that database twice, once as a debug
build and once as the optimized release binary, and passed all 51 checks each
time: pre-upgrade tokens and device, password login, the deactivated user
refused, purged edits still gone and the newest one bundled, thread count,
latest reply and relations, `/messages`, `/sync` and incremental sync from
pre-upgrade positions, display name, identity keys kept, an exact-key retry
accepted and a replacement refused with 403, new writes, edits and replies,
and the same answers after a restart. The configuration used the old
top-level keys, and none was reported unknown. No server panicked, and neither
start ran a migration beyond loading schema version 17.

**Release build.** `cargo build --offline --locked --release --bin tuwunel`
succeeded on the final code (binary sha256 `49cb061dbf4b…`), and the upgrade
probe above ran with it. `cargo test --offline --locked --workspace
--all-features --doc` passed; the workspace defines no doctests.

**Release tooling.** `scripts/fork_release_tag.py` previews
`v1.9.4-mindroom.40`.

A local build and synthetic upgrade do not prove live federation, a real Apple
identity-provider exchange, production-scale load, a production database
snapshot, cross-architecture packaging, or a remote CI run.

## Next rebase

Follow the v1.9.1 procedure with the later additions, and also:

1. Replay first, gate after. Stop only for conflicts, build the tip, fix
   forward with `fixup!` commits aimed at their owners, fold them in, and only
   then check every commit, in parallel worktrees. Gating each commit during
   the replay was most of this rebase's time: each stop costs one or two full
   strict Clippy runs before the fix.
2. Run the full suite on intermediate snapshots on another machine while the
   replay continues. The snapshot at #104 found three semantic failures hours
   before the tip existed.
3. Run every test whose expectation was changed by reading code. Two
   expectations written during this replay (#133's zero thread count and the
   knock test's left-room name) were wrong once a later commit (#140) or
   upstream's bounded timeline applied.
4. Upstream main moves fast and restructures in waves (crates, configuration
   sections, module splits). Rebase onto a point just after such a wave, and
   read upstream's own tests of each area the fork changes: they encode the
   policy the fork's tests must now agree or disagree with.
