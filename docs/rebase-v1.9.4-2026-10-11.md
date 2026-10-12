# Rebase on upstream v1.9.4 - 2026-10-11

## Inputs and scope

The starting fork main was `ab385cac3ae2d24e2764c6ab74a172e60609389c`
(`v1.9.3-mindroom.28`), 95 commits on upstream `main` at `3f5db6d3a`. The new
base is exactly the upstream `v1.9.4` release commit,
`f473d9a4a8342514302884041e0ccfea1ad503fc` ("Bump 1.9.4."); its annotated tag
object is `024cce75b51ec7dd4b8fbef959540251a0b6cb4f`. The GitHub release API
reports `v1.9.4` as the latest, non-draft, non-prerelease release (published
2026-10-10), the remote peeled tag matches, and upstream `main` is the same
commit. `v1.9.4` is 88 commits past `3f5db6d3a`. Fork main, upstream main and
the release were fetched again at the end of this rebase and had not moved,
and no fork PR was open.

The old fork tip is preserved locally as
`backup/main-before-v1.9.4-20261011`. No upstream commit or published release
tag is rewritten. The workspace version is now `1.9.4`, so the release tooling
previews `v1.9.4-mindroom.1`.

## Changes now owned by upstream

Upstream contains 34 of the fork's commits. Each was checked against the
`v1.9.4` code and tests, not by subject. Their fork code, tests and
`FORK_CHANGES.md` entries are dropped, and upstream's version is kept where it
differs.

| Fork | Change | Upstream `v1.9.4` |
| --- | --- | --- |
| #24 | Bounded search for a redacted latest reply's replacement | `20feec81f`, `281becc72` |
| #27 | UIAA keeps only small request bodies | `fcc07b1bd`, `0b43956f5`, `cdde2f41c` |
| #29 | Sliding sync timeline limit capped at 100 | `05f1acbb5` (PR #637) |
| #30 | Federated key queries keep the answering server's users | `f839d50ab` (PR #624), `113a83d5b` |
| #31 | Leaving without a membership records no departure | `12066bfb0` (PR #631) |
| #32 | A pending knock does not open a room over federation | `376ad2100` (PR #634) |
| #33 | Knock state leaves local memberships alone | `2774f0372` (PR #632) |
| #34 | Ask before sending a login token to an unlisted `redirectUrl` | `7d83687ca` |
| #35 | Sliding sync withholds the timeline of knocked rooms | `5aedf8395` (PR #625), `607b1040f` |
| #37 | SSO provider chaining without a `loginToken` in the redirect URL | `7d83687ca` |
| #38 | Auto-accept invite test waits for the `m.direct` write | `322f896a1` (PR #630) |
| #40 | Appservice account data stays in the user namespace | `28e3babf0` (PR #626), `7e9466e91` |
| #41 | IPv4-mapped IPv6 addresses match the IPv4 denylist | `3d2974221` (PR #636) |
| #43 | No email password reset for deactivated accounts | `4a4475b1b` (PR #628), `7f8b85e59` |
| #46 | Banned rooms refuse member events through `/state` | `b77f8e224` (PR #627), `f40de1537` |
| #47 | Backfilled events do not set the old-event cutoff | `deb955b7a` |
| #48 | Prev events from another room are rejected | `324520029`, `fd13f974d` |
| #49 | Public read receipts need an event of the room | `0d7794fab` |
| #50 | Per-user room lists stop at the user ID | `33183bd41` (PR #633) |
| #51 | Federation requests follow no redirects | `021841f4c`, `344d2c5f1` |
| #52 | Remote hierarchy answers leave local rooms' summaries alone | `5b2d53487` |
| #56 | Failed appservice requests leave the `hs_token` out | `d07c762dd` (PR #635) |
| #58 | Federated key claims keep the answering server's users | `bd50ce817` (PR #624), `113a83d5b` |
| #62 | SSO username fallback skips accounts linked elsewhere | `727884c6c` |
| #63 | Default denylist covers the unspecified addresses | `caf79ae3f` (PR #636) |
| #65 | Server discovery follows redirects only to HTTPS | `d9ab36cac` |
| #72 | Remote hierarchy answers are checked for their room id | `5b2d53487` |
| #75 | `ban-room` makes local members leave | `f40de1537` |
| #76 | Sliding sync profile changes only for joined rooms | `2059b2e23` |
| #77 | Remote profile lookups held to the local limits | `0933f37c3`, `5490f37ac` |
| #78 | Requested profile field lists capped | `0933f37c3` |
| #83 | Bracketed IPv6 literals parsed for the denylist checks | `b8234c9d9` |
| #86 | Remote profiles with more than 100 fields refused | `0933f37c3` |
| #102 | Local read receipts check event visibility | `0d7794fab` |

Where upstream chose a different policy, upstream's is kept:

- #24: the scan limit is `thread_latest_reply_search_limit` (default 1024, the
  fork used 256), and a bounded or failed search keeps the thread count and
  drops only `latest_event`.
- #34: the Continue page is shown for every target outside the
  `well_known.client` origin and `oidc_registration_allowed_redirect_hosts`,
  also when `oidc_require_client_approval` is false; an entry matches a URL's
  host, or its scheme only when it has no host. The fork-only
  `sso_trusted_redirect_hosts` option is gone, and a config still setting it
  gets the unknown-key warning. No MindRoom deployment configuration in the
  workspace sets it or `oidc_require_client_approval`.
- #77, #86: a fetched profile is merged into the cached one, not replacing it;
  the merged profile must stay within 100 fields (`max_remote_profile_fields`)
  and 64 KiB.
- #78: more profile fields than `max_profile_fields_per_request` (default 64)
  are refused with 400 instead of truncated, including in a stored filter.
- #76: rooms peeked without a membership no longer contribute profile changes.
- #83: the literal check parses with `IpAddr` only, so a short IPv4 form such
  as `127.1` is resolved as a name; the final request is still checked against
  the denylist.

## Adaptations

Each is recorded in the owning commit's message.

| Commit | Upstream change | Adaptation |
| --- | --- | --- |
| SSO owner | `7f8b85e59`: password writes under the per-user `changing_password` lock in `set_password_locked` | A new password clears the deactivation reason inside `set_password_locked`. `email_password_reset/scenarios.rs` passes `DeactivationReason::Admin` |
| SSO owner | `7d83687ca`: Continue page for unvetted `redirectUrl` targets; it also lengthens `sso_callback_route` | `sso_callback_completion.rs` lists its client in `oidc_registration_allowed_redirect_hosts`. The route expects `clippy::too_many_lines` until the native Apple commit moves the identity block out |
| Native Apple | `7d83687ca` | `apple_userinfo_fallback.rs` lists its client the same way; the lint expectation is removed |
| CI | v1.9.4 formatted for newer nightlies | The rustfmt pin stays nightly-2026-10-04 (see below); comments name v1.9.4 |
| #57 | `d3db90b1d`: send_join auth-chain events go through `Pdu::from_object_federation` | Taken from upstream; only the stored-copy guard is added on top. The entry no longer claims the room check |
| #66 | #49 dropped for `0d7794fab` | The `room_event_pdu_id` extraction #49 made is done here, onto upstream's `read_marker/mod.rs` |
| #67 | `94198c340`: `state_at_incoming` checks the room of the prev events | The fork's `state_at_incoming` hunks and the `room_id` parameter go. The `fetch_state` and local-build checks stay; the peer cases join upstream's `federation_prev_event_room.rs`, whose `assert_rejected` takes an origin |
| #84 | `ff77c2220`: transaction EDU byte budget, to-device content and device ID caps | Only the uploaded key size checks, their constant and their test remain; the subject now says so |
| #87 | `2774f0372` (fork #33) | Upstream's knock test is kept and switched to signed events, which the check requires; the fork's second test is added |
| #88 | `73e24ae0d` | Test only; it pins the create-event room id through the fork's send_join ingestion |
| #89 | `05f1acbb5` rewrote the `merged_room_details` fold | The required-state cap is applied inside upstream's fold |
| #90 | `0933f37c3`: `fetch_remote_profile` merges with `merge_profile` | The capped `request_remote_profile` feeds `merge_profile` |
| #91 | `deb955b7a`: `first_ts_in_room` became `first_normal_ts` | Uses the new name |
| #93, #98 | #78 dropped | Test imports lose `MAX_SYNC_FIELDS` |
| #103 | #33 dropped | The knock sentence moves into this change's own entry |

Every other replayed hunk has the same `+`/`-` lines on both bases, apart from
`FORK_CHANGES.md` context. For every file the fork touches, the change from the
old tip to the new tip equals upstream's own `3f5db6d3a..v1.9.4` change, apart
from the drops, the adaptations above and the documentation.

## Formatter

On `v1.9.4` the locked Fenix formatter (nightly-2026-08-05) reports nine files
and nightly-2026-10-04 reports none, so the CI pin is unchanged. `flake.lock`
is unchanged from upstream.

## Semantic review

Two independent read-only reviews covered the adapted commits and the drops:
each dropped commit against the `v1.9.4` code, kept commits against
duplicates, every file whose fork delta changed, and new upstream code paths
against the fork's fixes. They found no lost fork behaviour, no reverted or
bypassed upstream code, no duplicate, and no commit depending on a later one.
Their wording findings are fixed in the affected messages and entries.

Points that predate this rebase and are not changed here:

- SSO self-reactivation (`maybe_reactivate_deactivated_sso`) checks the
  deactivation reason and then writes the sentinel password without holding
  upstream's new `changing_password` lock across both, and `deactivate_account`
  writes the reason after that lock is released, so a self-reactivation racing
  an admin deactivation can undo it. The lock now exists to close it.
- Native Apple login creates its login token without a provider, while
  upstream's browser SSO tokens now carry one (`2607b7cde`); the token is only
  exchanged at `/login`.
- Upstream's `restore_redactions` migration (`c921f7f7a`) bypasses
  `append_pdu_effects`, so a redaction it restores does not redact the target's
  edits (#82). It only acts on databases a release that records redactions by
  reference has opened.

The dropped fixes no longer have fork tests. Upstream tests most of them; the
fork's tests for #32, #51, #65 (redirect hops), #75 and #102 cover behaviour
upstream has no test for, and are not carried (see the probe below).

## Verification

Checks used the project's Rust 1.95.0 inside the flake's dynamic development
shell, rustfmt nightly-2026-10-04, `TUWUNEL_DATABASE_PATH` unset and
`TUWUNEL_VERSION_EXTRA` pinned. Tests ran in a private user and network
namespace with only loopback.

**Every commit.** `cargo fmt --all -- --check` and
`cargo clippy --offline --locked --workspace --all-targets --all-features -- -D warnings`
passed on each of the 62 commit trees.

**Full suite** (`cargo test --offline --locked --workspace --all-targets
--all-features --no-fail-fast`):

| Tree | Passed | Failed | Ignored |
| --- | ---: | ---: | ---: |
| Unmodified upstream `v1.9.4` | 1656 | 0 | 4 |
| Rebased fork (code of the final tip) | 1815 | 0 | 4 |

On both trees, upstream's `src/main/tests/smoke.rs` binary aborts (SIGABRT)
after its first two tests when more than one test thread runs; with
`RUST_TEST_THREADS=1` all four pass on the fork. Every upstream test that runs
on the baseline also runs and passes on the fork, except the four
`decode_apple_userinfo_from_id_token_*` unit tests the native Apple owner
replaces, as at v1.9.3.

**Dropped tests against the new tip.** The last fork versions of the 16
integration tests the drops removed (for #31, #32, #35, #37, #46, #47, #48,
#49/#102, #51, #56, #62, #65, #72, #75, #76, and #84 before it was trimmed)
were added to the final tree and run: all pass. The #37 and #62 tests needed
their client listed in `oidc_registration_allowed_redirect_hosts` for
upstream's Continue page. The #78 test expects truncation and does not apply.
The dropped unit tests were not run; upstream tests the same functions.

**Negative check.** With #67's `fetch_state` room check removed, the merged
`federation_prev_event_room.rs` fails: the `unknown` case is accepted.

**Upgrade probe.** A debug build of `v1.9.3-mindroom.28` (`ab385cac3`) seeded a
fresh database (two users, a private room, an original message with three
edits, a thread root with three replies of which one is redacted, uploaded
device identity keys, a display name, and a self-deactivated third user), then
restarted with the edit purge enabled and purged the two superseded edits. A
debug build of the rebased fork served the database twice and passed all 53
checks: pre-upgrade tokens and device, password login, profile, the newest edit
bundled, purged edits still gone, thread count and latest reply, `/messages`,
`/sync` compaction and incremental sync from a pre-upgrade token, the
self-deactivated user refused, preserved identity keys, an exact-key retry
accepted and a replacement refused with 403, and new writes. Neither start ran
a migration beyond loading schema version 17, and no server panicked; the error
lines are the deliberate 403s and the shutdown dangling-reference report the
old binary prints as well.

**Release build.** `cargo build --offline --locked --release --bin tuwunel`
succeeded on the final tree, and the upgrade probe from `v1.9.3-mindroom.28`
passed all 53 checks with that optimized binary as the new server.
`cargo test --offline --locked --workspace --all-features --doc` passed; the
workspace defines no doctests.

**Release tooling.** `scripts/fork_release_tag.py` previews
`v1.9.4-mindroom.1`.

A local build and synthetic upgrade do not prove live federation, a real Apple
identity-provider exchange, production-scale load, a production database
snapshot, cross-architecture packaging, or a remote CI run.

## Next rebase

Follow the v1.9.1 procedure with the v1.9.3 and upstream main additions, and
also:

1. Match each fork commit to upstream by code, not subject: upstream took many
   fork fixes as the maintainer's own commits, sometimes several in one
   (`0933f37c3` covers #77, #78 and #86), and fork PRs submitted upstream may
   land extended (`7f8b85e59` after #43).
2. Upstream PR #629 (refresh-token rotation under a per-device lock, fork #54
   and #85) is still open; drop those commits once a release contains it.
3. Check that an adapted commit still passes clippy on its own tree: upstream
   growth can push a fork-extended function past `too_many_lines` before a
   later fork commit shrinks it.
