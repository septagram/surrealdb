# customware: v3.1.5 → v3.2.4

Record of the `/customware update` that re-anchored this fork from upstream
`v3.1.5` onto `v3.2.4`.

Plan: `26c-lively-uphold-wiry`

## Upstream range

196 commits (`git log v3.1.5..v3.2.4 --oneline`).

**The upstream release lines are disjoint** — `v3.1.5` is *not* an ancestor of
`v3.2.4`, and `v3.2.4` is not an ancestor of `v3.3.0-beta.2`. Each release tag
sits on its own release branch, pushed from an internal repo (note the
divergent PR numbering: the 3.1 line carries `(#353)` where the public `main`
mirror carries `(#7357)` for the same fix). The public `main` branch is stale
relative to the release tags.

So this update is a **re-anchor onto a different line**, not a fast-forward,
and `git log v3.1.5..v3.2.4` includes the 3.1-line tail as noise.

## Conventions changed this update

### No snapshot branch

Skill §3 Step 0 says to create a snapshot branch named after the upstream
version (`v3.1.5`). Two reasons not to:

1. Upstream tag `v3.1.5` already exists, so `refs/heads/v3.1.5` would make
   every bare `v3.1.5` reference ambiguous.
2. **The branch is redundant.** The customware tag already pins the exact
   pre-update commit, and did so on the previous update too:

   ```
   main tip                       = 47a2048db
   tag v3.1.5-customware.3        = 47a2048db   <- identical

   branch v3.1.0-alpha            = aca7ba6b8
   tag v3.1.0-alpha-customware.3  = aca7ba6b8   <- identical
   ```

`v3.1.5-customware.3` is the checkpoint for the pre-update state. If a
maintainable 3.1 line is ever wanted, the branch costs nothing later:
`git branch fork/v3.1.5 v3.1.5-customware.3`.

**Skill fix filed:** §3 Step 0 should reference the existing customware tag
instead of minting a duplicate branch.

### Patch anchor is a tag, not `upstream/main`

Regenerated patches use `git diff v3.2.4..HEAD` rather than the skill's literal
`git diff upstream/main..HEAD`, because `upstream/main` is on a different,
older line than the release tag we track.

## Per-entry outcomes

### 001-dorsid-first-class-record-ids — **re-implemented (partially)**

`git apply` failed outright. `git apply --3way` auto-merged 20 of 27 files and
left 7 conflicts, all resolved by hand:

| File | Resolution |
|---|---|
| `doc/alter.rs` | Real merge — see below |
| `doc/create.rs`, `insert.rs`, `relate.rs`, `upsert.rs` | Took upstream's call-site signature |
| `catalog/test.rs` | Encoded-size assertion rebased |
| `syn/parser/stmt/define.rs` | Import union |

**Upstream rewrote `generate_record_id` in 3.2.** It gained an `id`-field
`DEFAULT` evaluation path with auth-level clamping (`AuthLimit`), kind coercion
via a new `coerce_id_key`, and kind-aware synthesis via a new
`generate_typed_id`. The signature changed from `(ctx)` to `(stk, ctx, opt)`.

Resolution keeps **upstream's pipeline wholesale** and grafts the fork's dorsid
dispatch into its synthesis point:

- `generate_default_id` now takes `id_kind` and its `IdGeneration::Default` arm
  delegates to upstream's `Self::generate_typed_id(&tb, id_kind)`.
  001 originally used `RecordId::random_for_table(tb)`; upstream's version is a
  strict superset (it also synthesises `uuid` ids and singleton-literal ids),
  so delegating preserves upstream behaviour for non-Sid/Rid tables rather than
  regressing it.
- Sid/Rid mint `RecordIdKey::Number(i64)` and then flow through upstream's
  `coerce_id_key` like any other key, so a declared `id` field kind still
  validates them.

Other notes:

- `catalog/test.rs` encoded-size: 001 moved it `151 → 153`, so `id_generation`
  costs 2 bytes. Upstream v3.2.4 is at `167`, so the merged value is **169**.
