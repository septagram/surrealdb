//! In-process Dorsid `Sid` generator registry.
//!
//! One [`dorsid::sid::Generator`] is maintained per `(NamespaceId, DatabaseId, TableId)`
//! tuple. Generators are lazily constructed on first request and reused for
//! the lifetime of the [`crate::kvs::Datastore`]. Each [`Datastore`] owns its
//! own registry — different datastores don't share state, which keeps tests
//! deterministic under cargo's parallel test runner.
//!
//! The `realm_id` shared by every generator on this registry is captured at
//! `Datastore` construction time from the `DORSID_REALM_ID` environment
//! variable (defaulting to 0). Multi-writer Sid uniqueness is achieved by
//! running each writer with a distinct realm id; collisions between realms
//! are impossible by construction (see the Dorsid spec).
//!
//! On the first mint for a given table after process boot, the registry
//! performs a one-shot warm-up: a reverse range scan over the table's
//! record keys finds the largest existing `Number(i64)` id, wraps it as a
//! [`dorsid::Sid`], and calls
//! [`dorsid::sid::Generator::set_floor`]. This ensures the next mint
//! produces a Sid strictly greater than any previously persisted one — so
//! restarts (and any clock regression that comes with them) cannot
//! resurrect already-used Sids.

use std::sync::Arc;

use dashmap::DashMap;
use parking_lot::Mutex;
use tokio::sync::OnceCell;

use crate::catalog::{DatabaseId, NamespaceId, TableDefinition, TableId};
use crate::kvs::Transaction;
use crate::val::RecordIdKey;

type GeneratorMap = DashMap<(NamespaceId, DatabaseId, TableId), Arc<Mutex<dorsid::sid::Generator>>>;
type WarmupMap = DashMap<(NamespaceId, DatabaseId, TableId), Arc<OnceCell<()>>>;

pub struct SidRegistry {
	realm_id: u16,
	generators: GeneratorMap,
	/// Per-(ns,db,tb) marker that the table's max stored Sid has been
	/// scanned and applied via [`dorsid::sid::Generator::set_floor`].
	warmups: WarmupMap,
}

impl SidRegistry {
	/// Construct an empty registry pinned to the given realm id (typically
	/// derived from the `DORSID_REALM_ID` env var; see [`SidRegistry::realm_from_env`]).
	pub fn new(realm_id: u16) -> Self {
		Self {
			realm_id,
			generators: DashMap::new(),
			warmups: DashMap::new(),
		}
	}

	/// Resolve a realm id from the `DORSID_REALM_ID` environment variable.
	/// Returns `0` when the variable is unset, empty, or invalid — matching
	/// the single-writer default. A non-empty-but-invalid value (e.g. a
	/// non-numeric string or one above 1023) emits a `tracing::warn!` and
	/// falls back to 0.
	pub fn realm_from_env() -> u16 {
		match std::env::var("DORSID_REALM_ID") {
			Ok(val) if !val.is_empty() => match val.parse::<u16>() {
				Ok(id) if id < 1024 => id,
				_ => {
					tracing::warn!(
						"DORSID_REALM_ID={val:?} is not a valid realm (must be 0..=1023); falling back to 0",
					);
					0
				}
			},
			_ => 0,
		}
	}

	/// Realm id that every generator on this registry is initialised with.
	/// Kept as a public accessor for diagnostics and future ALTER TABLE
	/// support that may want to inspect the writer's realm.
	#[expect(dead_code, reason = "diagnostic accessor; not yet consumed in-tree")]
	pub fn realm_id(&self) -> u16 {
		self.realm_id
	}

	/// Get or insert the generator for a table.
	fn get_or_init_generator(
		&self,
		ns: NamespaceId,
		db: DatabaseId,
		tb: TableId,
	) -> anyhow::Result<Arc<Mutex<dorsid::sid::Generator>>> {
		// `DashMap::entry().or_try_insert_with` returns a RefMut, but we
		// just want an owned Arc clone so we don't hold the map lock.
		let entry = self.generators.entry((ns, db, tb)).or_try_insert_with(|| {
			dorsid::sid::Generator::new(self.realm_id)
				.map(|g| Arc::new(Mutex::new(g)))
				.map_err(|e| anyhow::anyhow!("dorsid Sid generator init failed: {e}"))
		})?;
		Ok(entry.value().clone())
	}

	/// Mint the next `Sid` for the given table, lazily constructing the
	/// per-table generator on first call. Sleeps briefly (≤ 1 ms) on sequence
	/// overflow — see `D6` in `plans/okay-let-s-do-the-replicated-shell.md`.
	/// Use [`next_sid_warmed`](Self::next_sid_warmed) when a [`Transaction`]
	/// is in hand — that variant scans the table's max stored Sid on first
	/// call and seeds the generator with [`dorsid::sid::Generator::set_floor`].
	pub fn next_sid(
		&self,
		ns: NamespaceId,
		db: DatabaseId,
		tb: TableId,
	) -> anyhow::Result<dorsid::Sid> {
		let generator = self.get_or_init_generator(ns, db, tb)?;
		let g = generator.lock();
		g.next().map_err(|e| anyhow::anyhow!("dorsid Sid generation failed: {e}"))
	}

