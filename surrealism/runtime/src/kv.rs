//! Per-module key-value store (in-memory only).
//!
//! The KV store is shared across all invocations of a module and persists for
//! the lifetime of the [`Runtime`](crate::runtime::Runtime). Each module
//! gets its own isolated store backed by a `BTreeMap` with `RwLock` for
//! concurrent access from multiple controllers.
//!
//! **Volatile:** Data lives only in process memory. It is lost on server
//! restart, module eviction from cache, or Runtime drop.

use std::collections::BTreeMap;
use std::ops::Bound;

use anyhow::Result;
use async_trait::async_trait;
use parking_lot::RwLock;
pub use surrealism_types::kv::SwapResult;

/// Maximum allowed key length in bytes. Prevents memory abuse through
/// excessively long keys while allowing hierarchical paths like
/// `cache::user::preferences::theme`.
pub const MAX_KV_KEY_BYTES: usize = 1024;

#[async_trait]
pub trait KVStore: Send + Sync {
	async fn get(&self, key: String) -> Result<Option<surrealdb_types::Value>>;
	async fn set(&self, key: String, value: surrealdb_types::Value) -> Result<()>;
	async fn del(&self, key: String) -> Result<()>;
	async fn exists(&self, key: String) -> Result<bool>;

	async fn del_rng(&self, start: Bound<String>, end: Bound<String>) -> Result<()>;

	async fn get_batch(&self, keys: Vec<String>) -> Result<Vec<Option<surrealdb_types::Value>>>;
	async fn set_batch(&self, entries: Vec<(String, surrealdb_types::Value)>) -> Result<()>;
	async fn del_batch(&self, keys: Vec<String>) -> Result<()>;

	async fn keys(&self, start: Bound<String>, end: Bound<String>) -> Result<Vec<String>>;
	async fn values(
		&self,
		start: Bound<String>,
		end: Bound<String>,
	) -> Result<Vec<surrealdb_types::Value>>;
	async fn entries(
		&self,
		start: Bound<String>,
		end: Bound<String>,
	) -> Result<Vec<(String, surrealdb_types::Value)>>;
	async fn count(&self, start: Bound<String>, end: Bound<String>) -> Result<u64>;

	/// Atomically add `delta` to the i64 value at `key`, returning the
	/// value as it was *before* the add. Mirrors
	/// [`std::sync::atomic::AtomicI64::fetch_add`].
	///
	/// On absent key the prior value is treated as 0. Errors if the
	/// existing value is not an integer or if the addition overflows.
	async fn fetch_add(&self, key: String, delta: i64) -> Result<i64>;

	/// Atomically compare the value at `key` against `expected`; if
	/// they match, replace it with `new` (or delete it if `new` is
	/// `None`). Returns [`SwapResult::Swapped`] on success or
	/// [`SwapResult::Mismatched`] carrying the actual current value
	/// (suitable as the next `expected` for a one-round-trip retry).
	///
	/// `expected = None` expresses set-if-absent;
	/// `new = None` expresses delete-if-equals.
	async fn compare_and_swap(
		&self,
		key: String,
		expected: Option<surrealdb_types::Value>,
		new: Option<surrealdb_types::Value>,
	) -> Result<SwapResult>;
}

/// In-memory BTreeMap implementation of KVStore with optional size limits.
///
/// Shared across all invocations of a module via `Arc`. Uses
/// `parking_lot::RwLock` for interior mutability (non-poisoning, lower
/// overhead than `std::sync::RwLock`).
pub struct BTreeMapStore {
	inner: RwLock<BTreeMap<String, surrealdb_types::Value>>,
	max_entries: Option<usize>,
	max_value_bytes: Option<usize>,
}

impl BTreeMapStore {
	pub fn new() -> Self {
		Self {
			inner: RwLock::new(BTreeMap::new()),
			max_entries: None,
			max_value_bytes: None,
		}
	}

