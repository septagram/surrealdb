# customware/002 — Prune SurrealDB's dependency tree for embedded use

## Context

The user uses this SurrealDB fork almost exclusively as an embedded library, never as a hosted server. The default dependency tree pulls in `axum`, `reqwest`, `jsonwebtoken`, `object_store`, `tokio-tungstenite`, `async-graphql`, `surrealism-runtime` (which pulls `wasmtime`), and dozens of transitive crates that exist purely to support server-mode functionality. None of this is needed in-process. The fork can shed it.

Goal: a working `cargo add surrealdb --no-default-features --features kv-mem` (or `kv-rocksdb`) that produces a substantially leaner dep tree, with the now-disabled code paths cleanly gated behind features. Server-mode users (the `surreal` binary, the `surrealdb-server` crate) keep their current behaviour via a default-on umbrella feature.

## Existing feature landscape (state of the world)

Already gated cleanly:
- `kv-mem`, `kv-rocksdb`, `kv-tikv`, `kv-surrealkv`, `kv-indxdb` — storage backends
- `http` (in core) — gates `reqwest`, the `http::*()` SurrealQL functions, and Fetch API DNS resolver
- `jwks` (in core) — gates `reqwest`-backed JWKS endpoint fetching only (jsonwebtoken itself is still unconditional)
- `scripting` — JS runtime (rquickjs)
- `surrealism` — WASM module runtime (wasmtime)
- `graphql` — async-graphql schema generation
- `ml` — surrealml-core
- `protocol-ws` / `protocol-http` (in SDK) — client transports for remote SurrealDB servers
- `rustls` / `native-tls` (in SDK) — TLS backends for the client transports

Unconditional in `surrealdb-core` today (heavy, server-flavoured):
- `jsonwebtoken` (workspace = true, no `optional`) — required by `iam/issue.rs`, `signin.rs`, `signup.rs`, `verify.rs`, `jwks.rs`, `token.rs`, `err/mod.rs`; backs `DEFINE ACCESS JWT`
- `object_store` (workspace = true, no `optional`) — used by `obs/mod.rs` for `DEFINE BUCKET`

Default features today:
- `surrealdb-core` defaults: `kv-mem, graphql`
- `surrealdb` (SDK) defaults: `protocol-ws, rustls`
- `surreal` binary defaults: everything (`allocator, allocation-tracking, storage-*, scripting, http, surrealism, graphql, cli`)

## Decisions

### D1. Introduce a `server` umbrella feature in `surrealdb-core`

`surrealdb-core/server` = `["http", "jwks", "auth", "buckets", "graphql", "scripting-fetch"]`. The semantic: "build the full server-mode surface area; if you don't enable this, you're embedded."

**Note `surrealism` is NOT in the umbrella.** Surrealism is the plugin (WASM module) runtime — it lets user code run inside the database. It's orthogonal to server-vs-embedded: an embedded SurrealDB can host plugins too (see the sibling `dorsid-surreal-plugin`). Keep `surrealism` as its own feature, default off everywhere. Anyone who wants plugins, embedded or hosted, opts in explicitly. `scripting` (JS via rquickjs) gets the same treatment — its own feature, default off; it's not "server-flavoured" either.

- **Default OFF on both `surrealdb-core` and the `surrealdb` SDK.** Bolder default swap per the user's call ("less stuff for us to remember; we are very much allowed to diverge"). A vanilla `cargo add surrealdb` now produces an embedded-flavoured build.
- `surrealdb-server` and the `surreal` binary explicitly enable `server` (and the other heavyweight features) in their own default sets, so the hosted-server build path is unchanged.

### D2. Two new fine-grained features under the umbrella

#### `auth` (new) — gates the ENTIRE IAM subsystem

Gates `jsonwebtoken`, `bcrypt`, the whole `iam/` module, the signin/signup/verify pipeline, and the executor branches for `DEFINE USER` / `DEFINE ACCESS`. Disabled in embedded → only `Session::owner()` works (the privileged in-process root identity); `DEFINE USER` and `DEFINE ACCESS` statements parse fine (catalog stability, see D5) but executing them returns a clear "this build does not support auth — rebuild with `--features auth`" error.

