//! Stores this fork's per-table Dorsid id-generation policy.
//!
//! # Why this is a sidecar key rather than a `TableDefinition` field
//!
//! `IdGeneration` used to live as a field on [`crate::catalog::TableDefinition`],
//! which is an upstream-owned revisioned struct. That put the fork's field in
//! upstream's revision namespace: when upstream took revision 3 for
//! `cache_lives_ts`, the fork's revision-3 layout and upstream's revision-3
//! layout became two different things wearing the same number, and data written
//! by either side misdecoded on the other.
//!
//! There is no way to version around that. `revision-derive` validates revision
//! contiguity at compile time (`resolve_history`: "revisions must be contiguous
//! starting from 1"), so the fork cannot claim a reserved high range, and the
//! decode arms are generated from the declared history rather than dispatched at
//! runtime.
//!
//! Storing the policy under a fork-owned key tag instead makes the collision
//! structurally impossible: [`IdGeneration`] is a fork-owned revisioned type
//! whose revision number only this fork can ever bump.
//!
//! See `customware/README.md` for the general rule this is an instance of, and
//! `customware/001-dorsid-first-class-record-ids.md` for the full history.
//!
//! # Lifecycle
//!
//! The key lives under [`TableRoot`], so it is removed automatically by the
//! single prefix delete that `REMOVE TABLE` performs — no explicit cleanup
//! exists or is needed, exactly as for `!ev` / `!fd` / `!ix`. The same sweep
//! runs when a table is redefined as a view and on the ALTER compaction path,
//! which correctly resets the policy in both cases.
//!
//! **An absent key means [`IdGeneration::Default`].** No key is written for
//! tables using the default policy, so a table that does not opt into Dorsid ids
//! leaves no fork-specific trace in the keyspace at all.

use storekey::{BorrowDecode, Encode};

use crate::catalog::{DatabaseId, IdGeneration, NamespaceId};
use crate::key::category::{Categorise, Category};
use crate::key::table::all::TableRoot;
use crate::kvs::impl_kv_key_storekey;
use crate::val::TableName;

/// Key structure for the per-table Dorsid id-generation policy.
///
/// Encodes as `/*{ns}*{db}*{tb}\0!ig` — a singleton per table, so there is no
/// trailing name component.
#[derive(Clone, Debug, Eq, PartialEq, PartialOrd, Encode, BorrowDecode)]
#[storekey(format = "()")]
pub(crate) struct Ig<'a> {
	table_root: TableRoot<'a>,
	_c: u8,
	_d: u8,
	_e: u8,
}

impl_kv_key_storekey!(Ig<'_> => IdGeneration);

impl Categorise for Ig<'_> {
	fn categorise(&self) -> Category {
		Category::TableIdGeneration
	}
}

impl<'a> Ig<'a> {
	/// Creates a new id-generation policy key for the given table.
	pub fn new(ns: NamespaceId, db: DatabaseId, tb: &'a TableName) -> Self {
		Ig {
			table_root: TableRoot::new(ns, db, tb),
			_c: b'!',
			_d: b'i',
			_e: b'g',
		}
	}
}

pub fn new<'a>(ns: NamespaceId, db: DatabaseId, tb: &'a TableName) -> Ig<'a> {
	Ig::new(ns, db, tb)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::kvs::KVKey;

	#[test]
	fn key() {
		let tb = TableName::from("testtb");
		let val = Ig::new(NamespaceId(1), DatabaseId(2), &tb);
		let enc = Ig::encode_key(&val).unwrap();
		assert_eq!(enc, b"/*\x00\x00\x00\x01*\x00\x00\x00\x02*testtb\0!ig");
	}

	/// The key must sort inside the table's own prefix, which is what makes the
	/// `REMOVE TABLE` prefix sweep collect it without explicit cleanup.
	#[test]
	fn key_is_under_table_root() {
		let tb = TableName::from("testtb");
		let root = TableRoot::new(NamespaceId(1), DatabaseId(2), &tb).encode_key().unwrap();
		let key = Ig::new(NamespaceId(1), DatabaseId(2), &tb).encode_key().unwrap();
		assert!(key.starts_with(&root), "!ig key must live under the table root prefix");
	}
}
