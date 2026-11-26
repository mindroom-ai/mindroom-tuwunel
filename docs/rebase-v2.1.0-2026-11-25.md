# Rebase on upstream `v2.1.0` - 2026-11-25

## Inputs and scope

The starting fork `main` was `09ecc2489371769f286b3c0d6024d5ef164efeac`
(`v2.0.0-mindroom.16`, including `#192`), 106 commits on upstream `main`
`f7dc0770b5c492fcc8f8de855a8c4094f67d6b7b`. The new base is the upstream
`v2.1.0` release tag at `4b0d90d9743e3f084815b37dcd986bb7efcea1fd` ("Bump
2.1.0.", 2026-11-25), which is 3 commits past `f7dc0770b` (`f7dc0770b` is its
ancestor).

Open fork PR `#150` (`pdu-size-without-unsigned`) is out of scope.

The starting fork `main` `09ecc2489` (`v2.0.0-mindroom.16`) is preserved as
`backup/main-09ecc2489-before-v2.1.0-20261125`. No upstream commit or published
release tag is rewritten. The workspace version moves from `2.0.0` to `2.1.0`,
so the fork release tooling previews `v2.1.0-mindroom.1`.

The result is 107 commits on `v2.1.0` (`4b0d90d97`): all 106 fork commits
replayed unchanged (`=` in `git range-diff`), plus this documentation commit.

## What upstream changed

The 3 upstream commits between `f7dc0770b` and `v2.1.0` touch no fork-modified
source files and introduce no database schema or wire-protocol changes:

- `986f447cdd` (`src/database/pool/configure.rs`, +143/−19): sizes database
  pool worker groups by storage queue ID so hosts pinned to a subset of CPUs
  with queue IDs above the highest core ID keep working queues (fixes upstream
  `#739`), plus four unit tests.
- `0c2ff3441f` (`docker/Dockerfile.playwright`, +5/−4): anchors the Playwright
  port overlay `sed` pattern on the port alone.
- `4b0d90d974` (`Cargo.toml`, `Cargo.lock`, `RELEASE.md`, `rpm/tuwunel.spec`):
  bumps the workspace version and all 14 `tuwunel*` package entries in
  `Cargo.lock` from `2.0.0` to `2.1.0`, and writes the `2.1.0` release notes.

None of the fork's 21 open upstream PRs were merged between `f7dc0770b` and
`v2.1.0` (the 14 upstream PRs merged earlier were already accounted for—dropped
or trimmed to remainders—in the `f7dc0770b` rebase).

## Outcome per fork commit

| Outcome | Count | Commits |
| --- | ---: | --- |
| Dropped | 0 | — |
| Reduced to remainder | 0 | — |
| Adapted | 0 | — |
| Carried unchanged (`=` in `git range-diff`) | 106 | All 106 commits from `f7dc0770b..09ecc2489` (ending at `133c0af4f`) |
| New in this rebase | 1 | This documentation commit |

The only files touched by both `f7dc0770b..v2.1.0` and `f7dc0770b..09ecc2489`
are `Cargo.toml` and `Cargo.lock`, on disjoint lines. Every commit replayed with
zero merge conflicts, and `git diff --stat 09ecc2489..133c0af4f` touches only
the 6 files changed by the 3 upstream commits (`Cargo.lock`, `Cargo.toml`,
`RELEASE.md`, `docker/Dockerfile.playwright`, `rpm/tuwunel.spec`,
`src/database/pool/configure.rs`). All 14 `tuwunel*` packages in `Cargo.lock`
are at `2.1.0`, and `mindroom_rebase_tests` remains at `0.0.0` with workspace
path dependencies.

## Verification

- **Replay and lockfile audit (measured locally):**
  - `git rebase --no-update-refs --no-autostash --onto v2.1.0 f7dc0770b`
    replayed 106/106 commits cleanly with 0 conflicts to `133c0af4f`.
  - `git range-diff f7dc0770b..09ecc2489 v2.1.0..133c0af4f` shows `=` for all
    106 commits.
  - `Cargo.lock` lists all 14 `tuwunel*` crates at `2.1.0` and zero remaining
    `2.0.0` references.
  - `python3 scripts/fork_release_tag.py` read-only preview outputs
    `base_version=2.1.0`, `release_iteration=1`,
    `release_tag=v2.1.0-mindroom.1`, `reused_tag_at_head=false`.
- **Formatter, Clippy, full test suite, and release binary (pending at commit
  time):** will run via `MindRoom CI` on GitHub Actions (x86_64, Rust `1.95.0`,
  `nightly-2026-11-20` rustfmt) once `rebase/v2.1.0` is pushed, and must pass
  before swapping `main`.
- **Inferred / not re-run locally:**
  - Per-commit Clippy was not re-run across all 106 commits: commits `1..=102`
    already passed per-commit Clippy on `f7dc0770b` (2026-11-24), `#192` passed
    `MindRoom CI` on `main` (run `45524083603`), and `f7dc0770b..v2.1.0`
    changes no Rust public APIs, signatures, or crate features.
  - A fresh synthetic upgrade probe was not re-run: `f7dc0770b..v2.1.0` makes
    no database schema, column-family, or serialization changes (schema stays
    version 17), and the `v2.0.0-mindroom.14` → fork tip `04b2fc9fb` (on
    `f7dc0770b`) probe passed 65/65 checks on 2026-11-24.
  - Not verified: live MatrixRTC calls, live federation, and the aarch64
    release binary.

## Next rebase

Continue following the `f7dc0770b` rebase checklist (`docs/rebase-upstream-f7dc077-2026-11-24.md`
"Next rebase"), plus the Rust `1.95.0` CI toolchain lesson from `v2.0.0-mindroom.15`:
run `cargo clippy --keep-going` under the MSRV (`rust-toolchain.toml`, `1.95.0`)
whenever a rebase changes crate dependency graphs or async future layouts, as
Rust `1.100` can accept query depths that `1.95.0` overflows.