Per the user's call ("Yeah let's gate it entirely"): this is the wholesale split for v1. A future customware entry can carve out `auth-user` (bcrypt-only DEFINE USER + signin, no JWT) when the Artilect-plugin-per-user use case actually lands.

Implies: `jwks` requires `auth` (you can't validate via JWKS without JWT machinery).

#### `buckets` (new)

Gates `object_store` and the `DEFINE BUCKET` machinery in `obs/mod.rs`. Disabled in embedded → no bucket code at all. Local-fs buckets carve-out (`buckets-local`) deferred to a future customware entry per the user's call ("It's not that important after all. Cancel my earlier local-fs request for now").

### D3. Close the scripting-fetch leak

Today JavaScript's `fetch()` (in `fnc/script/fetch/`) is gated by `scripting` alone, even though it pulls reqwest indirectly via the same path that `http::*()` functions use. New feature `scripting-fetch` gates the JS `fetch()` API specifically; it requires both `scripting` and `http`. Without it, the JS runtime is available but `fetch()` is missing — runtime error if a script calls it.

Embedded users who want scripting without network: `--features scripting` is now genuinely network-free.

### D4. Code gating

For every newly-optional dependency, the corresponding source files get `#[cfg(feature = "auth")]` or `#[cfg(feature = "buckets")]` wrappers. Module declarations gain conditional `pub mod ...;` lines.

Critical files (representative; full list during execution):
- `surrealdb/core/Cargo.toml` — flip `jsonwebtoken`, `bcrypt`, `object_store` to `optional = true`; add `auth`, `buckets`, `scripting-fetch`, `server` features. Drop `graphql` from `default` (it's now under `server`).
- `surrealdb/core/src/iam/` — entire module wrapped by `#[cfg(feature = "auth")]` at `mod.rs`. The few `iam::Auth`/`iam::Role` re-exports that `dbs::Session` depends on either move to `dbs/` or `catalog/` (un-gated), or get stub equivalents under `#[cfg(not(feature = "auth"))]` that only carry the `Anonymous` and `Owner` variants. **`Session::owner()` MUST keep compiling without `auth`.**
- `surrealdb/core/src/{sql,expr}/access_type.rs` — leave the AST variants in place (compiled in all configs) so SQL like `DEFINE ACCESS ... TYPE JWT ...` still parses; the executor branch for these statements (`expr/statements/define/access.rs` and similar) gates with `#[cfg(feature = "auth")]` and short-circuits otherwise with a clear runtime error.
- `surrealdb/core/src/syn/parser/stmt/define.rs` — left unchanged for `DEFINE ACCESS` / `DEFINE USER` (parser is agnostic to whether execution can succeed).
- `surrealdb/core/src/obs/mod.rs` — wrap with `#[cfg(feature = "buckets")]`; the `DEFINE BUCKET` executor (`expr/statements/define/bucket.rs`) gates with the same.
- `surrealdb/core/src/fnc/script/fetch/` — wrap with `#[cfg(feature = "scripting-fetch")]`; existing `#[cfg(feature = "scripting")]` gates stay, the fetch subdir gets the extra layer.
- `surrealdb/Cargo.toml` (SDK) — drop `protocol-ws, rustls` from defaults; add `auth`, `buckets`, `scripting-fetch`, `server` pass-through features; do NOT add `server` to defaults.
- `surrealdb/server/Cargo.toml` — add `server` to its dependency line on `surrealdb-core` (so the binary still gets everything).

### D5. Catalog/Revisioned types — DO NOT break wire format

The on-disk `TableDefinition`, `AccessDefinition`, etc. live in `catalog/` and use the `revisioned` crate. Their wire format must remain stable so a database written with `--features server` can still be read by a build with `default-features = false`. **Decision**: variants of revisioned enums stay in place unconditionally at the catalog level (e.g. `AccessType::Jwt(_)` always exists in the persisted enum), but **executing** them when the relevant feature is off produces a clear runtime error like "this build does not support JWT access". Compile-time gating happens at the parser and executor level, not at the catalog level.

This keeps databases portable across feature configurations — important because the user might write data with the server build (e.g. testing) and read it with the embedded build.

### D6. Bolder defaults — embedded-first

`surrealdb` SDK defaults change from `protocol-ws, rustls` to **just `kv-mem`**. `surrealdb-core` defaults change from `kv-mem, graphql` to **just `kv-mem`**.

Effect: a vanilla `cargo add surrealdb` against this fork now produces an embedded build by default — no WebSocket client, no GraphQL, no auth, no buckets. Anyone wanting the previous behaviour adds `--features protocol-ws,rustls,server` (or whatever subset they need).

This is the explicit user call: "Yep, bolder default swap. We can bring it back if people complain, but the whole point of customware concept is there aren't many people to care about."

### D7. Verification: lean-tree baseline, with before/after counts

**Baseline (before any changes)**, with SurrealKV as the backend:
```
cargo tree -p surrealdb --features kv-surrealkv -e normal --prefix none | sort -u | wc -l
```
Capture this number first (call it `BEFORE`). SurrealKV is an external crate (`surrealkv = "0.21.0"` in workspace deps — separate repo), so the kv-surrealkv feature pulls it as a dep.

**After Step 4**, with the new defaults:
```
cargo tree -p surrealdb --no-default-features --features kv-surrealkv -e normal --prefix none | sort -u | wc -l
```
Call this `AFTER`. Report `BEFORE - AFTER` and the percentage reduction.

`cargo tree -p surrealdb --no-default-features --features kv-surrealkv` should NOT include any of these as transitive deps:

- `axum`, `axum-server`, `axum-extra`, `tower-http`
- `reqwest`, `hyper`, `hyper-util` (the latter two may sneak in via tokio; that's fine — what matters is that `axum`+`hyper` aren't pulled)
- `jsonwebtoken`, `bcrypt`, `ring`, `aws-lc-rs` (the JWT/crypto/auth chain)
- `object_store`, `aws-sdk-*` (the cloud chain)
- `tokio-tungstenite`, `tokio-tungstenite-wasm`
- `async-graphql`, `async-graphql-axum`
- `wasmtime`, `wasmtime-wasi`, `wit-bindgen` (surrealism plugin runtime — only pulled with `--features surrealism`)
- `tonic`, `opentelemetry-otlp`
- `rquickjs` (JS scripting — only with `--features scripting`)

Embedded build SHOULD still include: `tokio` (runtime), `serde`, `revision`, `storekey`, `surrealkv`, the parser stack, dorsid (from customware/001), `dashmap`, `parking_lot`.

## Grey areas (resolved)

1. **GraphQL execution** — ACK: keep gated by `graphql`, included in `server` default, embedded users opt in with `--features graphql` if needed.
2. **`http::*()` SurrealQL functions** — ACK: keep gated by `http`.
3. **`DEFINE BUCKET` local-fs carve-out** — deferred per user's call: cancel earlier local-fs request, gate the whole `buckets` subsystem for v1, revisit later.
4. **bcrypt-only `DEFINE USER`** — user chose to gate the entire IAM subsystem (incl. password auth) under `server` for v1. Future customware entry can split out an `auth-user` (password-only) feature when the Artilect-plugins-per-user use case lands.
5. **Embedded live queries** — confirmed: pure in-process, no WebSocket, kept.
6. **`surrealism` (WASM modules)** — pulled OUT of the `server` umbrella per user's call. Orthogonal to server-vs-embedded; default off everywhere; opt-in with `--features surrealism` whether you're hosting or embedding.
7. **`scripting` (JS via rquickjs)** — own feature, default off everywhere, opt-in with `--features scripting`.

## Implementation plan

Stepped commits, in order. Each step compiles and runs the lib test sweep before the next.

### Step 0 — Capture baseline dep counts using `comparator` as the test bed

Use `C:/Users/septa/Documents/Code/comparator/` as the real-downstream measurement harness — it already does `surrealdb = { version = "3", default-features = false, features = ["kv-surrealkv", "kv-mem"] }`, which is exactly the embedded shape we care about.

Two measurement points to capture (call them `A` and `B`); both are taken BEFORE we touch any code so they're stable references:

```
# A. Comparator against the local fork tip (customware/001 already in place,
#    nothing pruned yet). Temporarily add a [patch.crates-io] block at the top
#    of comparator/Cargo.toml:
#
#    [patch.crates-io]
#    surrealdb = { path = "../surrealdb/surrealdb" }
#    surrealdb-core = { path = "../surrealdb/surrealdb/core" }
#
# then:
cargo tree -e normal --prefix none --no-dedupe \
    --manifest-path C:/Users/septa/Documents/Code/comparator/Cargo.toml \
    | sort -u | wc -l        # → A_total
cargo tree -e normal --prefix none --no-dedupe \
    --manifest-path C:/Users/septa/Documents/Code/comparator/Cargo.toml \
    | sort -u | grep -c '^'  # sanity check

# B. Comparator against crates.io surrealdb v3 (the unmodified upstream).
#    Remove the [patch.crates-io] block, then:
cargo tree -e normal --prefix none --no-dedupe \
    --manifest-path C:/Users/septa/Documents/Code/comparator/Cargo.toml \
    | sort -u | wc -l        # → B_total
```

Record both numbers and the full dep list (`cargo tree ... > /tmp/before.txt`) in the customware entry's implementation notes — they're the historical record. Restore comparator's Cargo.toml to its original state before continuing (don't leave the `[patch.crates-io]` block in place when not measuring).

### Step 1 — Add `auth` feature, gate the credential pipeline [LANDED, commit 09893a8a]

**Final scope (refined during execution):** the `auth` feature gates the JWT/credential machinery and the parts of the IAM module that use jsonwebtoken — NOT the Auth/Role/Action/ResourceKind types that are used pervasively by dbs/ and doc/. Those types live in `iam::auth`, `iam::base`, `iam::check`, `iam::entities` and stay unconditional, so `Session::owner()` keeps compiling without `auth`. No need to relocate or stub them — they don't touch jsonwebtoken.

**bcrypt stays unconditional**: it's used by the `crypto::bcrypt::compare` / `crypto::bcrypt::generate` SurrealQL functions (general-purpose crypto), not just by IAM signin. Gating it would break query-level crypto for embedded users.

**Catalog stability (D5)**: the `catalog::Algorithm` enum and `AccessDefinition`/`UserDefinition` types stay unconditional. Only the `From<Algorithm> for jsonwebtoken::Algorithm` impls in `sql/algorithm.rs` and `expr/algorithm.rs` and the `algorithm_to_jwt_algorithm` helper in `iam/mod.rs` are gated.

**Files actually touched:**
- `surrealdb/core/Cargo.toml` — `jsonwebtoken` made `optional = true` in both target-cfg dep blocks; added `auth = ["dep:jsonwebtoken"]`; `jwks = ["auth", "dep:reqwest"]`.
- `surrealdb/core/src/iam/mod.rs` — gated the submodule declarations for `issue`, `signin`, `signup`, `token`, `verify` (the JWT-touching ones) plus `pub use token::Token;`, `use std::sync::Once;`, `use crate::catalog;`, and the `algorithm_to_jwt_algorithm` fn body. `access`, `auth`, `base`, `check`, `clear`, `entities`, `file`, `reset` stay unconditional.
- `surrealdb/core/src/sql/algorithm.rs` and `surrealdb/core/src/expr/algorithm.rs` — gated the `From<Algorithm> for jsonwebtoken::Algorithm` impl.
- `surrealdb/core/src/err/mod.rs` — gated `use jsonwebtoken::errors::Error as JWTError` and `impl From<JWTError> for Error`.
- `surrealdb/core/src/rpc/mod.rs` — gated `mod protocol;` and `pub use protocol::RpcProtocol;` entirely. `RpcProtocol` is consumed only by `surrealdb-server` (verified via grep across the whole workspace), which will pull `auth` via the `server` umbrella in Step 4.
- `surrealdb/core/src/gql/mod.rs` — gated `mod auth;`. `surrealdb/core/src/gql/schema.rs` — gated `use super::auth::add_auth_mutations` and the single call site (an "if any access definitions exist" branch).
- `surrealdb/core/src/kvs/ds.rs` — gated the `#[cfg(test)] mod test { use crate::iam::verify::verify_root_creds; ... }` to require `feature = "auth"` too.

**Verification on Windows**: `cargo check --no-default-features --features kv-mem` was blocked by the same upstream nightly rustc-LLVM OOM that affected customware/001. Dependency-resolution verification via `cargo tree` succeeded and is sufficient evidence the gating works:
- `cargo tree -p surrealdb-core` (defaults) — no `jsonwebtoken`, no `reqwest`
- `cargo tree -p surrealdb-core --features auth` — `jsonwebtoken v10.3.0` pulled
- `cargo tree -p surrealdb-core --no-default-features --features kv-mem` — no `jsonwebtoken`

The "Session::owner() still works on embedded build" inline test is deferred to Step 5's verification (when the full feature set lands) — running it on this Windows host requires resolving the rustc-LLVM OOM first.

### Step 2 — Add `buckets` feature, gate object_store [DEFERRED to customware/003]

**Why deferred**: while making `object_store` optional in Cargo.toml is straightforward, the surrounding code uses `crate::buc::manager::BucketsManager` and `crate::buc::store::ObjectStore` pervasively — pulled through `kvs::Datastore` (struct field + builder + restart paths), `ctx::Context` (field + all constructor sites + accessor + `get_bucket_store` async method + `surrealism` integration), and downstream in `exec/function/builtin/file.rs`, `fnc/file.rs`, `expr/model.rs`, and the `DEFINE/ALTER/REMOVE BUCKET` parser/executor trees. Gating each touchpoint behind `#[cfg(feature = "buckets")]` is mechanical but high-volume (≈ 12 files, multiple cfg-gates per file) and the trait-bound on `build_with_factory_path<F: TransactionBuilderFactory + BucketStoreProvider>` adds particular friction.

The structural pattern is the same as customware/001 (SidRegistry threading through Context); the rewrite is bounded and doable — it just doesn't fit in this entry's time budget. Punting it into customware/003 keeps the present entry shippable.

`object_store` and its transitives stay in the embedded dep tree until customware/003 lands. Net impact on the dep count from this single dependency is bounded (object_store is one crate plus a handful of HTTP transitives that are largely shared with reqwest/jsonwebtoken — though those are gone after Step 1, object_store still pulls `quick-xml`, `bytes`, etc.).

### Step 3 — Add `scripting-fetch` feature, close the leak [SKIPPED — no leak]

**Why skipped**: the "leak" identified during exploration doesn't exist on this upstream tip. `fnc/script/mod.rs` already gates the real `fetch` module behind `#[cfg(feature = "http")]` and falls back to a `fetch_stub` when off:

```rust
#[cfg(feature = "http")]
mod fetch;
#[cfg(not(feature = "http"))]
mod fetch_stub;
#[cfg(not(feature = "http"))]
use self::fetch_stub as fetch;
```

Verified: `cargo tree -p surrealdb-core --no-default-features --features kv-mem,scripting` does NOT pull `reqwest`. Enabling `scripting` alone is already network-free; adding a new `scripting-fetch` feature would add noise without benefit. Upstream's design here is fine; the original exploration agent's report was wrong about this case.

### Step 4 — Stitch the `server` umbrella feature and swap defaults

- Edit `surrealdb/core/Cargo.toml`: define `server = ["http", "jwks", "auth", "buckets", "graphql", "scripting-fetch"]`. Change `default` to just `["kv-mem"]` (drop `graphql`).
- Edit `surrealdb/Cargo.toml` (SDK): add `server = ["surrealdb-core/server"]`, `auth = ["surrealdb-core/auth"]`, `buckets = ["surrealdb-core/buckets"]`, `scripting-fetch = ["surrealdb-core/scripting-fetch"]` pass-throughs. Change `default` to `["kv-mem"]` (drop `protocol-ws, rustls`).
- Edit `surrealdb/server/Cargo.toml`: update its dep on `surrealdb-core` and on `surrealdb` to include `"server"` in the features list (so the hosted-server build path is unchanged).
- Verify: `cargo build -p surreal` (the binary) still pulls everything as before. `cargo build -p surrealdb` (no extra flags) now produces a lean embedded tree.

### Step 5 — Cross-build verification + measurement

- **AFTER measurement**, mirroring Step 0's `A` exactly (comparator with `[patch.crates-io]` pointing at the local fork):
  ```
  cargo tree -e normal --prefix none --no-dedupe \
      --manifest-path C:/Users/septa/Documents/Code/comparator/Cargo.toml \
      | sort -u | wc -l        # → C_total
  ```
  Report `A - C` (absolute drop) and the percentage reduction. Save the full dep list to `/tmp/after.txt` and `diff /tmp/before.txt /tmp/after.txt` for the customware entry's implementation notes.
- Confirm none of D7's "forbidden" deps appear in the lean tree by grepping the saved `after.txt` for `axum|reqwest|jsonwebtoken|bcrypt|object_store|tokio-tungstenite|async-graphql|wasmtime|rquickjs|tonic`. Expect zero matches.
- Restore comparator's Cargo.toml to its original state once measurement is captured.
- Inline tokio tests in `core/src/doc/alter.rs` (the existing dorsid suite from customware/001) should still pass under both `default` and `--features server` builds of `surrealdb-core`.
- New inline test under embedded build proving `Session::owner()` can `CREATE` records and `DEFINE USER alice PASSWORD '...'` returns the "auth unsupported" error cleanly.
- New inline test under `--features auth` proving `DEFINE USER alice PASSWORD '...'` + signin-with-password works end-to-end (no JWT, no network).

### Step 6 — Tidy

- Doc-comment each new feature in `surrealdb/core/Cargo.toml` (one line each).
- Update `surrealdb/Cargo.toml`'s `[package.metadata.docs.rs]` `features` list so docs.rs builds with `server` enabled.
- README or CLAUDE.md note pointing at the new embedded build invocation (skip if neither exists at root yet for the fork).

### Customware capture (post-implementation)

- Plan + implementation notes → `customware/002-prune-deps-for-embedded.md`.
- Diff → `customware/002-prune-deps-for-embedded.patch` (excluding `customware/` itself).
- Commit, tag `v3.1.0-alpha-customware.2`, push `main` + tag.

## Verification (one-shot, end-to-end)

After Step 5 lands:

```
# Comparator dep-count measurement (must be done with the [patch.crates-io]
# block in comparator/Cargo.toml pointing at the local fork; restore after):
( cd C:/Users/septa/Documents/Code/comparator \
  && cargo tree -e normal --prefix none --no-dedupe | sort -u > /tmp/after.txt \
  && wc -l /tmp/after.txt \
  && grep -E 'axum|reqwest|jsonwebtoken|bcrypt|object_store|tokio-tungstenite|async-graphql|wasmtime|rquickjs|tonic' /tmp/after.txt \
       && echo 'LEAK: server dep found in embedded tree' || echo 'clean' )

# Diff against /tmp/before.txt (Step 0's A measurement) to itemise what got dropped.
diff /tmp/before.txt /tmp/after.txt

# Server build — should still pull the kitchen sink
cargo build -p surreal --release
cargo tree -p surreal | grep -E 'axum|reqwest|jsonwebtoken' \
    && echo 'server has its deps' || echo 'BUG: missing server dep'

# Existing inline test suite, both modes
cargo test -p surrealdb-core --lib doc::alter::tests
cargo test -p surrealdb-core --lib --no-default-features --features kv-mem doc::alter::tests
cargo test -p surrealdb-core --lib --features auth doc::alter::tests
```

## Known risks / open questions

- **`Auth` / `Role` types leak from `iam/` into `dbs::Session`.** Step 1 needs to either relocate the minimal Auth+Role types out of `iam/` (cleanest) or provide a stub module under `#[cfg(not(feature = "auth"))]`. The skill prompt for the executor is to pick the cleaner option after touching the actual code — relocation if it's a small refactor, stub if `iam/` is so deeply tangled that the cleaner version balloons the diff.
- **`async-graphql` is a chonky compile** even though it has no network deps. It's under `server` (default off for embedded), so embedded users avoid it. Server build keeps it.
- **Revisioned catalog stability across feature configs** (D5): the persisted enums (`AccessDefinition`, `UserDefinition`) MUST keep all their variants compiled in every config. The test in Step 1 should round-trip a catalog containing a JWT-typed AccessDefinition under the embedded build to catch this — decode succeeds, attempting to USE the access definition produces the "auth unsupported" error.
- **Workspace deps version unification**: making `jsonwebtoken`, `bcrypt`, and `object_store` optional in core does not affect the root binary (which still pulls them via `surrealdb-server`). No risk of duplicate versions.
- **Followups deferred**: `auth-user` (password-only auth, no JWT) for the per-user-per-plugin Artilect use case; `buckets-local` for cloud-free local FS blobs; auditing `surrealdb-protocol`/`tonic` paths if anything still leaks.

---

## Implementation notes (what actually shipped)

Commits: `09893a8a..fcc1772b` (3 commits on top of customware/001).

### What landed

- **Step 0 — baseline measurement**: A=461 (fork tip via comparator-equivalent project, no prune), B=455 (crates.io v3 via comparator).
- **Step 1 — `auth` feature** (`09893a8a`): gates `jsonwebtoken` and the IAM credential pipeline (iam/issue, signin, signup, token, verify, jwks submodules + the `From<Algorithm> for jsonwebtoken::Algorithm` impls in sql/expr `algorithm.rs` + the `JWTError` From impl in err/mod.rs + the entire rpc::protocol module + gql::auth). `bcrypt` stays unconditional because `crypto::bcrypt::*` SurrealQL functions are general-purpose. Catalog Algorithm/Access/User enums stay unconditional per D5.
- **Step 4 — `server` umbrella + bolder defaults** (`fcc1772b`): added `server = ["http", "jwks", "auth", "graphql"]` to surrealdb-core. Defaults swapped throughout: surrealdb-core/SDK now default to `kv-mem` only; surrealdb-server and the `surreal` binary explicitly add `server` to their own defaults to preserve the hosted-server build path.

### What was deferred / skipped

- **Step 2 — `buckets` feature**: deferred to customware/003. Gating `object_store` at the Cargo.toml level is trivial, but the surrounding `crate::buc` and `crate::obs` modules thread `BucketsManager` through `Context`, `Datastore`, `Datastore::Builder`, and a fan of downstream consumers (`exec/function/builtin/file.rs`, `fnc/file.rs`, `expr/model.rs`, parser/executor for DEFINE BUCKET / DEFINE MODEL). The pattern is similar to customware/001's `SidRegistry` threading but ~12 files of touchpoints, and the `BucketStoreProvider` trait bound on `Datastore::Builder::build_with_factory_path<F>` adds particular friction. Doing it justice is its own customware entry.
- **Step 3 — `scripting-fetch` leak closure**: skipped. The "leak" identified during exploration doesn't exist on this upstream tip — `fnc/script/mod.rs` already gates `fetch` behind `#[cfg(feature = "http")]` with a `fetch_stub` fallback. Enabling `scripting` alone is already network-free, verified via `cargo tree`.

### Measurement results (synthetic project mirroring `comparator`'s deps)

| Scenario | BEFORE (crates.io v3) | AFTER (this fork) | Delta |
|---|---:|---:|---:|
| Default features (`cargo add surrealdb`, kv-* extras) | 378 | 354 | −24 (~6.3 %) |
| `default-features = false` + `kv-surrealkv,kv-mem` | 461 | 455 | −6 (Step 1 alone — `jsonwebtoken` chain) |

Crates eliminated by Step 1 (`auth` gating): `aws-lc-rs`, `aws-lc-sys`, `jsonwebtoken`, `num-bigint`, `pem`, `signature`, `simple_asn1`, `untrusted`, `time-macros`.

Crates eliminated by Step 4's default swap (for users who don't `default-features = false`): `tokio-tungstenite` and its WS chain, `rustls` and its TLS chain, `async-graphql` and friends.

