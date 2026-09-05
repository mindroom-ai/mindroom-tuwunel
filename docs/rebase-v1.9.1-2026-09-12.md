# Rebase on upstream v1.9.1 - 2026-09-12

## Inputs and scope

The starting fork main was `d794b2911cae4fca5b15cd1a9bf0441813bc714d`,
with upstream `v1.9.0` (`5b3669144219d5d4c0774743c84191b476f1b54f`) as
its base. The new base is exactly the upstream `v1.9.1` release commit,
`114bc231622299f9a5da7a60f951e2b0e4144b89`; its annotated tag object is
`03c991a7bf2be1d13410f2127567c439e4a0d419`. The release API and remote
peeled tag were checked independently. Moving upstream development commits
are not the target.

The old fork tip is preserved by `backup/main-before-v1.9.1-20260912`.
The replacement stack is constructed in an isolated worktree, with earlier
rebased and WIP checkpoints preserved. No upstream commit or existing release
tag is rewritten. Publication to fork main is gated on the complete checks
and two independent reviews of the corrected final revision. Pushing main can
trigger existing release automation; no manual deployment or production
access is part of this work.

## Changes now owned by upstream

The decisions below use code and tests in the release, not just PR status.

| Previously carried change | v1.9.1 replacement | Fork disposition |
| --- | --- | --- |
| Default test-database isolation | `fce511c53` and native `default_test_database` coverage | Drop the `src/main/args.rs` delta; retain only the fork harness. |
| Directional pagination bounds and token validation | `7cbcbe5e5`, subsequent refinements, and native pagination fixtures | Drop the entire pagination owner and duplicate fork fixture. |
| Full-state sync for quiet rooms | `eef601a1c` and native `sync_full_state` coverage | Drop the runtime owner and duplicate fork fixture; move its CI-only temporary-directory setting into CI. |
| Fail-closed reads of stored device identities | `b6f52630f` and native corruption fixture | Use upstream's read/deserialize error handling under the fork's identity mutex; remove duplicate fork corruption coverage. |
| One-time-key deletion on device removal | `95ee2bf8` | Call upstream's cleanup helper, retaining only the fork's lock and identity-row deletion. |
| Membership notification recipient targeting | `3818eec81` | Preserve the upstream comparison with the recipient, alongside fork streaming behavior. |
| Full deactivation from Synapse-compatible admin routes | `da501bb3`, `e50b2700` | Preserve full cleanup and add the fork's required deactivation reason. |

The release's contact erasure, configured password-hash cost, cross-signing
validation, login privacy changes, and upstream lint fixes remain in place.
The prior Sliding Sync `num_live` fix remains upstream-owned as well.

The final tree must have no delta from the release in `src/main/args.rs`,
`src/api/client/message.rs`, or `src/api/client/sync/v3.rs`. Their native
regression fixtures, including filtered empty pages and sync-state corruption,
are retained unchanged. The native device-key fixture is the deliberate
exception: its replacement/retry expectations reflect the fork's immutable
identity policy; all four corrupt-byte cases remain intact.

## Retained ownership

The ownership layout uses eight linear commits in place of the previous ten,
without separate fixup, formatting, compatibility-follow-up, or merge commits.

| Order | Owner | Remaining responsibility |
| --- | --- | --- |
| 1 | Fork regression harness | Test crate and shared helpers; use upstream database isolation. |
| 2 | Edit lifecycle | Sync compaction, validated room-local purge, and surviving-edit bundles. |
| 3 | SSO policy | Origin-aware UIAA, administrative rejection, and reason-aware self-reactivation. |
| 4 | Native Apple login | Native endpoint and token validation, using shared SSO completion. |
| 5 | Streaming push | Suppress intermediate updates and evaluate terminal content with recipient rules. |
| 6 | Device identity | Atomic first upload, signature-preserving retries, rejected replacement, removal and reuse. |
| 7 | Fork CI and releases | Fork checks, release assets, container publication, and runner temporary-directory setting. |
| 8 | Documentation | Current inventory and this record; earlier rebase records remain unchanged. |

See [FORK_CHANGES.md](../FORK_CHANGES.md) for the retained feature rationale.
`Cargo.lock` inherits every upstream dependency unchanged and adds only the
fork regression-harness package.

## Tests for semantic overlaps

Four negative checks distinguished working composition from a plausible but
incorrect replay:

- SSO: with the old shallow deactivation calls, the new real-router assertions
  found both administratively deactivated accounts still joined to their rooms.
  Restoring upstream `full_deactivate`, with the fork reason and v1 `erase`
  flag, made the room-departure checks pass. Existing reason, erasure, and
  SSO reactivation tests remain enabled.
- Push: deliberately comparing invite membership with the sender caused the
  captured gateway request to omit the required `user_is_target: true`.
  Keeping upstream's recipient comparison made this check pass alongside the
  existing terminal-stream gateway fixture.
- Device identity: an old-to-new synthetic database upgrade against the
  new-base candidate before replaying the device-identity owner reached a
  replacement-key upload returning 200 instead of the required fork 403.
  Earlier checks had already proved database
  startup, tokens, password login, history, edit bundles, and exact key retries.
  The retained device owner is checked against that same policy boundary.
- Edit purge: a pre-fix isolated runtime probe and permanent router regression
  both detected deletion of current room state and loss of one room's only
  valid edit after a replacement-shaped event was sent in another room. These
  were existing fork defects, not upstream or rebase-introduced changes.
  The correction validates visible replacement boundaries before room-local
  grouping and preserves invalid or unverifiable candidates. Real database
  tests cover state, sender, room, type, original-event identity and read
  failures, malformed new content, and continued encrypted-edit support. A
  separate regression preserves an older valid edit when a newer replacement
  is malformed; removing the new-content guard made that test fail because
  the valid PDU was deleted. The corrected tests pass, including another purge
  cycle and a normal-edit control proving that deletion still works.

Device tests also exercise competing first uploads, preserved signatures,
malformed client input, all four upstream stored-row corruption cases,
deletion, one-time/fallback-key cleanup, and device-ID reuse. The configured
password-hash-cost test runs with the full suite. Native Apple token/JWKS unit
and browser callback fixtures are retained; no live Apple authentication is
claimed.

## Verification method

The unmodified release baseline and every retained commit tree are checked
with the project's Rust 1.95.0 and locked formatter, nightly-2026-08-05:

Enter the locked development shell with `nix develop .#dynamic` before these
commands; bare Cargo outside that shell does not select the documented formatter.

```sh
cargo fmt --all -- --check
cargo clippy --offline --locked --workspace --all-targets --all-features -- -D warnings
cargo test --offline --locked --workspace --all-targets --all-features
```

Fetch locked dependencies before running offline. Tests run serially inside
a private Linux user/mount/network namespace with loopback enabled, no external
interfaces, isolated writable database directories, and no inherited server
configuration overrides. Host listeners, production databases, and live
infrastructure are not used.

Each candidate is staged before checking. The verification records its parent
commit, exact index tree, source fingerprint, command, exit code, and test
summaries. Only that identical tested tree is then committed. This attributes
all three gates to every resulting commit without substituting a passing tip
for a passing history. Full logs and synthetic databases stay outside the
repository.

The completed baseline and final runtime/CI commit-tree gates are below.
Each final commit has its own fresh attribution; the table does not substitute
the earlier checkpoint's results for the rewritten history.

| Tree | Reported test passes | Failed | Ignored | Pinned format / Clippy |
| --- | ---: | ---: | ---: | --- |
| Unmodified v1.9.1 | 1217 | 0 | 4 | Pass / Pass |
| Harness | 1217 | 0 | 4 | Pass / Pass |
| Edit lifecycle | 1250 | 0 | 4 | Pass / Pass |
| SSO policy | 1264 | 0 | 4 | Pass / Pass |
| Native Apple login | 1281 | 0 | 4 | Pass / Pass |
| Streaming push | 1290 | 0 | 4 | Pass / Pass |
| Device identity | 1291 | 0 | 4 | Pass / Pass |
| CI and releases | 1291 | 0 | 4 | Pass / Pass |

Counts sum Rust test summaries, including nested native-test output; they are
not counts of unique logical tests. The same four upstream tests are explicitly
ignored at the baseline and every retained snapshot. The documentation owner
must pass the same three gates on its exact tree before it is committed; its
attribution is included in the final history audit.

Pinned Clippy encountered a Rust 1.95 incremental-cache internal compiler error
in an unchanged upstream API file when replaying the push owner. Clearing only
that package's generated artifacts for the active target and repeating all
three gates is the verification-environment remedy. Unsuccessful runs are
retained as evidence, never used as passing gates. No source or CI workaround
is carried for the compiler crash.

