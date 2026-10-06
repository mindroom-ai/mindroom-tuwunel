# Rebase on upstream v1.9.3 - 2026-10-05

## Inputs and scope

The starting fork main was `54b178e3595ff448f007bd8e79a39e395d09f338`, with
upstream `v1.9.1` (`114bc231622299f9a5da7a60f951e2b0e4144b89`) as its base. Its
last commit, `Backport upstream fixes from tuwunel main (#25)`, carried ten
upstream `main` commits as one squashed cherry-pick.

The new base is exactly the upstream `v1.9.3` release commit,
`7801b8ec6f7773d90b1bbfeeb352012d6e004e96`; its annotated tag object is
`99097f6bd49fa7f42a30f35d2469d05e2e0e4c37`. The GitHub release API reports
`v1.9.3` as the latest, non-draft, non-prerelease release (published
2026-09-25), and the remote peeled tag matches. Upstream `main` had moved 134
commits past the release; it is not the target.

While the rebase ran, fork main advanced to
`c90e963d7f5dce49edf66f055b1b081dd0105f06` (one commit past
`v1.9.1-mindroom.36`) with 42 more squash-merged PRs, #26-#67. They are
replayed in merge order after the backports, the context they were written in,
except #60, which upstream already owns; see below. Fork main must be
re-checked right before publication, since anything merged later is not in this
history.

The old fork tips are preserved locally by `backup/main-before-v1.9.3-20261005`
(`54b178e35`), `backup/origin-main-54246a482-20261005`,
`backup/origin-main-f2fe5c6d9-20261006`,
`backup/origin-main-5d6c7ee5e-20261006`,
`backup/origin-main-140251342-20261006`,
`backup/origin-main-4152ce59f-20261006` and
`backup/origin-main-c90e963d7-20261006`. No upstream commit or published
release tag is rewritten.

## Backports: release versus main

Only two of the ten commits #25 backported are in `v1.9.3`: `99c6c320a`
(refuse a device ID that names a cross-signing key) and `f4f5a03f5`
(`unwrap_or_else_async` returns the present value). Both are dropped. The
other eight landed on upstream `main` after the release. Rebasing onto the
release without them would reopen the account-lifecycle and visibility holes
they close, so each is re-applied with `git cherry-pick -x` as its own commit on
top of the rebased fork:

| Upstream commit | Change | Adaptation on v1.9.3 |
| --- | --- | --- |
| `11e7fcf31` | Clean up the server user when `emergency_password` is removed | None; the test now uses upstream's `Args::test_database_path` |
| `0d0467f59` | JWT login refuses a deactivated account | The test passes the fork's `DeactivationReason::Admin` |
| `fa3eefe45` | Clear the password last; keep Synapse admin user routes off the server user | Server-user deactivation keeps `DeactivationReason::Admin` |
| `eb655bc57` | Token and JWT login and the JWT stage of UIAA refuse deactivated accounts | Test keeps only the deactivation cases (the rate-limit cases need `f37d0327f`/`305f0b60d`, not backported); upstream's appservice helper `src/main/tests/appservice/mod.rs` (`0f3d138a5`) travels with it |
| `18039076c` | `/events` streams only the room events the requester may see | Only `is_world_readable_at` is taken from the `user_can.rs` conflict; the test runs unchanged on upstream's v1.9.3 boot fixture |
| `23ef4fdaf` | Judge a user's own member event by the membership it sets | The `visibility_filter` call is changed in `client/message.rs`, where v1.9.3 keeps it |
| `e85f4dc30` | `/refresh` refuses locked accounts | None |
| `45800bd79` | OIDC sign-in, device approval and token issuance refuse locked and deactivated accounts | Same resolution as #25; upstream OIDC code is unchanged between v1.9.1 and v1.9.3 |

The fork-harness copies #25 needed on v1.9.1 (`src/main/tests/fixture/mod.rs`,
`src/main/tests/device_key_removal.rs`, local database-path and client-post
helpers) are gone: v1.9.3 ships them (`68177b330`, `1347197ee`, `0b88bf381`).
Keeping one commit per cherry-pick lets the next rebase drop each as soon as a
release contains it.