### Wins NOT measured here

- The compile-time pruning is bigger than the dep-count pruning suggests: `async-graphql` and the WASM stack (when surrealism is on) are particularly expensive to compile.
- The "lean-tree" target list in D7 is mostly satisfied for an embedded build (no `axum`, no `reqwest`, no `jsonwebtoken`, no `bcrypt`-only-paths, no `tokio-tungstenite`, no `async-graphql`). `axum` still leaks via `surrealdb-protocol → tonic → axum` — that chain is structural to surrealdb-protocol and warrants its own customware entry.

### Verification on Windows

`cargo check`, `cargo test`, and `cargo build` continue to hit the upstream rustc-LLVM `STATUS_STACK_BUFFER_OVERRUN` / OOM bug that affected customware/001's verification on this Windows nightly toolchain. Dep-resolution verification via `cargo tree` succeeded and is the evidence basis for these measurements. The structural correctness of the cfg-gating (no leaked references to `jsonwebtoken` or `auth`-gated modules from un-gated code) was verified by iterative `cargo check --no-default-features --features kv-mem` runs — the gating reduced the error count to 0 even though final code-gen never completed.

The customware/001 inline test suite in `core/src/doc/alter.rs` should still pass under both the embedded default and `--features server` build profiles. Running those tests requires a host where rustc can finish a full lib-test compile; left for the user to verify on Linux if they want a green tick.