The four fork-owned workflows pass actionlint with ShellCheck integration.
`docker/bake.sh` passes `bash -n` and ShellCheck with the established
`SC2034,SC2154,SC2086` exclusions. Twelve impromptu release-tag cases pass,
including numeric ordering, prefix handling, existing-tag reuse, invalid
versions, and the new workspace base version. The read-only preview selects
`v1.9.1-mindroom.1`; it creates no tag or release. The release Rust pin and
both CI formatter commands match the repository's toolchains.

The inherited upstream `main.yml` separately reports 22 undefined-input
actionlint diagnostics. Running the exact release's file through the same
linter produces the identical diagnostic messages and snippets. The fork
changes only its automatic triggers, not those expressions. No unrelated
upstream workflow fix is carried.

Current nightly-2026-09-12 formatting and strict all-target/all-feature Clippy
(Rust 1.100.0-nightly) passed the corrected edit-owner tree, using the release's
own nightly flags from `docker/bake.hcl`. An earlier extra check found one
unnecessary event-ID clone
in the fork's edit-compaction loop. Moving the already-owned ID instead passes
the linter without changing the map key or borrowed event. That one-line
cleanup belongs to the edit owner, not a follow-up commit; all affected commit
trees passed fresh full gates after it was folded into the history. No
upstream-only source or lint workaround is added.

An earlier rebase checkpoint passed a synthetic upgrade from the saved fork
binary identifying as
`1.9.0-9 (v1.9.0-9-g34252092c1)` to the tested v1.9.1 runtime tree, followed
by another v1.9.1 restart. This is a fresh synthetic database, not a production
snapshot or the exact old-main tip: the saved binary precedes the final
quiet-room full-state commit. The test checks original and new access tokens,
password login, history, edit bundles, stored identity keys, unchanged-signature
retries, rejected key replacement, and new writes/sync after both transitions.
This first smoke run used the current dynamic RocksDB library for both servers;
it proves logical server compatibility, not the old-to-new engine transition.
The final binary run must use each binary's matching RocksDB library and verify
the loaded library independently, since the release updates that dependency.

The corrected final binary must also pass the isolated purge-boundary probe:
current state stays readable, a foreign-room replacement cannot remove the
original room's surviving edit, and a valid obsolete-edit control is purged.
An additional synthetic encrypted-event case redacts the newer replacement
before purging and checks that the older valid edit remains. The built and
exercised executable must match by digest; source-level tests alone do not
substitute for this final runtime check.

Final handoff also requires documentation tests, an optimized default-feature
binary build, and the upgrade fixture against that final binary:

```sh
cargo test --offline --locked --workspace --all-features --doc
cargo build --offline --locked --release --bin tuwunel
```

Those gates run after the documentation tree is frozen; their results belong
to the final handoff and external evidence. A local build or synthetic upgrade
does not prove live federation, a real Apple identity-provider exchange,
production-scale load, cross-architecture packaging, or a remote CI/deployment.

## Next rebase

1. Start from the then-current fork main in an isolated worktree. Preserve a
   backup and record exact old/new release commits before replaying anything.
2. Compare each owner with implementations and tests in the new release. Drop
   absorbed fixes and duplicate fixtures; a merged PR alone is not proof that
   a particular release contains the fix.
3. Keep the harness first, Apple completion after SSO policy, and CI/docs last.
   Resolve semantic overlaps in favor of upstream behavior, retaining only
   genuine fork policy. Put required adaptations and regression tests in the
   owning feature commit.
4. Use explicit old/new bases and `--no-update-refs` when rebasing; automatic
   ref updates are enabled in the development environment. Never rewrite
   existing published release tags. Preserve backup refs before a history edit.
5. Run formatting, strict Clippy, and the complete workspace/all-target/
   all-feature suite on every resulting tree. A changed tree or rewritten
   parent requires fresh attribution and checks. Add a failing regression
   when the semantic resolution is uncertain.
6. Check the final range-diff, clean worktree, exact release ancestry, unchanged
   upstream-owned files, historical records, privacy, release tooling, and
   old-to-new database compatibility before deciding whether to publish.

Remaining custom lifecycle and protocol policies are not assumed suitable for
upstream merely because they are useful in the fork. This rebase opens no PRs.