## Fork PRs merged during the rebase

| PR | Change | On v1.9.3 |
| --- | --- | --- |
| #26 | `/context` keeps current state from requesters who may not read it | Clean |
| #27 | UIAA keeps only small request bodies for pending sessions | Clean |
| #28 | Sliding sync receipts and typing only for joined rooms | `range.rs` imports upstream's `room_config` (renamed from `room_config_hash`); the test uses the shared client helpers |
| #29 | Sliding sync caps the timeline limit at 100 | The cap's unit test moves to `rooms/tests.rs`, where upstream moved the module's tests |
| #30 | Federated key queries keep only the answering server's users | Clean; independent of upstream's `get_keys.rs` refactor |
| #31 | Leaving without a membership records no departure | The test uses the shared client helpers |
| #32 | A pending knock does not open a room over federation | The test uses the shared client helpers |
| #33 | Knock state leaves local memberships alone | Clean |
| #35 | Sliding sync withholds the timeline of knocked rooms | The test uses the shared client helpers |
| #36 | GitHub sign-in keys on the account id | Clean |
| #37 | SSO provider chaining does not read `loginToken` from the redirect URL | Clean |
| #38 | Upstream's auto-accept test polls for the `m.direct` write | Clean |
| #39 | Redactions stay out of `/sync` edit compaction | Clean |
| #41 | IPv4-mapped IPv6 addresses match the IPv4 denylist ranges | Clean |
| #42 | The auth chain fetch walk is bounded | Clean; independent of upstream's `fetch_auth.rs` change |
| #43 | Email password resets refuse deactivated accounts | Clean |
| #44 | Removing an alias by power level requires room membership | The test uses the shared client helpers |
| #48 | Every prev event's room is checked before its state is used | Upstream's comment on the boxed fetch kept on the now fallible collect |
| #49 | Public read receipts need membership and a room event | Clean |
| #50 | Per-user room scans stop at the user id boundary | Upstream's new module doc kept; the doc comments it added to the scans now describe the bounded prefix |
| #51 | Federation requests stop following a peer's redirects | Clean |
| #56 | The appservice `hs_token` stays out of failed request errors | Clean |
| #57 | `send_join` response events are checked before they are stored | Upstream's new doc comments kept; the two that said a signatures-only result is accepted as received now describe the redaction |
| #40 | Appservice account data requests stay within their user namespace | Clean |
| #55 | Sync timelines apply history visibility | Imports upstream's `RoomCreate`; the test uses the shared client helpers |
| #58 | Federated key claims keep only the answering server's users | Clean; independent of upstream's removal of appservice key claims |
| #60 | Sliding sync profile changes only for joined rooms | Dropped: upstream's `005830a1b` already does this, and #60's test passes on v1.9.3 without the change |
| #61 | Left rooms carry state only for users who joined | The test uses the shared client helper; its test fails on v1.9.3 without the change |
| #62 | The SSO username fallback skips accounts linked to another identity | Clean |
| #52 | A remote space hierarchy answer does not cache summaries of rooms we are in | Clean |
| #53 | A former member reads the room state from when they left | Fixed for upstream's `/members` `at` token (`6fe0095dd`), which the plain replay silently overrode; see below |
| #54 | Refresh rotates a device's tokens under its device lock | Clean; no caller of `set_access_token` holds that lock already |
| #59 | A withdrawn knock does not extend a former member's history | Upstream's `user_can_redact` doc comment kept; the test uses the shared client helper |
| #45 | Bundled replies and edits leave out events sent after a user left | Clean |
| #46 | The state endpoint refuses joins, knocks and invites in a banned room | Clean; independent of the #53 adaptation in `state.rs` |
| #47 | The old-event cutoff ignores backfilled events | Upstream's doc comment on the next timeline helper kept |
| #34 | SSO asks before sending a login token to an unlisted `redirectUrl` | Clean; its trusted-host option in `sso_callback_completion.rs` lets the rebase's `loginToken` exchange there keep running |
| #63 | The default `ip_range_denylist` denies the unspecified addresses | Clean |
| #65 | `.well-known` redirects are followed only to HTTPS URLs | Clean |
| #66 | Federated read receipts need a joined user and a room event | Clean |
| #67 | The events a PDU's state is taken from are room-checked | Imports follow upstream's removal of `StateKey` from `state_local_build.rs`; the checks sit on upstream's refactored event handler |
| #64 | The auth chain fetch is bounded by size as well as count | Upstream's doc comment on `validate` kept and extended with the size check; upstream's recursion comment kept on the now parsed-on-demand outlier call |