- `syn/parser/stmt/define.rs`: v3.1.5 already imported `Kind` and upstream
  dropped it, so 001 only ever added `IdGeneration`. Re-adding `Kind` would
  have been an unused import.

**Patch regenerated** (the entry drifted).

#### Revision collision on `TableDefinition` — **a real data-compat bug, not just a test break**

001 declared `id_generation` as `#[revision(start = 3)]`, correct against v3.1.5.
Upstream then bumped the same struct to revision 3 in the 3.2 line, using it for
the transactional `cache_lives_ts` live-query cache key. **Both fields claimed
revision 3**, so any `TableDefinition` written by upstream 3.2.x decoded at the
wrong offset in this fork:

```
A deserialization error occured: Invalid revision `0` for type `IdGeneration`
```

Fixed by moving the field to `#[revision(start = 4)]` and bumping the struct to
revision 4. Upstream's revision-3 bytes now decode with `IdGeneration::default()`.

**Standing hazard for every future update:** this collision only appears when
upstream and the fork extend the same revisioned struct, and the stored patch
cannot encode "pick the next free revision". A note lives on the field itself in
`catalog/table.rs` — re-check it against upstream's `revisioned(revision = N)` on
every rebase and bump again if upstream has consumed 4.

Advancing the write format past upstream's then broke the byte-exact re-encode
assertion against the frozen `v3_2_2` fixtures. `compat/tests.rs` documents the
remedy for exactly this ("capture a new `vX_Y_Z` snapshot and move the tag
here"), and frozen files must never be mutated, so a fork snapshot
`compat/v3_2_4_customware.rs` was generated (124 fixtures), registered, added to
all 124 `compat_test!` version lists, and given the current-format tag. `v3_2_2`
was demoted to decode-and-equals exactly as `v3_1_0` and `v3_1_1` were before it.
Wire-stability coverage is preserved rather than disabled.

#### Dropped from 001: the `pprof` cfg-gate — **upstream absorbed**

001 carried a fork-local fix moving `pprof` into a `[target.'cfg(unix)'.dev-dependencies]`
section, because pprof's profiler uses Unix-only libc types and does not build
on Windows.

**Upstream v3.2.4 removed `pprof` entirely** from both the workspace root and
`surrealdb/core/Cargo.toml`. The 3-way merge resurrected our gated block
against a `workspace.dependencies` entry that no longer exists, which failed
the build with:

```
error inheriting `pprof` from workspace root manifest's `workspace.dependencies.pprof`
`dependency.pprof` was not found in `workspace.dependencies`
```

The workaround is obsolete. Dropped it to match upstream. The fork's manifest
delta is now only the `dorsid` dependency additions.

### 002-prune-deps-for-embedded — **re-implemented (partially)**

`git apply --3way` **aborted atomically**: upstream's `gql/` → `graphql/` rename
(#519 — the name `gql/` now holds the new ISO GQL lexer/parser/ast surface)
means `surrealdb/core/src/gql/schema.rs` no longer exists, and `git apply` rolls
back the entire application when any target file is missing. Re-run with
`--exclude='surrealdb/core/src/gql/*'`, then the three
`#[cfg(feature = "auth")]` gates were placed by hand into `graphql/mod.rs` and
`graphql/schema.rs`. The target sites were identical after the rename.

Manifest conflicts, all resolved **toward upstream** where upstream had since
solved the same problem independently:

| Conflict | Resolution |
|---|---|
| Root `Cargo.toml` default features | Kept upstream's `default = ["surrealdb-server/default"]`. 3.2 refactored the binary to inherit the server crate's list instead of duplicating it, superseding the entry's explicit array. The entry's own `surrealdb/server/Cargo.toml` change already puts `server` in that list, so the hosted build is unchanged. |
| `surrealdb/Cargo.toml` → `radix_trie` | Dropped. Upstream replaced radix_trie with `ahash::HashSet` (#370), so re-adding it would inherit from a workspace dependency that no longer exists. |
| `surrealdb/core/Cargo.toml` features | Kept the entry's lean `default = ["kv-mem"]` and its `server` umbrella. |

**Judgement call on the new `gql` feature.** Upstream added `gql` (ISO GQL) and
put it in core's defaults. It is deliberately **not** added to the entry's
`server` umbrella: the server crate's own default list already names `gql`
explicitly, so the hosted build still gets it, and embedded builds shed it
exactly as the entry intends. Adding it to the umbrella would have been
inventing policy for a feature the entry never contemplated.

**Patch regenerated** (the entry drifted).

#### Dropped from 002: the `pprof` cfg-gate (again) — **upstream absorbed**

The same fork-local pprof workaround described under 001, mirrored for
`surrealdb/Cargo.toml`. Both entries carried it; both resurrected a dangling
`workspace = true` inheritance and failed the build identically. Both removed.

This is worth naming as a pattern: **`git apply --check` passing is not
evidence of anything.** Three of 002's hunks turned out to be fork-local
workarounds upstream has since absorbed or obsoleted (`pprof`, `radix_trie`,
and the manifest duplication that `surrealdb-server/default` now solves). Only
a real build surfaced them. Budget for that on the next update.

### 003-agents-workflow-context — **applied cleanly**

`AGENTS.md` only; upstream has no such file. Patch left byte-exact as the
historical record.

## Environment note: `cargo make fmt` on Windows

`cargo make fmt` fails on this machine before running anything:

```
[cargo-make] ERROR - Error while evaluating script for env: SURREAL_NIGHTLY_RUST_TOOLCHAIN, exit code: 1
cat: '$CARGO_MAKE_WORKSPACE_WORKING_DIRECTORY/rust-toolchain.nightly': No such file or directory
```

`rust-toolchain.nightly` exists — cargo-make simply isn't expanding
`${CARGO_MAKE_WORKSPACE_WORKING_DIRECTORY}` in the lookup script. Environmental,
not caused by any customware entry, and it will recur on every Windows run.

The env var is guarded by `condition = { env_not_set = [...] }`, so supplying it
directly bypasses the broken script:

```bash
export SURREAL_NIGHTLY_RUST_TOOLCHAIN=$(cat rust-toolchain.nightly)
cargo make fmt
```

This update needed **no formatting commit** — `cargo make fmt` produced zero
changes, so the hand-merged conflict resolutions already matched rustfmt. (The
previous update needed `194d1f5ce` for this.)

## No upstream absorption of the entries' core intent

Scanned the 196-commit range for overlap:

- **001**: no first-class record-ID generation landed upstream. The record-id
  commits in range are unrelated (`restore ASSERT on the record id field`,
  `Fix duplicate edge record ID`, `record-id point lookup`, `store record id
  inline`). The last of those is what forced the `generate_record_id` rewrite.
- **002**: no embedded-oriented feature gating landed. Upstream did trim some
  dependency defaults (`build(deps): trim unused default features (sysinfo,
  tar, zstd, otlp)`, `drop radix_trie`), which overlaps 002's *goal* but not
  its mechanism — the `server` umbrella feature is still fork-local.

## Patch regeneration

001 and 002 both drifted and were regenerated; 003 applied cleanly and its
`.patch` is left byte-exact as the historical record, per skill §3 Step 4.

**Attribution was not a clean commit-range split.** Two fix commits landed after
the three re-apply commits (matching the previous update's shape, where
`ec96508e2` / `38b5dea1d` / `ea32547ff` also followed the re-applies), and one of
them straddled two entries. Ownership was resolved per file:

| Source | Attributed to |
|---|---|
| `Fix TableDefinition revision collision` (all 5 files) | 001 |
| `Fix language-test wiring…` → `language-tests/Cargo.{toml,lock}` | 002 (its `auth` gate) |
| `Fix language-test wiring…` → `info/subquery.surql` | 001 (its `id_generation` field) |

Only three files are genuinely shared between 001 and 002 — `Cargo.toml`,
`surrealdb/core/Cargo.toml`, `surrealdb/core/src/kvs/ds.rs`. Those were split by
commit range so each entry carries only its own hunks:

```bash
# per entry: final state of exclusively-owned files, plus own hunks on shared files
git diff v3.2.4 HEAD       -- <owned files>  ':(exclude)customware'
git diff v3.2.4 ffe002749  -- <shared files> ':(exclude)customware'   # 001's share
git diff ffe002749 HEAD    -- <shared files> ':(exclude)customware'   # 002's share
```

**Composition was verified, not assumed.** Applying 001 → 002 → 003 onto a clean
`v3.2.4` worktree reproduces this tree byte-for-byte: `git diff` between the
patched baseline and `HEAD`, excluding `customware/`, is empty.

Two traps worth remembering for the next update:

- The naive range `v3.2.4..<001 tip>` includes the `Restore customware/` commit,
  so the first regeneration attempt embedded the `customware/*.md` and `*.patch`
  files inside 001's own patch. `':(exclude)customware'` is mandatory, and the
  self-reference is invisible unless you check for it — the composition test
  still passed with it present.
- `surrealdb/core/src/catalog/compat/v3_2_4_customware.rs` matches a naive
  `customware` filter by name but is a real source file. Exclude the *directory*,
  not the substring.

## Test outcomes

Every failure was classified against a **clean `v3.2.4` worktree baseline** built
and run on this same machine, rather than by inference.

| Suite | Clean v3.2.4 | This fork | Verdict |
|---|---|---|---|
| `cargo test -p surrealdb-core` | 3727 passed, **9 failed** | 3099 passed, **9 failed** | identical failure set |
| `cargo test -p surrealdb-core --features server` | not run | 3211 passed, 10 failed | +1 flaky concurrency test (passes 3/3 in isolation) |
| `cd language-tests && cargo run run` | 4497 passed, **6 failed** | 4522 passed, **6 failed** | identical failing files |

The 9 core failures and 6 language failures are **pre-existing upstream
failures on Windows**, unrelated to the fork:

- `mac::test::fail_*` — asserts `surrealdb/core/...` against `surrealdb\core\...`
- `obs::test_initialize_store_env_var`, `iam::file::test_allow_list_access`,
  `buc::store::file::tests::*` — host path / env-var assumptions
- `language/primitive/files/list.surql` — expects `File access denied:
  /some_directory`, but `/some_directory` is not absolute on Windows so the
  bucket URL parser rejects it earlier with a different error
- `language/statements/define/analyzer/basic.surql` — needs
  `SURREAL_FILE_ALLOWLIST` for the MAPPER file, which upstream made deny-by-default

Note the fork runs **fewer** core tests than baseline (3099 vs 3727) because
002's lean default gates out `auth` / `graphql` / `gql`. A green default run
therefore proves less than it did before 002 existed — hence the additional
`--features server` run, which must be kept in the gate list.

### Inherited failures this update fixed

Two fork-attributable failures were **already red at `v3.1.5-customware.3`** and
were surfaced, not caused, by this rebase. Both stem from `id_generation`
appearing in `TableDefinition::structure()` output while expectations went
un-updated — customware/001's patch carries **no** language-test changes at all:

- `cf::mutations::tests::serialization` / `serialization_rev2`
- `language/statements/info/subquery.surql` (× 3 execution modes)

### The recurring 002 failure mode

002 does not break when upstream *changes* gated code — it breaks when upstream
adds a **new consumer** of gated code. This update: upstream's 3.2 language-test
harness began calling `iam::signup` / `iam::signin`, which 002 gates behind
`auth`, so the harness stopped compiling. The previous update needed
`ec96508e2` for the identical reason in the server crate. No stored patch can
anticipate this; expect one such fix per update and budget for it.
