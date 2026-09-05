# Rebase on upstream v1.9.0 - 2026-09-05

## Goal

Rebase the fork from its latest published `main` onto upstream `v1.9.0`, while
preserving each still-relevant fork behavior, dropping work already absorbed
upstream, and proving both textual and semantic correctness. The release tag,
not upstream `main`, is the target.

## Verified inputs

- Fork tip of record: `origin/main` = `59c132f21a5f7722977cb39937d8293aa30b7f86`.
- Fork base before this rebase: upstream `v1.8.2` =
  `9099defe5ee340140ca0d56e478751637f34354d`.
- Upstream target: annotated tag `v1.9.0` =
  `27102f73efd5f64eb826c51f2fa400baa7f7b5df`, peeled release commit
  `5b3669144219d5d4c0774743c84191b476f1b54f`.
- Remote tag enumeration identifies `v1.9.0` as the newest stable version tag.
  Upstream `main` is newer than the release, so it is intentionally not the
  rebase target.
- The starting fork delta is 10 linear commits and 67 changed files. The
  upstream release delta is 391 commits and 639 changed files.
- The histories overlap in 28 files.

## Safety and history design

- `backup/origin-main-before-rebase-v1.9.0-20260905` preserves the exact
  starting fork tip.
- Work occurs only on `mindroom/rebase-v1.9.0-20260905` in a persistent,
  isolated worktree. Local `main`, the starting feature branch, and remote refs
  remain untouched.
- Every rebase invocation uses `--no-update-refs` because
  `rebase.updateRefs=true` is configured globally.
- Replay starts from the fetched `origin/main`, with `v1.8.2` as the explicit
  old base and `v1.9.0` as the explicit new base.
- No merge commit or aggregate squash is used. Feature-level commits remain
  independently inspectable and bisectable.
- Before a public write, audit fork-added content, commit messages, and
  generated metadata for private cross-references. Incidental names inherited
  unchanged from upstream are not a reason to carry fork-only renaming patches.
- Carry changes needed by fork behavior and its compatibility with the release.
  Leave unrelated upstream tests, generated fixtures, and test tooling alone;
  isolate local verification from host-specific configuration instead.

## Absorption decision

The final fork commit, `59c132f21` (the Sliding Sync `num_live` correction), is
dropped. Upstream release commit `704b6c480` reproduces its propagation of
`previous_connection_pos`, preservation of event positions through timeline
filtering and bundling, returned-timeline live-suffix calculation, and all five
focused unit cases. A one-commit `git range-diff` between the two changes shows
only commit metadata and two blank-line differences in that implementation.
The release also includes related upstream refinements that separate activity
domains, advance only delivered-room cursors, and commit sliding-window ranges
atomically. Replaying the fork copy would duplicate and potentially regress
that maintained upstream sequence.

The other nine fork commits have no patch-id match in `v1.9.0` and remain in
scope unless conflict inspection proves semantic absorption.

## Overlap review surface

The same-file intersection is:

- `.github/workflows/main.yml`
- `Cargo.lock`
- `README.md`
- `src/admin/user/mod.rs`
- `src/api/client/keys/upload_keys.rs`
- `src/api/client/session/mod.rs`
- `src/api/client/session/sso.rs`
- `src/api/client/sync/mod.rs`
- `src/api/client/sync/v5.rs`
- `src/api/client/sync/v5/rooms.rs`
- `src/api/router.rs`
- `src/core/config/check.rs`
- `src/core/config/mod.rs`
- `src/core/matrix/event.rs`
- `src/core/matrix/event/relation.rs`
- `src/database/map/remove.rs`
- `src/database/maps.rs`
- `src/main/args.rs`
- `src/service/media/data.rs`
- `src/service/media/mod.rs`
- `src/service/mod.rs`
- `src/service/pusher/mod.rs`
- `src/service/pusher/send.rs`
- `src/service/pusher/tests.rs`
- `src/service/services.rs`
- `src/service/users/device.rs`
- `src/service/users/mod.rs`
- `tuwunel-example.toml`