v1.9.3's shared test client (`68177b330`) provides the `post` and `post_url`
helpers eight of these tests had defined for themselves, which no longer
compile beside it. Each adaptation is recorded in its commit message. Every
other replayed file either has the same content before the PR on both bases or
gets the same result as on fork main. The files on bases upstream changed were
checked hunk by hunk against upstream's changes: `get_keys.rs`, sliding sync
`range.rs` and `rooms.rs`, `membership/{leave,knock,join}.rs`,
`auto_accept_invites.rs`, `sync/mod.rs`, `alias/mod.rs`,
`event_handler/{fetch_auth,fetch_prev}.rs`, `state_cache/mod.rs`,
`server_keys/verify.rs`, `keys/claim_keys.rs`, legacy sync `v3.rs`,
`membership/members.rs`, `client/state.rs`, `state_accessor/user_can.rs`,
`users/device.rs`, `timeline/mod.rs`, `event_handler/handle_incoming_pdu.rs`,
`api/router.rs`, the fork's `sso_callback_completion.rs`,
`event_handler/{fetch_state,state_local_build,upgrade_outlier_pdu}.rs`,
`fetcher/{opts,validate,tests}.rs`, `server/send.rs`, and the config option
text and tests in `config/{mod,tests}.rs` and `tuwunel-example.toml`.

#53 auto-merged into a silent conflict. Upstream v1.9.3 serves `/members` from
the snapshot at an optional `at` token; #53 replaced the snapshot with the
caller's visible state, and the replay placed its assignment after upstream's,
so `at` was ignored for every caller. The state accessor helper now takes the
token: a joined user reads the token's snapshot, and a former member reads it
only for a token before their departure, otherwise the state from when they
left. #53's test gained `at` cases for both; they failed on the plain replay
and pass with the fix.

#55 filters the same timeline the fork compacts: history visibility first
drops what the user may not see, then compaction collapses superseded edits
among the rest. Its test and `sync_edit_compaction.rs` both pass on the
combined loader.

## Changes now owned by upstream