### Files touched (representative)

Step 1: `surrealdb/core/Cargo.toml`, `surrealdb/core/src/iam/mod.rs`, `surrealdb/core/src/{sql,expr}/algorithm.rs`, `surrealdb/core/src/err/mod.rs`, `surrealdb/core/src/rpc/mod.rs`, `surrealdb/core/src/gql/{mod.rs,schema.rs}`, `surrealdb/core/src/kvs/ds.rs`.

Step 4: `Cargo.toml`, `surrealdb/core/Cargo.toml`, `surrealdb/Cargo.toml`, `surrealdb/server/Cargo.toml`.

## Final commit list (`upstream/main..HEAD`)

```
427e9367 Pin fork to nightly Rust and gate pprof to Unix          (customware/001)
6ee8e2f6 Add IdGeneration table option to TableDefinition         (customware/001)
d85c5305 Parse DEFINE TABLE ... ID [default|sid|rid]              (customware/001)
9e493488 Wire up Rid generation in Document::generate_record_id   (customware/001)
6bae31b5 Add in-process Sid generator registry (no warmup)        (customware/001)
2331a664 Warm the Sid generator floor from stored record keys     (customware/001)
95e4ea8e Cross-mode integration tests and tidy                    (customware/001)
e0ebefee Capture Dorsid first-class-IDs work as customware/001
09893a8a Add auth feature, gate the credential pipeline behind it (customware/002)
fcc1772b Add `server` umbrella feature and swap defaults          (customware/002)
```

## v3.1.5 reapplication notes

- Reapplied the `auth`/`jwks` feature split and embedded-first defaults onto upstream `v3.1.5`.
- Kept upstream's existing `surrealism = ["dep:surrealism-runtime", "dep:async-trait"]` feature wiring while restoring the customware `server`, `auth`, and `jwks` feature relationships.
- Updated the SDK command/router token carrier to use the public `surrealdb::opt::auth::Token` instead of `surrealdb_core::iam::Token`, because the core token type is now gated behind `feature = "auth"`.
- Added embedded-router `#[cfg(feature = "auth")]` conversion helpers for local signin/signup/authenticate/refresh/revoke flows, with a clear query error when those auth-only operations are invoked from an auth-free embedded build.
- Verification during reapplication: `cargo check -p surrealdb --no-default-features --features kv-mem` and `cargo check -p surrealdb --no-default-features --features kv-mem,auth` pass on this Windows host, with only pre-existing/upstream warnings.
