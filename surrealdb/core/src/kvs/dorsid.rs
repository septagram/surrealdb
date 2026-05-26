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
		let mut g = generator.lock();
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
