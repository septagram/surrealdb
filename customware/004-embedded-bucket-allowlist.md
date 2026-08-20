# customware/004 — Embedded file & bucket allowlist

Vendors upstream **[PR #7352](https://github.com/surrealdb/surrealdb/pull/7352)**
("fix(config): Expose local file and bucket allowlists on the embedded SDK
config") by `efortin`, which closes
**[issue #7353](https://github.com/surrealdb/surrealdb/issues/7353)**.

Status when vendored (2026-08-20): **open**, `MERGEABLE` but `REVIEW_REQUIRED`,
opened 2026-06-06, last touched 2026-07-07. **Zero maintainer reviews**; full CI
never triggered — only the `label-community-pr` check has ever run. The single
comment is from another community member and was addressed the same day.

## Why the fork needs it

`DEFINE BUCKET ... BACKEND "file://..."` **always fails with `File access
denied`** when SurrealDB is embedded via the `surrealdb` crate. There is no
configuration that makes it work — the seam does not exist.

The chain:

- `buc::store::file::FileStore::parse_url` calls
  `is_path_allowed(path, _, &config.bucket_list)`, which is
  `allowed.iter().any(..)` over an empty slice → `false` → deny. **An empty
  allowlist denies everything, by design**, since the original Files PR (#5701).
- `bucket_list` is parsed from the `bucket_folder_allowlist` key on
  `cnf::ConfigMap`.
- The server populates it: `surrealdb/server/src/dbs/mod.rs` does
  `ConfigMap::from_env()` then `.with_config(..)`.
- **The embedded engine never does.** `run_router` in
  `surrealdb/src/engine/local/native.rs` builds `Datastore::builder()` with
  timeouts, auth, capabilities and temp dir — but no `.with_config(..)`, so the
  builder keeps its `ConfigMap::empty()` default. `surrealdb/src/opt::Config`
  exposes no setter either.

So `SURREAL_BUCKET_FOLDER_ALLOWLIST` is read **only by the server binary**.
Exporting it in an embedded process does nothing.

### It is a regression, not a longstanding limitation

Before the config-globals refactor (`fc5c7bbe3`, upstream PR #21),
`cnf::BUCKET_FOLDER_ALLOWLIST` was a process-global `LazyLock` reading
`SURREAL_BUCKET_FOLDER_ALLOWLIST` straight from the environment — which *did*
work in embedded mode. #21 moved it to the per-datastore `ConfigMap` and the
embedded path was never wired up.

### What it blocks here

The `rust-dorsid` skill's canonical init sequence uses a `file://` bucket to
persist the `.surli` module artifact across restarts:

```surql
DEFINE BUCKET IF NOT EXISTS dorsid BACKEND "file://<bucket_path>";
```

That cannot work through a plain `Surreal::new::<SurrealKv>((db_path, config))`.

## What the patch does

Three files, +98/−2 upstream, plus a fork-local negative test.

```rust
// surrealdb/src/opt/config.rs
#[cfg(storage)]
pub(crate) datastore_config: ConfigMap,

#[cfg(storage)]
pub fn bucket_folder_allowlist<I, P>(mut self, paths: I) -> Self
where I: IntoIterator<Item = P>, P: AsRef<Path> { ... }

#[cfg(storage)]
pub fn file_allowlist<I, P>(mut self, paths: I) -> Self { ... }

// surrealdb/src/engine/local/native.rs — the missing wiring
let builder = Datastore::builder();
#[cfg(storage)]
let builder = builder.with_config(address.config.datastore_config.clone());
```

Helpers `with_path_allowlist` / `path_allowlist_value` /
`path_allowlist_delimiter` (`;` on Windows, `:` elsewhere, matching server-side
parsing) fill a `ConfigMap` that `run_router` forwards to the datastore builder.

Entirely opt-in: call neither method and the config stays empty and behaviour is
byte-identical to upstream. No new dependencies, environment variables, or flags.

## Known deviations from the upstream PR

**Native only — `wasm.rs` is deliberately untouched.** The PR patches
`surrealdb/src/engine/local/native.rs`; `surrealdb/src/engine/local/wasm.rs` has
its own `Datastore::builder()` chain that is left alone.

This is a conscious choice, not an oversight. `FileStore` is
`#[cfg(not(target_arch = "wasm32"))]`, so `file://` buckets cannot work on WASM
regardless, and the gap is harmless for buckets (`file_allowlist` also feeds
analyzer mappers, so it is not *entirely* moot). Keeping this entry
byte-comparable to the upstream PR is the whole point of a drop-when-merged
entry — divergence here would mean the drop trigger below could not be a clean
deletion.

**Added a negative test** (`local_config_without_allowlist_still_denies_file_buckets`).
The upstream PR proves the allowlist *grants* access; nothing in it proves the
default still *denies* it. Since this entry touches file-access control, that
asymmetry matters — a later refactor could make an empty allowlist permissive and
every existing test would still pass. `SECURITY_GUIDE.md` section 12: *"Backend
URLs in DEFINE BUCKET must be validated against an allowlist."*

## Verification

```bash
cargo test -p surrealdb --test api --features kv-mem local_config
```

- `local_config_applies_file_bucket_allowlist` (upstream's) — set an allowlist,
  define a `file://` bucket, `put` bytes via `type::file(...)`, read them back,
  assert the round-trip.
- `local_config_without_allowlist_still_denies_file_buckets` (fork-local) — the
  same sequence minus the allowlist call must fail with `File access denied`.

## Drop trigger

> **Delete this entry when upstream merges
> [PR #7352](https://github.com/surrealdb/surrealdb/pull/7352).**
>
> Check during every `/customware update`:
>
> ```bash
> gh pr view 7352 --repo surrealdb/surrealdb --json state,mergedAt
> ```
>
> If `state == "MERGED"` **and** the target release actually contains it
> (`git log v<target> --oneline | grep -iE "bucket.*allowlist|7352"`), delete
> `004-embedded-bucket-allowlist.md` and `.patch`, record the removal in that
> update's `CHANGES-*.md`, and retire the number `004` — never reuse it.
>
> If upstream merges a *different* fix for issue #7353 (say, a redesigned config
> surface), re-implement this entry against the new API rather than deleting it
> blindly — call sites depend on `Config::bucket_folder_allowlist`.

## Re-implementation trigger (3.3 and later)

> **This patch will not survive the move to the 3.3 line as-is.**
>
> It anchors on `run_router` in `surrealdb/src/engine/local/native.rs` building
> its datastore via `Datastore::builder()`. On `v3.3.0-beta.2` that code is gone
> — the local engine was split into a separate `surrealdb-engine-local` crate and
> `native.rs` now just does `use surrealdb_engine_local::Datastore`. `git apply`
> will fail, and a fuzzy apply would be worse than a failure.
>
> When a future `/customware update` targets **3.3 or later**, treat this entry
> as a **re-implementation**, not a patch application:
>
> 1. Locate where the local engine now constructs its `Datastore` inside
>    `surrealdb-engine-local`.
> 2. Re-thread `opt::Config::datastore_config` (or its successor) through to
>    `.with_config(...)` at that new site.
> 3. Keep the public API identical — `Config::bucket_folder_allowlist(paths)` and
>    `Config::file_allowlist(paths)` — so downstream call sites do not move.
> 4. Regenerate `004-embedded-bucket-allowlist.patch` against the new anchor and
>    record what changed under "Implementation notes".
>
> Check the drop trigger **first** on any such update: if PR #7352 landed
> upstream in the meantime, delete the entry instead of re-implementing it.
> Re-implementation is only for the case where the gap still exists on the new
> line.
>
> Related: `customware/002-prune-deps-for-embedded` rewrites
> `surrealdb/src/engine/local/mod.rs` and needs its own design pass for the same
> crate split. Sequence 002 before 004 on that update — 004's anchor may depend
> on where 002 lands.

## Implementation notes (what actually shipped)

- **Patch anchor.** The patch is `git diff <Phase A tip>..HEAD -- ':(exclude)customware'`,
  not the skill's literal `git diff upstream/main..HEAD`. Taken literally that
  command produces a *cumulative* diff carrying 001–003 as well; the fork's
  convention (visible in the existing entries) is one patch per entry. The
  anchor here is `1467b1ef8`, the `-s ours` merge that closed the v3.2.4 update.
  It is also a tag-line anchor rather than `upstream/main`, which sits on a
  different and older release line.
- **Applied cleanly.** `git apply --check` passed against v3.2.4 + customware
  001–003; no conflict resolution was needed. Verified PR #7352 was still `OPEN`
  immediately before vendoring, so the drop trigger did not fire.
- **Test target.** The tests live in the `api` test target (`surrealdb/tests/api.rs`
  declares `mod api_integration`), so the invocation is
  `cargo test -p surrealdb --test api --features kv-mem local_config`.
  `--test api_integration` does not exist and fails with "no test target named".
  `--features kv-mem` is required because customware/002 made the SDK's default
  feature set lean.
- **No WASM mirror.** As stated under "Known deviations" — deliberate, to keep
  this byte-comparable with the upstream PR.
- Both tests pass: the upstream round-trip and the fork-local negative case.