	pub fn with_limits(max_entries: Option<usize>, max_value_bytes: Option<usize>) -> Self {
		Self {
			inner: RwLock::new(BTreeMap::new()),
			max_entries,
			max_value_bytes,
		}
	}

	fn check_key_length(key: &str) -> Result<()> {
		if key.len() > MAX_KV_KEY_BYTES {
			anyhow::bail!(
				"KV key length ({} bytes) exceeds limit ({MAX_KV_KEY_BYTES} bytes)",
				key.len()
			);
		}
		Ok(())
	}

	fn check_value_size(&self, value: &surrealdb_types::Value) -> Result<()> {
		if let Some(max_bytes) = self.max_value_bytes {
			let size = surrealdb_types::encode(value)?.len();
			if size > max_bytes {
				anyhow::bail!("KV value size ({size} bytes) exceeds limit ({max_bytes} bytes)");
			}
		}
		Ok(())
	}

	fn check_entry_count(
		&self,
		map: &BTreeMap<String, surrealdb_types::Value>,
		adding: usize,
	) -> Result<()> {
		if let Some(max) = self.max_entries {
			let new_count = map.len() + adding;
			if new_count > max {
				anyhow::bail!("KV store entry count ({new_count}) would exceed limit ({max})");
			}
		}
		Ok(())
	}
}

impl Default for BTreeMapStore {
	fn default() -> Self {
		Self::new()
	}
}

#[async_trait]
impl KVStore for BTreeMapStore {
	async fn get(&self, key: String) -> Result<Option<surrealdb_types::Value>> {
		let map = self.inner.read();
		Ok(map.get(&key).cloned())
	}

	async fn set(&self, key: String, value: surrealdb_types::Value) -> Result<()> {
		Self::check_key_length(&key)?;
		self.check_value_size(&value)?;
		let mut map = self.inner.write();
		if !map.contains_key(&key) {
			self.check_entry_count(&map, 1)?;
		}
		map.insert(key, value);
		Ok(())
	}

	async fn del(&self, key: String) -> Result<()> {
		let mut map = self.inner.write();
		map.remove(&key);
		Ok(())
	}

	async fn exists(&self, key: String) -> Result<bool> {
		let map = self.inner.read();
		Ok(map.contains_key(&key))
	}

	async fn del_rng(&self, start: Bound<String>, end: Bound<String>) -> Result<()> {
		let mut map = self.inner.write();
		let keys: Vec<String> = map.range((start, end)).map(|(k, _)| k.clone()).collect();
		for key in keys {
			map.remove(&key);
		}
		Ok(())
	}

	async fn get_batch(&self, keys: Vec<String>) -> Result<Vec<Option<surrealdb_types::Value>>> {
		let map = self.inner.read();
		Ok(keys.iter().map(|key| map.get(key).cloned()).collect())
	}

	async fn set_batch(&self, entries: Vec<(String, surrealdb_types::Value)>) -> Result<()> {
		for (key, value) in &entries {
			Self::check_key_length(key)?;
			self.check_value_size(value)?;
		}
		let mut map = self.inner.write();
		let new_keys = entries
			.iter()
			.map(|(k, _)| k.as_str())
			.collect::<std::collections::HashSet<_>>()
			.into_iter()
			.filter(|k| !map.contains_key(*k))
			.count();
		self.check_entry_count(&map, new_keys)?;
		for (key, value) in entries {
			map.insert(key, value);
		}
		Ok(())
	}

	async fn del_batch(&self, keys: Vec<String>) -> Result<()> {
		let mut map = self.inner.write();
		for key in keys {
			map.remove(&key);
		}
		Ok(())
	}

	async fn keys(&self, start: Bound<String>, end: Bound<String>) -> Result<Vec<String>> {
		let map = self.inner.read();
		Ok(map.range((start, end)).map(|(k, _)| k.clone()).collect())
	}

	async fn values(
		&self,
		start: Bound<String>,
		end: Bound<String>,
	) -> Result<Vec<surrealdb_types::Value>> {
		let map = self.inner.read();
		Ok(map.range((start, end)).map(|(_, v)| v.clone()).collect())
	}

