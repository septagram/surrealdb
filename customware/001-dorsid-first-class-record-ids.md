# Dorsid Sid/Rid as first-class SurrealDB record IDs

## Context

Today, when SurrealDB creates a record without an explicit `id`, it generates a 20-character random alphanumeric string (`Document::generate_record_id` → `RecordIdKey::rand`). For the user's projects, neither random strings nor ULIDs are the desired key format — the project uses **Dorsid** IDs (from the sibling crate at `../diceware-ordinal/dorsid`):

- **Sid** — i64-packed timestamp + realm + per-ms sequence (collision-free monotonic ID, ~1024 mints per realm per millisecond).
- **Rid** — i64 with 63-bit CSPRNG payload (stateless, multi-source safe).

Until now, this was bolted on via a Surrealism WASM plugin (`dorsid-surreal-plugin`) that used KV-backed atomic counters. That plugin is being retired. The goal of this change is to make Sid/Rid ID generation native to this SurrealDB fork: choose the ID kind in `DEFINE TABLE`, and SurrealDB takes care of the rest. Out of scope for now: rendering i64 IDs as diceware strings on query output, parsing diceware strings on input, and any `Record`-type smart loading. Those land later; only ID generation matters now.

This is the "yolo fork" pipeline ([[project-purpose]]) — direct-to-main work, no PR rigor.

## Decisions

### D1. Nightly Rust for the fork

`dorsid/src/lib.rs:1-9` declares `#![feature(...)]` and pins nightly. The fork accepts nightly — pin via `rust-toolchain.toml` at repo root, matching `diceware-ordinal`. ("We are free to dance in the shadows till the dawn.")

### D2. Syntax

```
DEFINE TABLE foo SCHEMAFULL ID default       -- explicit default (== omitted)
DEFINE TABLE foo SCHEMAFULL ID sid
DEFINE TABLE foo SCHEMAFULL ID rid
```

Three kinds, all explicitly parseable; omitted clause = `default` semantics (20-char random string, current behavior). `ID default` and the omitted form must produce identical `TableDefinition` state and the same DDL on round-trip (`INFO FOR DB` re-emits the explicit form when the user wrote it, or normalizes to omitted — pick whichever is easier; both are acceptable as long as the chosen normalization is consistent). Matches the one-keyword-with-value pattern of existing options (`TYPE NORMAL`, `PERMISSIONS NONE`). The token `ID` is not currently a top-level `DEFINE TABLE` option, so no parser conflict.

### D3. Sid state model — single-writer first, multi-writer via realm split

One `dorsid::sid::Generator` per `(NamespaceId, DatabaseId, TableId)` held on `Datastore` (process-local). `realm_id` from `DORSID_REALM_ID` env var at process start (default 0). Multi-writer in the same DB is supported by giving each node a distinct `realm_id` — no DB-level coordination needed; collision-free by Sid layout. **No KV-backed atomic counter, no per-record KV CAS** (explicitly ruled out — pure SurrealQL only).

### D4. Warmup (high-water mark)

On first Sid mint for a table per process boot, async-scan the table's max stored Sid and call `Generator::set_floor` to seed the seq counter above it. Survives clock regression (NTP slews) without collision. Implemented as a `OnceCell` per (ns, db, table) in a `Datastore`-level map (sibling to `cache`), driven by `OnceCell::get_or_init_async` in the upstream-of-`generate_record_id` path (`dbs/processor.rs` `NsDbTbCtx` construction sites). `generate_record_id` itself stays sync.

### D5. Rid is stateless

Use the singleton: `dorsid::rid::next_persistent(None)?.to_bits()` → `RecordIdKey::Number(i64)`. Requires `rid` + `rid-persistent` features. No registry, no warmup, no allocation per call (the underlying `Generator` is zero-sized; the singleton avoids even constructing it). `word_count` is left at the dorsid default (5) — making it per-table configurable is a deferred enhancement.