## Semantic reconciliation matrix

- Edit compaction and purge: compare the fork deletion set with the release's
  transactional timeline append and split relation-index modules. Confirm the
  surviving edit remains bundleable and dangling relation rows remain safely
  skipped.
- SSO/UIAA: preserve fork origin tracking, self-reactivation, and exact IdP
  flow selection while inheriting upstream account-locking, LDAP, and OIDC
  changes. Every new deactivation caller must record the correct reason.
- Native Apple exchange: retain the fork endpoint and token verification while
  rebuilding shared completion logic from the release's current SSO path.
- Stream push completion: compose one-shot terminal delivery and suppression
  with upstream badge refresh, retry, failure propagation, and extracted push
  handlers.
- Device identity keys: preserve immutable existing-device keys and deletion
  cleanup while adopting transactional one-time-key upload and token-index
  changes.
- Message pagination: preserve strict token parsing and directional `to`
  clamping against the release's unchanged pagination core.
- Sliding Sync: inherit the upstream implementation and its post-landing
  follow-ups; do not carry a duplicate fork patch.
- CI and release automation: retain fork-only triggers and release assets while
  incorporating upstream dependency, packaging, and workflow changes.

## Execution gates

1. Establish a clean source baseline in the pinned Rust 1.95 Nix shell. The
   user-level Cargo wrapper setting must be disabled with `RUSTC_WRAPPER=`;
   test database path overrides must be unset.
2. Rebase the linear fork stack with `--no-update-refs`, resolve one commit at
   a time, and drop only the absorbed Sliding Sync commit.
3. After each replayed code commit, run
   `cargo check --locked --workspace --all-targets --all-features` in the same
   controlled shell. A failure stops the replay.
4. For every semantic adaptation whose intended result is not already pinned,
   write a focused test first, observe the expected failure, implement the
   smallest correction, and observe the pass.
5. At the final tip, run formatting, clippy with warnings denied, all workspace
   tests with all targets and features, and the release build. Run focused fork
   regression binaries separately so their results are explicit.
6. Compare old and new stacks with `git range-diff`, audit the final tree and
   commit messages, confirm the release commit is an ancestor, and confirm
   `main` and `origin/main` did not move.

## Results

### History outcome

- The rebase completed on `mindroom/rebase-v1.9.0-20260905` with release commit
  `5b3669144219d5d4c0774743c84191b476f1b54f` as both an ancestor and the exact
  merge base.
- The nine still-relevant fork features remain separate commits. The absorbed
  Sliding Sync change was omitted, and upstream commit
  `704b6c4807f812fd441238a16b053ec1c3e90cee` remains in the ancestry.
- Before this evidence commit, the release range contains 11 linear commits:
  nine rebased feature commits and two fork compatibility follow-ups. Including
  this evidence commit, the final stack contains 12 commits and no merges.
- `git range-diff` maps all nine retained behaviors to the new stack. The
  device-key change appears as one removed/one added entry because its
  read/compare/write sequence was deliberately replaced by an atomic service
  operation. The tenth old commit appears only as removed because it is the
  absorbed Sliding Sync change.
- `origin/main` and its safety ref both remain at
  `59c132f21a5f7722977cb39937d8293aa30b7f86`. Local `main` remains at
  `8375c6d345a3965f9480c579fc30f4a5f608fa0e`; the starting feature ref also
  remains unchanged. No remote ref was updated.

### Conflict and semantic decisions

- Scope cleanup: omitted the unrelated upstream test-name normalization and
  default-listener-port changes. The password-reset test helper, Docker test
  plumbing, generated documentation, and result fixtures remain byte-for-byte
  identical to the release. No name-normalization scripts or tests remain in
  the fork. The listener defaults are unchanged, while the fork's existing
  test-database isolation is retained.
- Edit purge: composed fork configuration checks and purge behavior with the
  release's newer relation and timeline layout. A real-service integration
  test proves that the retained edit is still bundled after superseded edits
  and their eligible sidecars are purged.