	async fn entries(
		&self,
		start: Bound<String>,
		end: Bound<String>,
	) -> Result<Vec<(String, surrealdb_types::Value)>> {
		let map = self.inner.read();
		Ok(map.range((start, end)).map(|(k, v)| (k.clone(), v.clone())).collect())
	}

	async fn count(&self, start: Bound<String>, end: Bound<String>) -> Result<u64> {
		let map = self.inner.read();
		Ok(map.range((start, end)).count() as u64)
	}

	async fn fetch_add(&self, key: String, delta: i64) -> Result<i64> {
		Self::check_key_length(&key)?;
		let mut map = self.inner.write();
		let was_present = map.contains_key(&key);
		let prev = match map.get(&key) {
			Some(surrealdb_types::Value::Number(surrealdb_types::Number::Int(n))) => *n,
			Some(_) => anyhow::bail!("kv fetch_add: key {key:?} is not an integer"),
			None => 0,
		};
		let next = prev
			.checked_add(delta)
			.ok_or_else(|| anyhow::anyhow!("kv fetch_add: overflow on key {key:?}"))?;
		// No check_value_size: encoded i64 is bounded; the check would only ever fire
		// at absurd max_value_bytes configurations that would also reject every existing
		// integer in the store. Reintroduce if/when fetch_add_decimal is added.
		if !was_present {
			self.check_entry_count(&map, 1)?;
		}
		map.insert(key, surrealdb_types::Value::Number(surrealdb_types::Number::Int(next)));
		Ok(prev)
	}

	async fn compare_and_swap(
		&self,
		key: String,
		expected: Option<surrealdb_types::Value>,
		new: Option<surrealdb_types::Value>,
	) -> Result<SwapResult> {
		Self::check_key_length(&key)?;
		if let Some(ref v) = new {
			self.check_value_size(v)?;
		}
		let mut map = self.inner.write();
		let actual = map.get(&key).cloned();
		if actual == expected {
			match new {
				Some(v) => {
					// Inside actual == expected, so expected.is_none() implies the key is
					// currently absent and we're inserting fresh. Avoids a redundant
					// contains_key lookup.
					if expected.is_none() {
						self.check_entry_count(&map, 1)?;
					}
					map.insert(key, v);
				}
				None => {
					map.remove(&key);
				}
			}
			Ok(SwapResult::Swapped)
		} else {
			Ok(SwapResult::Mismatched(actual))
		}
	}
}

#[cfg(test)]
mod tests {
	use surrealdb_types::Value;

	use super::*;

	fn int_val(n: i64) -> Value {
		Value::Number(surrealdb_types::Number::Int(n))
	}

	fn str_val(s: &str) -> Value {
		Value::String(s.into())
	}

	#[tokio::test]
	async fn get_set_del() {
		let store = BTreeMapStore::new();
		assert!(store.get("k".into()).await.unwrap().is_none());

		store.set("k".into(), int_val(42)).await.unwrap();
		assert_eq!(store.get("k".into()).await.unwrap(), Some(int_val(42)));

		store.del("k".into()).await.unwrap();
		assert!(store.get("k".into()).await.unwrap().is_none());
	}

	#[tokio::test]
	async fn exists() {
		let store = BTreeMapStore::new();
		assert!(!store.exists("k".into()).await.unwrap());

		store.set("k".into(), int_val(1)).await.unwrap();
		assert!(store.exists("k".into()).await.unwrap());
	}

	#[tokio::test]
	async fn overwrite() {
		let store = BTreeMapStore::new();
		store.set("k".into(), int_val(1)).await.unwrap();
		store.set("k".into(), int_val(2)).await.unwrap();
		assert_eq!(store.get("k".into()).await.unwrap(), Some(int_val(2)));
	}