### D6. Sid overflow behavior — let it block

`dorsid::sid::Generator::next` calls `warn_overflow_rate_limited()` (so it logs, not silent) and then `sleep_until_next_ms`, which internally uses `std::thread::sleep`. In a tokio worker this briefly blocks that thread (max 1ms per overflow burst). Acceptable per user preference: **backpressure over errors**. No special handling — call `next()`, let it block, let dorsid's rate-limited warning surface in logs. If sustained 1024+ inserts/ms/realm/table load ever materializes as a real problem, revisit with `tokio::task::block_in_place` or a runtime-aware sleep — out of scope for v1.

### D7. Collision handling

SurrealDB does **not** auto-retry on id collision. `Document::insert` (`core/src/doc/insert.rs:83`) and `upsert` (`core/src/doc/upsert.rs:49`) catch `Error::RecordExists` to drive ON DUPLICATE / UPDATE flows; plain CREATE propagates the error.

- **Sid**: cannot collide within a realm by construction (monotonic seq + warmup-enforced floor). Multi-realm: cannot collide because realm bits differ. No retry needed.
- **Rid**: collision is mathematically possible but rare (~52-bit payload with `word_count=5`; birthday-paradox 50% at ~67M draws per table). On collision the user sees a `RecordExists` error from CREATE. Matches the behavior of the existing 20-char default (which also doesn't retry, though its entropy is much larger). Accepted as a known limitation for v1; future enhancement could add retry or expose `word_count` per table.

## Implementation plan

Each numbered step is one logical commit. Land in order.

### Step 1 — Toolchain

- Add `rust-toolchain.toml` at repo root pinning nightly (match `diceware-ordinal`'s pin).
- Smoke build `cargo build -p surrealdb-core` on nightly to confirm no surprises before adding the dep.

### Step 2 — `IdGeneration` enum + `TableDefinition` revision bump

- New type `IdGeneration { Default, Sid, Rid }` in `core/src/catalog/table.rs` (next to `TableType`). Implement `InfoStructure` returning a string (`"default"`, `"sid"`, `"rid"`).
- Add `pub(crate) id_generation: IdGeneration` to `TableDefinition` (`core/src/catalog/table.rs:43-66`). Bump `#[revisioned(revision = 1)]` → `revision = 2`. Mark new field `#[revision(start = 2)]`. Old records loading at revision 2 default to `IdGeneration::Default` — verify the revision crate's default-on-missing behavior; if it doesn't default automatically, supply via the `convert_fn` mechanism modeled on `Relation::rev_convert_from` (same file, ~line 231).
- Update `TableDefinition::to_sql_definition` (`catalog/table.rs:106-123`) to emit `ID sid`/`ID rid` when set.
- Update `InfoStructure::structure` (`catalog/table.rs:132-146`) to include `"id_generation"` key.
- *No parser changes yet.* `INFO FOR DB` should still work; the field just always reads `Default`.

### Step 3 — Parser + AST + `compute`

- Add `pub id_generation: IdGeneration` to `DefineTableStatement` (`core/src/expr/statements/define/table.rs:35-62`).
- Extend `parse_define_table` (`core/src/syn/parser/stmt/define.rs:624-716`) with an `ID` arm in the option loop. Required to accept all three: `ID default`, `ID sid`, `ID rid` (case-insensitive, consistent with existing keyword matching). Omitted clause = `IdGeneration::Default`.
- Plumb the parsed value into `TableDefinition` in `DefineTableStatement::compute` (`expr/statements/define/table.rs:65-146`).
- Test: DDL round-trip — define, `INFO FOR DB`, reparse, structural equality, for all three explicit forms plus the omitted-clause form.

### Step 4 — Rid generation path

- Add `dorsid` as a `path = "../../diceware-ordinal/dorsid"` dep in `surrealdb/core/Cargo.toml`. Required features: `sid`, `sid-persistent`, `rid`, `rid-persistent`, `csprng-getrandom` (some are defaults; verify in `dorsid/Cargo.toml`).
- In `Document::generate_record_id` (`core/src/doc/alter.rs:28-64`), only the no-explicit-id branches (lines 42 and 48 — both currently call `RecordId::random_for_table(tb.clone())`) need to dispatch. Inspect `self.doc_ctx.tb()?.id_generation`:
  - `Default` → existing `RecordId::random_for_table(tb.clone())`.
  - `Rid` → `dorsid::rid::next_persistent(None)?.to_bits()` → wrap as `RecordIdKey::Number(i64)` inside a `RecordId { table: tb.clone(), key: ... }`.
  - `Sid` → `todo!()` for now (next step lights this up).
- The explicit-id branches (lines 34-36 and 44) are untouched — `id.generate(tb.clone(), false)` keeps respecting user-supplied ids regardless of `id_generation`.
- Tests: `CREATE foo` on an `ID rid` table produces a `Number(i64)` id with sign bit 0; `CREATE foo:abc` on the same table still produces `abc` as the id.

### Step 5 — Sid generator registry (no warmup yet)

- New module `core/src/kvs/dorsid.rs` exposing a struct (e.g. `SidRegistry`) wrapping `DashMap<(NamespaceId, DatabaseId, TableId), Arc<Mutex<dorsid::sid::Generator>>>` (or `RwLock<HashMap>` if avoiding the `dashmap` dep — check what's already in tree).
- Add `sid_registry: Arc<SidRegistry>` to `Datastore` (`core/src/kvs/ds.rs:123` area, sibling to `cache`).
- Read `realm_id` from `DORSID_REALM_ID` env var once at `Datastore::new`; default 0. Stash it on `SidRegistry` for use when minting a new generator.
- In `generate_record_id`'s Sid branch (replacing the `todo!()` from step 4), look up the table's generator via `(ns_id, db_id, table_id)` from the `TableDefinition`, call `.next()`, convert to `RecordIdKey::Number`. Per D6, the brief `std::thread::sleep` on overflow is intentional — let it happen.
- Tests: monotonic mint, sign bit 0, two inserts in the same ms get distinct seqs, explicit-id still wins.

### Step 6 — Sid warmup

- Add `Arc<DashMap<(NsId, DbId, TableId), Arc<OnceCell<()>>>>` to `SidRegistry` (the `OnceCell` is just a "warmup completed" marker; the generator itself lives in the other map).
- At the `NsDbTbCtx` construction sites in `core/src/dbs/processor.rs:199-216` (and the sibling sites flagged by the Plan agent in `iterator.rs`), after `txn.get_or_add_tb` resolves the table def, if `tb.id_generation == Sid`: `cell.get_or_init_async(|| async { warmup(ns, db, tb).await }).await`.
- `warmup` does a reverse range scan over the table's record keys, limit 1, decodes the largest `RecordIdKey::Number(i64)`, wraps as `dorsid::Sid::from_bits`, calls `Generator::set_floor`. Skip silently if the table is empty.
- Tests: insert N Sids, drop the registry (simulating restart), insert more, assert no value-equality between pre- and post-restart batches even when artificially backdating wall clock.

### Step 7 — Tidy

- Documentation comment on `IdGeneration` describing semantics.
- One integration test exercising all three modes side-by-side in one DB.
- Verify `INFO FOR TABLE foo` round-trips through `DefineTableStatement::to_string` cleanly for all three.

## Critical files

| File | Purpose |
|---|---|
| `surrealdb/core/src/catalog/table.rs:43-146` | `TableDefinition`, revision bump, `to_sql_definition`, `InfoStructure` |
| `surrealdb/core/src/expr/statements/define/table.rs:35-146` | AST + `compute` plumbing |
| `surrealdb/core/src/syn/parser/stmt/define.rs:624-716` | `parse_define_table` |
| `surrealdb/core/src/doc/alter.rs:28-64` | `generate_record_id` dispatch |
| `surrealdb/core/src/kvs/ds.rs` (~line 123) | `Datastore` field for `SidRegistry` |
| `surrealdb/core/src/kvs/dorsid.rs` *(new)* | `SidRegistry` impl |
| `surrealdb/core/src/dbs/processor.rs:199-216` | warmup hook (and sibling sites in `dbs/iterator.rs`) |
| `surrealdb/core/Cargo.toml` | `dorsid` path dep |
| `rust-toolchain.toml` *(new)* | nightly pin |
| `surrealdb/core/tests/define.rs` | integration tests, mirror existing patterns |

## Verification

End-to-end manual check (after step 6):

```sql
DEFINE TABLE foo_default SCHEMAFULL;
DEFINE TABLE foo_sid     SCHEMAFULL ID sid;
DEFINE TABLE foo_rid     SCHEMAFULL ID rid;

CREATE foo_default;             -- id is RecordIdKey::String("...")
CREATE foo_sid;                 -- id is RecordIdKey::Number(positive_i64), Sid-shaped
CREATE foo_sid;                 -- monotonically > previous
CREATE foo_rid;                 -- id is RecordIdKey::Number(positive_i64), random
CREATE foo_sid SET id = 42;     -- id is 42, schema preference ignored
INFO FOR DB;                    -- shows "id sid" / "id rid" in DDL round-trip
```

Restart test: stop the process, restart, `CREATE foo_sid;` again, assert new id > prior max (warmup populated `set_floor`).

Automated: `cargo test -p surrealdb-core --test define` covers DDL & info; new tests in the same file cover generation and warmup.

## Known risks / followups (NOT in this plan)

- Diceware-string rendering on query output (deferred per user).
- Parsing diceware strings as record ids on `THING` literals (deferred).
- `Record<>` type loading IDs as `Sid`/`Rid` based on schema (deferred).
- Configurable Rid `word_count` per table (deferred — hard-default to `None`/5; smaller payload than the 20-char default means non-trivial collision probability at high cardinality, see D7).
- Multi-writer Sid via in-DB realm coordination (out of scope; multi-writer = distinct `DORSID_REALM_ID` per node, manual).
- Tokio-aware Sid overflow handling (the `std::thread::sleep` is fine for v1 per D6; revisit only if sustained ≥1024 inserts/ms/realm/table workloads appear).
- Rid collision retry — none today; relies on payload entropy + user accepting the `RecordExists` error path (D7).

---

## Implementation notes (what actually shipped)

Deviations from the plan as written, captured after the work landed (commits `427e9367..95e4ea8e` on this branch):

- **Step 4 warmup location revised in Step 6.** The plan put warm-up at the `NsDbTbCtx` construction sites in `dbs/processor.rs` and `dbs/iterator.rs` (D4). We instead did **lazy warm-up at mint time inside `generate_default_id`** — `SidRegistry::next_sid_warmed(&txn, &tb_def)` runs a `tokio::sync::OnceCell::get_or_try_init`-guarded reverse range scan on the first Sid for each table, then mints. One-place hook instead of 5; same once-per-process-per-table guarantee.
- **`Document::generate_record_id` became `async`.** The Plan agent had said this wasn't necessary, but only on the assumption that warm-up lived elsewhere. With lazy warm-up at mint time, the KV scan needs await, so the function is now async; the six callers (`create`, `upsert` ×2, `insert` ×2, `relate`) `.await` it.
- **New `Transaction::scan_raw_keys_reverse(rng: Range<Vec<u8>>, limit)`** wraps `tr.scanr` for byte-range bounds. `KVKey`-typed bounds are not a fit for `crate::key::record::prefix..suffix`.
- **`Keyword` enum bumped from `#[repr(u8)]` to `#[repr(u16)]`.** Adding three new keywords (`ID`, `SID`, `RID`) pushed the enum past 256 variants. The `_TOKEN_KIND_SIZE_ASSERT` static check on `TokenKind` was updated from 2 → 4 bytes.
- **`pprof` dev-dep gated to `cfg(unix)`** in `surrealdb/core/Cargo.toml`. Unconditional dependency broke Windows test builds (`pprof` references `libc::pthread_t`/`siginfo_t`/`ucontext_t` which don't exist on MSVC). Picked up as part of Step 1 since it was a hard blocker for `cargo test` on the dev machine.
- **`profile.dev` trimmed to `debug = "line-tables-only"`.** Full `debug = 2` plus the dorsid + test-binary monomorphization budget exceeded rustc-LLVM's memory on Windows nightly; line tables keep backtraces while shrinking codegen pressure enough to fit.
- **Tests live inline in `core/src/doc/alter.rs`** rather than in `core/tests/define.rs`. The original plan referenced `core/tests/define.rs`, but the integration-test target on this Windows + nightly toolchain hits a separate rustc-LLVM `STATUS_STACK_BUFFER_OVERRUN` (pre-existing). Inline `#[cfg(test)]` tokio tests in the lib crate aren't affected and gave us the cross-mode coverage requested in Step 7 anyway. Re-evaluate when the rustc bug clears.
- **`SidRegistry` lives at `core/src/kvs/dorsid.rs`** with `DashMap` (workspace dep, already in tree) + `parking_lot::Mutex` for the per-table generator and `tokio::sync::OnceCell` for the warm-up marker.
- **Plumbing through `Context`.** The registry is reached at mint time via `ctx.get_sid_registry()`, mirroring how `Sequences` is exposed. `Context::from_ds` gained a `sid_registry: Arc<SidRegistry>` parameter; the six Context constructors propagate the field.
- **DDL emission normalization.** `IdGeneration::Default` emits no `ID` clause; `Sid` emits `" ID SID"`; `Rid` emits `" ID RID"`. `DEFINE TABLE foo ID DEFAULT` parses fine and produces the same `TableDefinition` as the omitted form (chose to normalize emission to "omitted" rather than preserve the explicit form). `INFO FOR DB STRUCTURE` reports `id_generation: "DEFAULT" | "SID" | "RID"` on every table.
- **8 pre-existing upstream lib-test failures** (`mac::test::fail_*`, `dbs::executor::tests::check_execute_timeout`, `iam::file::tests::test_allow_list_access`, `obs::tests::test_initialize_store_env_var`) are untouched — they were already failing on Windows + nightly before this work and aren't in scope for the Dorsid integration.

## Final commit list (`upstream/main..HEAD`)

```
427e9367 Pin fork to nightly Rust and gate pprof to Unix
6ee8e2f6 Add IdGeneration table option to TableDefinition (no parser yet)
d85c5305 Parse DEFINE TABLE ... ID [default|sid|rid]
9e493488 Wire up Rid generation in Document::generate_record_id
6bae31b5 Add in-process Sid generator registry (no warmup)
2331a664 Warm the Sid generator floor from stored record keys
95e4ea8e Cross-mode integration tests and tidy
```

## v3.1.5 reapplication notes

- Reimplemented against upstream `v3.1.5` instead of applying the old patch
  byte-for-byte.
- `TableDefinition` was already at revision 2 in `v3.1.5` for GraphQL
  alias/deprecation fields, so `id_generation` now starts at catalog revision 3.
- The lazy Sid warm-up path now adapts to the `ScanResult` return type from
  `Transaction::scanr` and records scan metrics before returning raw keys.
- The document pipeline keeps the `v3.1.5` permission/retry ordering; only the
  default record-id minting hook became async for Dorsid Sid warm-up.
- Updated the `TableDefinition` catalog serialization fixture to expect 153
  bytes with the revision-3 `id_generation` field.
- Verified the reimplementation with
  `cargo check -p surrealdb-core --no-default-features --features kv-mem`.