| Previously carried change | v1.9.3 replacement | Fork disposition |
| --- | --- | --- |
| Optional appservice users in directory search (#16) | `b996bc597`, `ef274db22` (PR #594), `773bb5ce2`, native `user_directory.rs` test | Drop the commit and its five test files; option name and default are unchanged, so deployed configs keep working |
| Delete a device's identity-key row on removal | `1347197ee` (superseding `8877ad982`, `1e441a712`) | Drop the fork's delete; keep the per-device lock around upstream's removal |
| Password UIAA for SSO accounts | `0726625e6` (PR #590) | Adopt upstream's credential-matched rule; keep the fork's SSO, JWT and legacy-repair policy |
| Two #25 backports | `99c6c320a`, `f4f5a03f5` | Not carried |
| Sliding sync profile changes only for joined rooms (#60, merged during the rebase) | `005830a1b` | Not carried; #60's test passes on v1.9.3 without it |

## Semantic overlaps

- **UIAA (SSO owner).** Upstream now offers `m.login.password` when the account
  holds a real password, or for an LDAP-origin account under LDAP. The fork had
  hidden the password flow from every SSO-origin account. The resolution takes
  upstream's rule, which still never offers it to a passwordless SSO account,
  and keeps the fork's rest: SSO only for SSO-origin accounts, JWT never
  advertised and refused for SSO accounts, and the legacy-origin repair run
  before the origin is read. Visible change: an SSO-origin account that also
  holds a real password now sees password and SSO. Upstream's
  `uiaa_password_flows.rs` asserts exactly that and passes unchanged.
- **Deactivation (SSO owner).** Upstream split `full_deactivate` into helpers
  and bounded concurrent room departures (`c53d80d80`). The fork adds only its
  required `DeactivationReason` parameter; upstream's new lifecycle test
  `full_deactivate.rs` passes `DeactivationReason::Admin`.
- **Device identity.** The fork's per-device `device_key_mutex` now wraps
  upstream's removal, and upstream's new per-user `device_list_mutex` is taken
  inside `mark_device_key_update`. Locks are only ever taken in that order
  (device, then device list), so they cannot deadlock. Uploads announce
  `DeviceListChange::Device`, as upstream's do.
- **Push.** Upstream split the sending service (`54ebeaa96`, `dec5bc10a`) and
  added a suppressed-push flush. Every path, including the flush, still
  evaluates through `send_push_notice` and `get_actions`, where the fork's
  stream classification sits. Only imports conflicted.
- **Sync.** Legacy and sliding sync both still load timelines through
  `load_timeline_with_errors`, where the edit compaction runs. Upstream's new
  `timeline_prev_batch` (`5895a78b9`) reads the first event after compaction;
  compaction never removes state events (#22) nor empties a non-empty slice, so
  a create-rooted slice still starts with the create event.
- **Threads, migrations, media.** Clean replays. The fork's
  `recount_thread_replies` and `scrub_redacted_thread_latest` markers sit beside
  upstream's new migrations and run once.

## Added regression tests

Each is a real-router test in the owning commit, and each was shown to fail
when the behavior it pins is broken:

| Test | Pins | Deliberate break that made it fail |
| --- | --- | --- |
| `uiaa_sso_policy.rs` (SSO owner) | Flows for password, passwordless SSO, SSO with password, password-on-IdP-device, legacy and passwordless accounts with JWT enabled; JWT stage refused for SSO accounts | Binding a device IdP for every origin (password account offered SSO); skipping the legacy repair (legacy account offered nothing) |
| `sso_callback_completion.rs` (SSO owner) | The `loginToken` minted after self-reactivation logs in | Treating the `*` sentinel as deactivated in `deactivated_check` (403 instead of 200) |
| `sync_edit_compaction.rs` (#22) | Legacy and sliding sync, initial and incremental, keep only the newest edit plus the original and a state event claiming a replacement | Disabling compaction (superseded edit served); dropping #22's state guard (topic missing) |
| `state_departed_member.rs` (#53) | `/members` with an `at` token: a former member reads a pre-departure token's snapshot and is capped at the departure after it; a joined user reads the token's snapshot | The plain replay of #53 (`at` ignored; the former member's pre-kick token read `leave` instead of `join`) |
| `device_key_immutability.rs` (device identity) | Uploads racing removals on four devices of one user, under the fork's per-device lock and upstream's per-user device-list lock, finish within a deadline and leave no identity keys | Dropping the device re-check after the lock (keys resurrected; failed 3 of 3 runs, while the unmodified test passed 5 of 5) |

## Verification

Checks used the project's Rust 1.95.0 and locked formatter
(nightly-2026-08-05) inside `nix develop .#dynamic`, with
`TUWUNEL_DATABASE_PATH` unset. Tests ran in a private user and network
namespace with only loopback; no host listener, production database, or live
service was used.

**Every commit.** Pinned `cargo fmt --all -- --check` and
`cargo clippy --offline --locked --workspace --all-targets --all-features -- -D warnings`
passed on each of the 62 commit trees; every final commit's tree is
byte-identical to a tree that passed. The gate caught problems in this
rebase's own changes, each fixed in its owning commit before the history was
finalized: an unfulfilled `clippy::too_many_lines` expectation, an unnecessary
trailing comma and a `Duration::from_secs(60)` in the new tests, the
cherry-picked `0d0467f59` test calling `deactivate_account` without the fork's
reason, and the eight replayed fork PR tests that redefined the shared client
helpers.

**Full suite** (`cargo test --offline --locked --workspace --all-targets
--all-features --no-fail-fast`):

| Tree | Passed | Failed | Ignored |
| --- | ---: | ---: | ---: |
| Unmodified `v1.9.3` | 1336 | 0 | 4 |
| Owner commits and #15 (tree `c0f7f66f`) | 1411 | 0 | 4 |
| Through #24 (tree `2d2ef502`) | 1459 | 0 | 4 |
| With the backports (tree `ed961358`) | 1463 | 0 | 4 |
| With fork PRs #26-#38 (tree `ab0eecf2`) | 1477 | 0 | 4 |
| With fork PRs through #57 (tree `b461d8de`) | 1492 | 0 | 4 |
| With #58, #61 and #62 (tree `c9cec5f6`) | 1495 | 0 | 4 |
| With #52-#54 and #59 (tree `1c38b1ed`) | 1500 | 0 | 4 |
| With #45-#47 and #34 (final code) (tree `376d783e`) | 1503 | 0 | 4 |

Counts sum every `test result` summary, including the nested child-process
runs some service tests start; those can interleave a summary across two
lines, which a line-based count misses. The same four upstream tests are
ignored at the baseline and every snapshot. An earlier run with the
development shell's `TUWUNEL_DATABASE_PATH` still exported failed 15 fork unit
tests (SSO reactivation and the sidecar sweep) on a shared database lock; all
pass once the variable is unset.

**Upgrade probe.** A debug build of a released fork main seeded a fresh
database: two users, a private room, an original message with three edits, a
thread root with three replies of which one is redacted, uploaded device
identity keys, and a display name. A debug build of the rebased fork then
served that database twice. The probe ran from the pre-rebase main
(`54b178e35`, `v1.9.1-mindroom.12`) to an early rebased tree, from
`54246a482` (`v1.9.1-mindroom.19`) to the tree with #26-#38, from `f2fe5c6d9`
(`v1.9.1-mindroom.24`) to the tree with #26-#57, from `5d6c7ee5e`
(`v1.9.1-mindroom.27`) to the tree with #58-#62, from `140251342`
(`v1.9.1-mindroom.30`) to the tree with #52-#59, and from the latest fork main
(`4152ce59f`, `v1.9.1-mindroom.33`) to the final code (`a9864f793`). Each run
passed all 47 checks: pre-upgrade access tokens and device, password login, profile, the
newest edit bundled on the original, the thread count and latest reply,
`/messages` history, `/sync` compaction and incremental sync from a
pre-upgrade token, preserved identity keys, an exact-key retry accepted and a
replacement refused with 403, new writes, and a new edit becoming the bundle.
Server logs show no panic; their error lines are the deliberate 403, UIAA 401
challenges, and the shutdown dangling-reference report the old binary prints
as well. Both binaries used the flake's dynamic RocksDB (the flake lock is
unchanged between v1.9.1 and v1.9.3), so the probe covers logical
compatibility, not a RocksDB library transition.

**Independent review.** A separate reviewer compared the old and new fork
deltas file by file, the conflict resolutions, lock ordering, deactivation
reasons, and the backports. It found no defect in them, and flagged that fork
main had advanced during the rebase; those PRs are now replayed (above). Its
test suggestions (incremental sliding sync, the upload/removal race) are
added.

**Release tooling.** `scripts/fork_release_tag.py` previews
`v1.9.3-mindroom.1` for the rebased tree.

Documentation tests and an optimized build run on the frozen documentation
tree; their results belong to the handoff, as in the v1.9.1 record. A local
build and synthetic upgrade do not prove live federation, a real Apple
identity-provider exchange, production-scale load, cross-architecture
packaging, or a remote CI run.

## Next rebase

Follow the v1.9.1 procedure, with these additions:

1. Compare each temporary backport with the target release by commit, not
   by PR. Drop the cherry-picks the release contains, and re-pick the rest.
2. Unset `TUWUNEL_DATABASE_PATH` (exported by the development shell) before
   running tests. Fork unit tests that load `Config` from a file honor it and
   otherwise share one database.
3. Pin `TUWUNEL_VERSION_EXTRA` for per-commit checks; it otherwise changes with
   every commit and forces a full rebuild.
4. Fetch fork main again before publishing and require it to be an ancestor
   of the old base the replay started from
   (`git merge-base --is-ancestor origin/main <old-base>`). Replay anything
   merged meanwhile; this rebase had to pick up twelve PRs that way.