	#[tokio::test]
	async fn batch_ops() {
		let store = BTreeMapStore::new();
		store
			.set_batch(vec![
				("a".into(), int_val(1)),
				("b".into(), int_val(2)),
				("c".into(), int_val(3)),
			])
			.await
			.unwrap();

		let vals = store.get_batch(vec!["a".into(), "c".into(), "z".into()]).await.unwrap();
		assert_eq!(vals, vec![Some(int_val(1)), Some(int_val(3)), None]);

		store.del_batch(vec!["a".into(), "c".into()]).await.unwrap();
		assert!(!store.exists("a".into()).await.unwrap());
		assert!(store.exists("b".into()).await.unwrap());
		assert!(!store.exists("c".into()).await.unwrap());
	}

	#[tokio::test]
	async fn range_keys_values_entries() {
		let store = BTreeMapStore::new();
		for c in b'a'..=b'e' {
			let key = String::from(c as char);
			store.set(key, int_val(c as i64)).await.unwrap();
		}

		let keys =
			store.keys(Bound::Included("b".into()), Bound::Excluded("d".into())).await.unwrap();
		assert_eq!(keys, vec!["b".to_string(), "c".to_string()]);

		let vals = store.values(Bound::Included("d".into()), Bound::Unbounded).await.unwrap();
		assert_eq!(vals, vec![int_val(b'd' as i64), int_val(b'e' as i64)]);

		let count = store.count(Bound::Unbounded, Bound::Unbounded).await.unwrap();
		assert_eq!(count, 5);
	}

	#[tokio::test]
	async fn del_rng() {
		let store = BTreeMapStore::new();
		for c in b'a'..=b'e' {
			store.set(String::from(c as char), int_val(c as i64)).await.unwrap();
		}

		store.del_rng(Bound::Included("b".into()), Bound::Excluded("e".into())).await.unwrap();

		assert!(store.exists("a".into()).await.unwrap());
		assert!(!store.exists("b".into()).await.unwrap());
		assert!(!store.exists("c".into()).await.unwrap());
		assert!(!store.exists("d".into()).await.unwrap());
		assert!(store.exists("e".into()).await.unwrap());
	}

	#[tokio::test]
	async fn max_entries_limit() {
		let store = BTreeMapStore::with_limits(Some(2), None);
		store.set("a".into(), int_val(1)).await.unwrap();
		store.set("b".into(), int_val(2)).await.unwrap();

		let err = store.set("c".into(), int_val(3)).await;
		assert!(err.is_err());
		assert!(err.unwrap_err().to_string().contains("exceed limit"));

		// Overwriting existing key should not fail
		store.set("a".into(), int_val(10)).await.unwrap();
	}

	#[tokio::test]
	async fn max_entries_batch_limit() {
		let store = BTreeMapStore::with_limits(Some(2), None);
		store.set("a".into(), int_val(1)).await.unwrap();

		let err = store.set_batch(vec![("b".into(), int_val(2)), ("c".into(), int_val(3))]).await;
		assert!(err.is_err());
	}

	#[tokio::test]
	async fn max_value_bytes_limit() {
		let store = BTreeMapStore::with_limits(None, Some(128));
		// A short string should be fine
		store.set("k".into(), str_val("hi")).await.unwrap();

		// A large string should be rejected
		let big = "x".repeat(1024);
		let err = store.set("k2".into(), str_val(&big)).await;
		assert!(err.is_err());
		assert!(err.unwrap_err().to_string().contains("exceeds limit"));
	}

	#[tokio::test]
	async fn del_nonexistent_is_ok() {
		let store = BTreeMapStore::new();
		store.del("nope".into()).await.unwrap();
	}

	#[tokio::test]
	async fn max_key_length_limit() {
		let store = BTreeMapStore::new();
		let ok_key = "k".repeat(MAX_KV_KEY_BYTES);
		store.set(ok_key, int_val(1)).await.unwrap();

		let bad_key = "k".repeat(MAX_KV_KEY_BYTES + 1);
		let err = store.set(bad_key.clone(), int_val(2)).await;
		assert!(err.is_err());
		assert!(err.unwrap_err().to_string().contains("exceeds limit"));

		let err = store.set_batch(vec![(bad_key, int_val(3))]).await;
		assert!(err.is_err());
	}

