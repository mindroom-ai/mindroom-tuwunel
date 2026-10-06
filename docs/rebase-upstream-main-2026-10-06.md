# Rebase on upstream main - 2026-10-06

## Inputs and scope

The starting fork main was `e24ad339559e943289db434612225e4ac6aeb4e8`
(`v1.9.3-mindroom.1`), with the upstream `v1.9.3` release commit
`7801b8ec6f7773d90b1bbfeeb352012d6e004e96` as its base. Neither fork main nor
upstream main had moved when both were fetched again at the end of this
rebase, and no fork PR was open.

The new base is upstream `main` at
`3f5db6d3a8b9fcf2b92f1b665a5ceb111a517f35` ("utils/future: Join Boolean
disjunctions without a Vec or Unpin.", 2026-10-05), 134 commits past `v1.9.3`.
Unlike the v1.9.x rebases this targets upstream development, not a release, as
the 2026-07 rebases did. The workspace version is still `1.9.3`, so the release
tooling previews `v1.9.3-mindroom.2`.

The old fork tip is preserved locally as
`backup/main-before-upstream-main-20261006`. No upstream commit or published
release tag is rewritten.

## Changes now owned by upstream

| Previously carried change | Upstream main replacement | Fork disposition |
| --- | --- | --- |
| Thread reply counts exclude redacted replies (#19) | `c9307347a` (PR #617) | Dropped |
| Redacted thread roots keep their summary (#20) | `787099ab5` (PR #618) | Dropped |
| Redacted replies leave the thread summary (#21) | `e6735fdf3` (PR #619) | Dropped |
| `recount_thread_replies` and `scrub_redacted_thread_latest` startup passes (#19, #21) | `rebuild_thread_summaries` (`b0c52a081`) | Dropped; the new pass runs once on a fork database, and the old markers are left unread |
| Eight temporary backports from upstream `main` | `11e7fcf31`, `0d0467f59`, `fa3eefe45`, `eb655bc57`, `18039076c`, `23ef4fdaf`, `e85f4dc30`, `45800bd79` | Dropped; each `cherry picked from` commit is an ancestor of the new base |

Upstream's tests for these changes, including the migration tests now keyed on
`rebuild_thread_summaries`, run unchanged. The fork keeps no test for them
beyond #24's.

## Adaptations

Each is recorded in the owning commit's message.

| Commit | Upstream change | Adaptation |
| --- | --- | --- |
| SSO owner | `3fe9ef681`, `5420f2e56`: `deactivate_account` takes the admin-room lock and refuses the last admin | The deactivation reason is written next to the password clear, under that lock. Upstream's `last_admin_deactivation.rs` and `jwt_login_deactivated.rs` pass `DeactivationReason::Admin`; `emergency` keeps upstream's server-user token revocation |
| SSO owner | `6527b269f`: UIAA checks the LDAP origin before the password lookup | The fork's `sender_uses_sso` sits beside upstream's `ldap_origin`/`has_password` |
| Device identity | `25dbbb5a3`: per-device `claiming_one_time_keys` lock | Kept beside `device_key_mutex`; no path takes both |
| CI | `707d1337a`, `b1f3572eb`: upstream formatted for newer nightlies | rustfmt pin moves from nightly-2026-08-05 to nightly-2026-10-04 (see below) |
| #18 | `0a6eef544`: admin commands take a `CommandInput` | The end-to-end test calls `command_in_place(command.into())`; the delete-retry test is formatted for the new pin |
| #24 | `b0c52a081`: thread summaries rebuilt over a fallible `try_get_relations` | The bound moves to `try_get_relations_limited`, taken before events load; `thread_replies` passes `MAX_LATEST_REPLY_SCAN` from `latest_thread_reply` and no limit from the startup rebuild. The fork test is unchanged |
| #51 | `a8e41c308`: federation clients built by `make_federation`, one direct and one SRV client each | `Policy::none()` goes into the shared configuration, so both follow no redirects |
| #53 | `3f5db6d3a`: `pin_mut` dropped from `user_can.rs` | Only `try_join` is imported |
| #61 | `3f5db6d3a`: `BoolExt::or` takes unpinned futures | The `pin_mut!` before the `once_joined`/`world_readable` join is dropped |
| #62 | `9993cb5f8`: reserved-name check for new SSO usernames | The fork's linked-identity check stays in the branch for an existing account |

The edits owner's import lines also merge with upstream's new `msgtype` and
`login_ratelimit` imports. Every other replayed hunk has the same `+`/`-` lines
on both bases. For every
file the fork touches, the change from the old tip to the new tip equals
upstream's own `v1.9.3..3f5db6d3a` change, apart from the adaptations above,
the dropped commits, and the documentation.

## Formatter

Upstream main is formatted for the 2026-09-26 nightly (`707d1337a`) and for
the lint fixes of the 2026-10-03 one (`b1f3572eb`). On `3f5db6d3a` the locked
Fenix formatter (nightly-2026-08-05) reports nine files, nightly-2026-09-26
reports one, and nightly-2026-10-04 reports none. CI now pins
nightly-2026-10-04. `flake.lock` is unchanged from upstream, so the development
shell's formatter is older than the CI pin and must not be used to reformat.

## Semantic review

Five independent read-only reviews covered federation and event handling,
authentication and accounts, client visibility and sync, storage and upgrade,
and push, network policy and CI. Each fork fix was checked against every code
path upstream added or changed in the range. They found no conflict introduced
by this rebase beyond the build errors already fixed above (#18, #53, #61),
which strict Clippy also caught.

Points found that predate this rebase (the same code is at `e24ad3395`) and
are not changed here:

- Native Apple login completes through `complete_sso_session` without
  upstream's `locked_check`, which the browser callback runs afterwards; a
  locked Apple user receives a login token that `/login` refuses but the OIDC
  account pages accept. Self-reactivation also runs before that lock check and
  outside the admin-room lock.
- Knock-response state stored as outliers before a remote join keeps the
  `send_join` checks (#57) from replacing those copies.
- Federation's IP-literal check passes `Url::host_str()`, which brackets IPv6
  literals (`[::1]`), to `ipaddress`, which rejects that form, so such hosts
  skip the denylist there (#41 covers the resolver and outbound clients, not
  this check).

Two interactions of the edit purge with upstream main remain open:

- Upstream's full injectivity repair runs only where the earlier repair did
  not settle (`global/fix_short_injectivity` holds a decline record or is
  missing). There, a damaged state anchored on a purged edit stays unrepaired
  and a warning is logged on each start; startup is not refused and no data is
  removed. The upgrade probes below take the settled path.
- `f64de3d0a` treats an event whose prev event has no `eventid_pduid` row as
  gapped and backs off its repeated delivery; a remote event citing a purged
  edit takes that path.

## Verification

Checks used the project's Rust 1.95.0 inside the flake's dynamic development
shell, rustfmt nightly-2026-10-04, and `TUWUNEL_DATABASE_PATH` unset. Tests
ran in a private user and network namespace with only loopback.

**Every commit.** `cargo fmt --all -- --check` and
`cargo clippy --offline --locked --workspace --all-targets --all-features -- -D warnings`
passed on each of the 57 commit trees.

**Full suite** (`cargo test --offline --locked --workspace --all-targets
--all-features --no-fail-fast`):

| Tree | Passed | Failed | Ignored |
| --- | ---: | ---: | ---: |
| Unmodified upstream main `3f5db6d3a` | 1541 | 0 | 4 |
| Rebased fork (code of the final tip) | 1690 | 0 | 4 |

On both trees, upstream's `src/main/tests/smoke.rs` binary aborts (SIGABRT)
after its first two tests whenever more than one test thread runs:
`boots_with_federation_disabled` (`97a637828`) and `smoke` boot servers in
the same process at once. With `RUST_TEST_THREADS=1` all four tests pass. Its
remaining two tests are therefore missing from the counts above; the fork does
not patch the upstream test.

Counts sum every `test result` summary, including nested child-process runs.
The same four upstream tests are ignored on both trees. Every upstream test
that runs on the baseline also runs and passes on the fork, except the four
`decode_apple_userinfo_from_id_token_*` unit tests in `session/sso.rs`, which
the native Apple owner replaces as it did on v1.9.3.

**Upgrade probe.** A debug build of a released fork seeded a fresh database:
two users, a private room, an original message with three edits, a thread
root with three replies of which one is redacted, uploaded device identity
keys, a display name, and a self-deactivated third user. The same old binary
then restarted with the edit purge enabled and purged the two superseded
edits. A debug build of the rebased fork served the database twice. Runs from
`v1.9.3-mindroom.1` (code of `974932e66`) and from `c90e963d7`
(`v1.9.1-mindroom.36-1`) both passed all 53 checks: pre-upgrade access tokens
and device, password login, profile, the newest edit bundled on the original,
the purged edits still gone, the thread count and latest reply after
`rebuild_thread_summaries`, `/messages` history, `/sync` compaction and
incremental sync from a pre-upgrade token, the self-deactivated user still
refused, preserved identity keys, an exact-key retry accepted and a replacement
refused with 403, new writes, and a new edit becoming the bundle. The first
start logged "Short id families verified; the superseded repair's settlement
stands." and "Rebuilt thread summaries. roots=0"; the second ran neither.
Server logs show no panic; their error lines are the deliberate 403s and the
shutdown dangling-reference report the old binaries print as well. Both
binaries used the flake's dynamic RocksDB, and the RocksDB crates are
unchanged, so the probe covers logical compatibility, not a library change.

**Release build.** `cargo build --offline --locked --release --bin tuwunel`
succeeded on the final tree, and the upgrade probe from `v1.9.3-mindroom.1`
passed all 53 checks with that optimized binary as the new server.
`cargo test --offline --locked --workspace --all-features --doc` passed; the
workspace defines no doctests.

**Release tooling.** `scripts/fork_release_tag.py` previews
`v1.9.3-mindroom.2`.

A local build and synthetic upgrade do not prove live federation, a real Apple
identity-provider exchange, production-scale load, a production database
snapshot, cross-architecture packaging, or a remote CI run.

## Next rebase

Follow the v1.9.1 procedure with the v1.9.3 additions, and also:

1. When the base is upstream `main`, check each fork fix upstream took by
   commit, and port what the fork keeps onto upstream's version rather than
   replaying its own (#24 here).
2. Check the formatter pin against the new base before anything else; the
   locked Fenix formatter may lag upstream's formatting.
3. Before upgrading a production database, read `global/fix_short_injectivity`
   and look for "Short id families verified" in the first start's log.
