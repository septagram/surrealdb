//! Stores the key prefix for all keys under a table
use std::borrow::Cow;

use storekey::{BorrowDecode, Encode};

use crate::catalog::{DatabaseId, NamespaceId};
use crate::key::category::{Categorise, Category};
use crate::val::TableName;

#[derive(Clone, Debug, Eq, PartialEq, PartialOrd, Encode, BorrowDecode)]
#[storekey(format = "()")]
pub(crate) struct TableRoot<'a> {
	__: u8,
	_a: u8,
	pub ns: NamespaceId,
	_b: u8,
	pub db: DatabaseId,
	_c: u8,
	pub tb: Cow<'a, TableName>,
}

// Hand-written impl (instead of `impl_kv_key_storekey!`) so prefix deletes
// over a whole table (`REMOVE TABLE`) mark the transaction as staging record
// writes on that table: the span covers the table's record keys, and a
// `DEFINE INDEX` later in the same transaction must defer its build rather
// than scan a snapshot that still contains the doomed rows.
impl crate::kvs::KVKey for TableRoot<'_> {
	type ValueType = Vec<u8>;

	fn record_table(&self) -> Option<crate::kvs::RecordTableRef<'_>> {
		Some(crate::kvs::RecordTableRef {
			ns: self.ns,
			db: self.db,
			tb: self.tb.as_ref(),
		})
	}

	fn encode_key(&self) -> anyhow::Result<Vec<u8>> {
		Ok(storekey::encode_vec(self).map_err(|_| crate::err::Error::Unencodable)?)
	}

	fn value_context(&self) {}
}

pub fn new(ns: NamespaceId, db: DatabaseId, tb: &TableName) -> TableRoot<'_> {
	TableRoot::new(ns, db, tb)
}

impl Categorise for TableRoot<'_> {
	fn categorise(&self) -> Category {
		Category::TableRoot
	}
}

impl<'a> TableRoot<'a> {
	#[inline]
	pub fn new(ns: NamespaceId, db: DatabaseId, tb: &'a TableName) -> Self {
		Self {
			__: b'/',
			_a: b'*',
			ns,
			_b: b'*',
			db,
			_c: b'*',
			tb: Cow::Borrowed(tb),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::kvs::KVKey;

	#[test]
	fn key() {
		let tb = TableName::from("testtb");
		let val = TableRoot::new(NamespaceId(1), DatabaseId(2), &tb);
		let enc = TableRoot::encode_key(&val).unwrap();
		assert_eq!(enc, b"/*\x00\x00\x00\x01*\x00\x00\x00\x02*testtb\0");
	}
}