	// ── fetch_add ────────────────────────────────────────────────────

	#[tokio::test]
	async fn fetch_add_absent_key_starts_at_zero() {
		let store = BTreeMapStore::new();
		assert_eq!(store.fetch_add("c".into(), 1).await.unwrap(), 0);
		assert_eq!(store.get("c".into()).await.unwrap(), Some(int_val(1)));
	}

	#[tokio::test]
	async fn fetch_add_returns_previous_value() {
		let store = BTreeMapStore::new();
		store.set("c".into(), int_val(10)).await.unwrap();
		assert_eq!(store.fetch_add("c".into(), 5).await.unwrap(), 10);
		assert_eq!(store.get("c".into()).await.unwrap(), Some(int_val(15)));
	}

	#[tokio::test]
	async fn fetch_add_negative_delta_decrements() {
		let store = BTreeMapStore::new();
		store.set("c".into(), int_val(10)).await.unwrap();
		assert_eq!(store.fetch_add("c".into(), -3).await.unwrap(), 10);
		assert_eq!(store.get("c".into()).await.unwrap(), Some(int_val(7)));
	}

	#[tokio::test]
	async fn fetch_add_overflow_errors() {
		let store = BTreeMapStore::new();
		store.set("c".into(), int_val(i64::MAX)).await.unwrap();
		let err = store.fetch_add("c".into(), 1).await;
		assert!(err.is_err());
		assert!(err.unwrap_err().to_string().contains("overflow"));
	}

	#[tokio::test]
	async fn fetch_add_underflow_errors() {
		let store = BTreeMapStore::new();
		store.set("c".into(), int_val(i64::MIN)).await.unwrap();
		let err = store.fetch_add("c".into(), -1).await;
		assert!(err.is_err());
		assert!(err.unwrap_err().to_string().contains("overflow"));
	}

	#[tokio::test]
	async fn fetch_add_non_integer_errors() {
		let store = BTreeMapStore::new();
		store.set("c".into(), str_val("hello")).await.unwrap();
		let err = store.fetch_add("c".into(), 1).await;
		assert!(err.is_err());
		assert!(err.unwrap_err().to_string().contains("not an integer"));
	}

	#[tokio::test]
	async fn fetch_add_concurrent_no_lost_updates() {
		// N parallel fetch_add(key, 1) must produce final value N — the lost-update
		// race that motivates this primitive's existence.
		use std::sync::Arc;
		let store = Arc::new(BTreeMapStore::new());
		const N: i64 = 64;
		let mut handles = Vec::with_capacity(N as usize);
		for _ in 0..N {
			let s = store.clone();
			handles.push(tokio::spawn(async move {
				s.fetch_add("c".into(), 1).await.unwrap();
			}));
		}
		for h in handles {
			h.await.unwrap();
		}
		assert_eq!(store.get("c".into()).await.unwrap(), Some(int_val(N)));
	}

	// ── compare_and_swap ─────────────────────────────────────────────

	#[tokio::test]
	async fn cas_swaps_on_match() {
		let store = BTreeMapStore::new();
		store.set("k".into(), int_val(1)).await.unwrap();
		let r =
			store.compare_and_swap("k".into(), Some(int_val(1)), Some(int_val(2))).await.unwrap();
		assert_eq!(r, SwapResult::Swapped);
		assert_eq!(store.get("k".into()).await.unwrap(), Some(int_val(2)));
	}

	#[tokio::test]
	async fn cas_returns_actual_on_mismatch() {
		let store = BTreeMapStore::new();
		store.set("k".into(), int_val(1)).await.unwrap();
		let r =
			store.compare_and_swap("k".into(), Some(int_val(99)), Some(int_val(2))).await.unwrap();
		assert_eq!(r, SwapResult::Mismatched(Some(int_val(1))));
		// Value unchanged.
		assert_eq!(store.get("k".into()).await.unwrap(), Some(int_val(1)));
	}