	/// Mint the next `Sid` for `tb_def`, performing a one-shot warm-up on
	/// the first call after process boot: a reverse range scan finds the
	/// largest existing `Number(i64)` record id and applies it as the
	/// generator's floor.
	///
	/// Idempotent across calls — subsequent calls hit the cached
	/// [`OnceCell`] and skip the scan.
	pub async fn next_sid_warmed(
		&self,
		txn: &Transaction,
		tb_def: &TableDefinition,
	) -> anyhow::Result<dorsid::Sid> {
		let ns = tb_def.namespace_id;
		let db = tb_def.database_id;
		let tb = tb_def.table_id;

		let cell = self
			.warmups
			.entry((ns, db, tb))
			.or_insert_with(|| Arc::new(OnceCell::new()))
			.value()
			.clone();

		cell.get_or_try_init(|| async { self.warmup_for_table(txn, tb_def).await }).await?;

		self.next_sid(ns, db, tb)
	}

	/// Scan the table's record keys in reverse, decode the largest
	/// `RecordIdKey::Number(i64)`, and feed it to
	/// [`dorsid::sid::Generator::set_floor`]. Silent no-op when the table
	/// is empty or its largest key isn't a `Number` (e.g. a string id
	/// landed there via an explicit-id INSERT).
	async fn warmup_for_table(
		&self,
		txn: &Transaction,
		tb_def: &TableDefinition,
	) -> anyhow::Result<()> {
		let ns = tb_def.namespace_id;
		let db = tb_def.database_id;
		let tb_id = tb_def.table_id;
		let tb_name = &tb_def.name;

		let beg = crate::key::record::prefix(ns, db, tb_name)?;
		let end = crate::key::record::suffix(ns, db, tb_name)?;

		// Reverse scan, limit 1: the lexicographically last record key in
		// the table's byte range. storekey IndexFormat keeps numeric ids
		// numerically ordered, so for an ID SID table this is the highest
		// previously-issued Sid.
		let rows = txn.scan_raw_keys_reverse(beg..end, 1).await?;
		for key_bytes in rows {
			let Ok(record_key) = crate::key::record::RecordKey::decode_key(&key_bytes) else {
				continue;
			};
			if let RecordIdKey::Number(n) = record_key.id {
				let generator = self.get_or_init_generator(ns, db, tb_id)?;
				generator.lock().set_floor(dorsid::Sid::from_bits(n));
			}
		}
		Ok(())
	}
}

impl std::fmt::Debug for SidRegistry {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("SidRegistry")
			.field("realm_id", &self.realm_id)
			.field("tables_seen", &self.generators.len())
			.finish()
	}
}

#[cfg(all(test, feature = "kv-surrealkv"))]
mod tests {
	use surrealdb_types::{RecordIdKey, Value};
	use temp_dir::TempDir;

	use crate::dbs::Session;
	use crate::kvs::Datastore;

	/// Run a statement and fail loudly if any result in it errored.
	///
	/// `Datastore::execute` returns `Ok` for a batch whose statements failed —
	/// the per-statement error is inside the `QueryResult` — so a bare
	/// `.expect()` on the outer result silently ignores DDL failures.
	async fn execute_ok(ds: &Datastore, session: &Session, sql: &str) {
		let results = ds.execute(sql, session, None).await.expect("execute should dispatch");
		for result in results {
			result.result.unwrap_or_else(|e| panic!("statement failed: {sql}: {e}"));
		}
	}

	/// Extract the integer record-id key from a `CREATE ... RETURN VALUE id`
	/// response.
	async fn create_sid(ds: &Datastore, session: &Session) -> i64 {
		let mut res = ds
			.execute("CREATE ONLY sid_tb RETURN VALUE id", session, None)
			.await
			.expect("create should succeed");
		let value = res.remove(0).result.expect("create should not error");
		let Value::RecordId(rid) = value else {
			panic!("expected a record id, got {value:?}");
		};
		match rid.key {
			RecordIdKey::Number(n) => n,
			other => panic!("expected an integer Sid key, got {other:?}"),
		}
	}

	/// The Sid generator is process-local: a fresh `Datastore` over an existing
	/// store starts with an empty registry and must warm its floor from the
	/// highest Sid already persisted, or it would re-mint ids that already
	/// exist.
	///
	/// This is the one part of the Sid path that a fresh-datastore test cannot
	/// reach, and it is also the part that protects against clock regression,
	/// so it gets a dedicated restart test over a real on-disk backend.
	#[tokio::test]
	async fn sid_floor_warms_from_stored_keys_after_restart() {
		let dir = TempDir::new().expect("temp dir");
		let path = format!("surrealkv://{}", dir.child("warmup.skv").display());
		let session = Session::owner().with_ns("test").with_db("test");

		// First boot: define an `ID SID` table and mint a few ids.
		let highest = {
			let ds = Datastore::new(&path).await.expect("open datastore");
			execute_ok(
				&ds,
				&session,
				"DEFINE NAMESPACE test; DEFINE DATABASE test; DEFINE TABLE sid_tb ID SID",
			)
			.await;

			let mut highest = i64::MIN;
			for _ in 0..3 {
				highest = highest.max(create_sid(&ds, &session).await);
			}
			ds.shutdown().await.expect("shutdown");
			highest
		};

		// Second boot over the same store: the registry is empty, so the next
		// mint must come from a floor seeded by scanning what is on disk.
		let ds = Datastore::new(&path).await.expect("reopen datastore");
		let after_restart = create_sid(&ds, &session).await;

		assert!(
			after_restart > highest,
			"Sid after restart ({after_restart}) must exceed the highest persisted id \
			 ({highest}); the generator did not warm its floor from stored keys"
		);
	}
}
