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
//! No KV-backed warm-up is performed here. Sid generators are zero-state on
//! creation, so the very first mint after process boot may collide with a
//! previously-stored Sid if the wall clock has not advanced. Step 6 of
//! `plans/okay-let-s-do-the-replicated-shell.md` adds a one-shot warm-up
//! pass that calls [`dorsid::sid::Generator::set_floor`] from a reverse
//! range scan over the table's record keys.

use std::sync::Arc;

use dashmap::DashMap;
use parking_lot::Mutex;

use crate::catalog::{DatabaseId, NamespaceId, TableId};

type GeneratorMap = DashMap<(NamespaceId, DatabaseId, TableId), Arc<Mutex<dorsid::sid::Generator>>>;

pub struct SidRegistry {
	realm_id: u16,
	generators: GeneratorMap,
}

impl SidRegistry {
	/// Construct an empty registry pinned to the given realm id (typically
	/// derived from the `DORSID_REALM_ID` env var; see [`SidRegistry::realm_from_env`]).
	pub fn new(realm_id: u16) -> Self {
		Self {
			realm_id,
			generators: DashMap::new(),
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

	/// Mint the next `Sid` for the given table, lazily constructing the
	/// per-table generator on first call. Sleeps briefly (≤ 1 ms) on sequence
	/// overflow — see `D6` in `plans/okay-let-s-do-the-replicated-shell.md`.
	pub fn next_sid(
		&self,
		ns: NamespaceId,
		db: DatabaseId,
		tb: TableId,
	) -> anyhow::Result<dorsid::Sid> {
		let entry = self.generators.entry((ns, db, tb)).or_try_insert_with(|| {
			dorsid::sid::Generator::new(self.realm_id)
				.map(|g| Arc::new(Mutex::new(g)))
				.map_err(|e| anyhow::anyhow!("dorsid Sid generator init failed: {e}"))
		})?;
		let mut generator = entry.value().lock();
		generator.next().map_err(|e| anyhow::anyhow!("dorsid Sid generation failed: {e}"))
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