	#[tokio::test]
	async fn cas_set_if_absent_succeeds_when_absent() {
		let store = BTreeMapStore::new();
		let r = store.compare_and_swap("k".into(), None, Some(int_val(7))).await.unwrap();
		assert_eq!(r, SwapResult::Swapped);
		assert_eq!(store.get("k".into()).await.unwrap(), Some(int_val(7)));
	}

	#[tokio::test]
	async fn cas_set_if_absent_fails_when_present() {
		let store = BTreeMapStore::new();
		store.set("k".into(), int_val(1)).await.unwrap();
		let r = store.compare_and_swap("k".into(), None, Some(int_val(7))).await.unwrap();
		assert_eq!(r, SwapResult::Mismatched(Some(int_val(1))));
	}

	#[tokio::test]
	async fn cas_delete_if_equals() {
		let store = BTreeMapStore::new();
		store.set("k".into(), int_val(1)).await.unwrap();
		let r = store.compare_and_swap("k".into(), Some(int_val(1)), None).await.unwrap();
		assert_eq!(r, SwapResult::Swapped);
		assert!(store.get("k".into()).await.unwrap().is_none());
	}

	#[tokio::test]
	async fn cas_delete_if_equals_mismatch_keeps_value() {
		let store = BTreeMapStore::new();
		store.set("k".into(), int_val(1)).await.unwrap();
		let r = store.compare_and_swap("k".into(), Some(int_val(99)), None).await.unwrap();
		assert_eq!(r, SwapResult::Mismatched(Some(int_val(1))));
		assert_eq!(store.get("k".into()).await.unwrap(), Some(int_val(1)));
	}

	#[tokio::test]
	async fn cas_over_strings_works() {
		// Design strength: arbitrary-Value CAS, not just fixed-width primitives.
		let store = BTreeMapStore::new();
		store.set("k".into(), str_val("hello")).await.unwrap();
		let r = store
			.compare_and_swap("k".into(), Some(str_val("hello")), Some(str_val("world")))
			.await
			.unwrap();
		assert_eq!(r, SwapResult::Swapped);
		assert_eq!(store.get("k".into()).await.unwrap(), Some(str_val("world")));
	}

	#[tokio::test]
	async fn cas_respects_max_entries_on_set_if_absent() {
		let store = BTreeMapStore::with_limits(Some(1), None);
		store.set("a".into(), int_val(1)).await.unwrap();
		let err = store.compare_and_swap("b".into(), None, Some(int_val(2))).await;
		assert!(err.is_err());
		assert!(err.unwrap_err().to_string().contains("exceed limit"));
	}

	#[tokio::test]
	async fn cas_concurrent_loop_no_lost_updates() {
		// Build an increment via CAS-loop using SwapResult::Mismatched(actual) for
		// 1-round-trip retry. N concurrent loops must produce final value N.
		use std::sync::Arc;
		let store = Arc::new(BTreeMapStore::new());
		const N: i64 = 32;
		let mut handles = Vec::with_capacity(N as usize);
		for _ in 0..N {
			let s = store.clone();
			handles.push(tokio::spawn(async move {
				let mut expected: Option<Value> = s.get("c".into()).await.unwrap();
				loop {
					let cur = match &expected {
						Some(Value::Number(surrealdb_types::Number::Int(n))) => *n,
						None => 0,
						_ => panic!("non-int found"),
					};
					let next = int_val(cur + 1);
					match s
						.compare_and_swap("c".into(), expected.clone(), Some(next))
						.await
						.unwrap()
					{
						SwapResult::Swapped => break,
						SwapResult::Mismatched(actual) => expected = actual,
					}
				}
			}));
		}
		for h in handles {
			h.await.unwrap();
		}
		assert_eq!(store.get("c".into()).await.unwrap(), Some(int_val(N)));
	}
}