- SSO and UIAA: retained origin repair, deactivation reasons, administrative
  rejection, and self-reactivation while using the release's atomic identity
  session commit. The callback test covers session reuse, rejection, recovery,
  and serialization of two concurrent commits for one identity.
- Native Apple exchange: retained the native route, audience/nonce/JWKS checks,
  and ID-token fallback while routing account completion through the same
  atomic SSO helper as browser callbacks.
- CI: kept fork-only checks and publishing. Verification found and corrected a
  stale Rust 1.91.1 release pin to the repository-required 1.95.0, and removed
  the deprecated Buildx `install` input because every build explicitly selects
  its named builder.
- Push delivery: composed stream-terminal suppression and one-shot delivery
  with the release's badge refresh, retry propagation, and extracted push
  handlers. Seventeen focused unit cases cover main/thread notification keys,
  relation normalization, encrypted events, and sender-independent protocol
  classification.
- Device identity keys: a deterministic 16-request router test exposed the old
  read/compare/write race with both competing identities accepted (success
  counts `(2, 8)`). The final service serializes updates per user/device,
  rechecks device existence after waiting, shares that lock with removal, and
  atomically classifies insert/unchanged/conflict. The green case permits all
  eight retries for exactly one identity and rejects all eight for the other.
  The same test also covers signature-preserving retries, rotated-key
  rejection, malformed stored data, key cleanup, and device-ID reuse.
- Message pagination: the real-router test covers forward and backward
  traversal, a `to` position between events, exact exclusion at the bound, and
  malformed `from` and `to` tokens.
- Test infrastructure: an upstream smoke test needs default port 8008, which
  was occupied on the verification host. The final stack leaves that upstream
  listener default unchanged. Run the suite in a fresh network namespace with
  loopback enabled and no external interfaces, keeping host services untouched.
  The pre-existing fork test-database isolation remains because the fork's
  integration harness depends on it.

### Verification evidence

- Every replayed code commit passed
  `cargo check --locked --workspace --all-targets --all-features` before the
  next commit was applied.
- `cargo fmt --all -- --check` passes in the pinned Nix development shell.
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`
  passes. The first run identified Rust 1.95-only diagnostics in rebased code
  and tests; those were corrected locally, and the complete gate was rerun from
  the workspace root.
- `RUST_TEST_THREADS=1 cargo test --locked --workspace --all-targets
  --all-features` passes. This includes all fork-specific integration tests,
  all upstream integration and unit binaries, state-resolution suites, and
  bench smoke targets. The final run exited successfully with only the
  repository's explicitly ignored tests reported as ignored. Local verification
  uses a loopback-only network namespace so upstream default ports are free.
- `cargo build --locked --release -p tuwunel` passes under Rust 1.95.0; the
  optimized native-target build completed in 3 minutes 25 seconds.
- The sole retained shell change, `docker/bake.sh`, passes `bash -n` and
  ShellCheck under the repository's established unused-variable exclusions;
  its two excluded unquoted-expansion findings are unchanged from the release
  base. No upstream shell normalization patch is retained.
- All four fork-owned workflows pass actionlint. A static assertion confirms
  the release workflow toolchain equals `rust-toolchain.toml` (`1.95.0`). A
  repository-wide actionlint run still reports undefined reusable-workflow
  inputs in `main.yml`, custom self-hosted runner labels and existing shell
  quoting in `publish.yml`, and the same custom label in `stats.yml`; running
  actionlint directly on those three files from tag `v1.9.0` produces the same
  categories.
- The only non-code diagnostic emitted by successful Nix gates is a Nixpkgs
  deprecation warning about the release base's nested RocksDB `buildInputs`.
- Final whitespace, ancestry, tree/path, commit-message, and
  private-cross-reference checks are required to pass after this evidence file
  is committed. Inherited upstream identifiers are left unchanged; newly added
  fork content must not introduce private references. Final check results are
  reported in the handoff.
