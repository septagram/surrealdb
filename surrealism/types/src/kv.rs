//! Shared types for the Surrealism KV store.
//!
//! These types cross the host/guest boundary in spirit — the wire format
//! is the FlatBuffers-encoded `serialized-value` defined in WIT — and are
//! presented as the same Rust enum on both sides so plugin code and host
//! code can speak about CAS outcomes in the same vocabulary.

use surrealdb_types::Value;

/// Outcome of a `compare_and_swap` operation.
///
/// Returned as a value (not a `Result::Err`) because mismatch is normal
/// control flow in CAS retry loops, not an error condition. Genuine
/// errors (key length limits, capability denials, host issues) live in
/// the surrounding `anyhow::Result` instead.
#[derive(Debug, Clone, PartialEq)]
pub enum SwapResult {
	/// The swap was performed: the prior value equalled `expected`,
	/// and the kv was updated to `new`. The previous value is `expected`
	/// itself (which the caller already supplied), so it is not echoed
	/// back here.
	Swapped,
	/// The current value did not match `expected`; the kv was not
	/// modified. `actual` is the value as observed under the store's
	/// write lock — pass it as the next `expected` to retry in a single
	/// round-trip rather than two.
	Mismatched(Option<Value>),
}
