use revision::{
	DeserializeRevisioned, Revisioned, SerializeRevisioned, SkipRevisioned, revisioned,
};
use surrealdb_types::{SqlFormat, ToSql, write_sql};
use uuid::Uuid;

use crate::catalog::{DatabaseId, NamespaceId, Permissions, ViewDefinition};
use crate::expr::statements::info::InfoStructure;
use crate::expr::{ChangeFeed, Kind};
use crate::fmt::EscapeKwFreeIdent;
use crate::kvs::impl_kv_value_revisioned;
use crate::sql;
use crate::sql::statements::DefineTableStatement;
use crate::val::{TableName, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct TableId(pub u32);

impl_kv_value_revisioned!(TableId);

impl Revisioned for TableId {
	fn revision() -> u16 {
		1
	}
}

impl SerializeRevisioned for TableId {
	#[inline]
	fn serialize_revisioned<W: std::io::Write>(
		&self,
		writer: &mut W,
	) -> Result<(), revision::Error> {
		SerializeRevisioned::serialize_revisioned(&self.0, writer)
	}
}

impl DeserializeRevisioned for TableId {
	#[inline]
	fn deserialize_revisioned<R: std::io::Read>(reader: &mut R) -> Result<Self, revision::Error> {
		DeserializeRevisioned::deserialize_revisioned(reader).map(TableId)
	}
}

impl SkipRevisioned for TableId {
	#[inline]
	fn skip_revisioned<R: std::io::Read>(reader: &mut R) -> Result<(), revision::Error> {
		<u32 as SkipRevisioned>::skip_revisioned(reader)
	}
}

impl revision::WalkRevisioned for TableId {
	type Walker<'r, R: revision::BorrowedReader + 'r> = revision::LeafWalker<'r, TableId, R>;

	#[inline]
	fn walk_revisioned<'r, R: revision::BorrowedReader>(
		reader: &'r mut R,
	) -> Result<Self::Walker<'r, R>, revision::Error> {
		Ok(revision::LeafWalker::new(reader))
	}
}

#[revisioned(revision = 3)]
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct TableDefinition {
	pub(crate) namespace_id: NamespaceId,
	pub(crate) database_id: DatabaseId,
	pub(crate) table_id: TableId,
	pub(crate) name: TableName,
	pub(crate) drop: bool,
	pub(crate) schemafull: bool,
	pub(crate) view: Option<ViewDefinition>,
	pub(crate) permissions: Permissions,
	pub(crate) changefeed: Option<ChangeFeed>,
	pub(crate) comment: Option<String>,
	pub(crate) table_type: TableType,

	/// The last time that a DEFINE FIELD was added to this table
	pub(crate) cache_fields_ts: Uuid,
	/// The last time that a DEFINE EVENT was added to this table
	pub(crate) cache_events_ts: Uuid,
	/// The last time that a DEFINE TABLE was added to this table
	pub(crate) cache_tables_ts: Uuid,
	/// The last time that a DEFINE INDEX was added to this table
	pub(crate) cache_indexes_ts: Uuid,

	/// The last time the set of LIVE queries on this table changed (a LIVE was
	/// registered or a KILL removed one). Bumped transactionally with the
	/// live-query row write, so the live-query cache keys on committed state —
	/// exactly like `cache_fields_ts` etc. A free-floating in-memory version
	/// (bumped before commit) allowed a concurrent writer with a pre-commit
	/// snapshot to poison the cache with a stale subscriber list. Old tables
	/// default to the nil UUID; the first LIVE/KILL bumps it to a real value.
	#[revision(start = 3)]
	pub(crate) cache_lives_ts: Uuid,

	/// Optional alias used as the GraphQL type / query / mutation prefix for
	/// this table. See GitHub issue #4537. `Option<String>::default()` is
	/// `None`, so the standard `#[revision]` default is sufficient.
	#[revision(start = 2)]
	pub(crate) graphql_alias: Option<String>,

	/// Reason emitted on the GraphQL `@deprecated` directive for every
	/// auto-generated Query/Mutation field that targets this table.
	#[revision(start = 2)]
	pub(crate) graphql_deprecated: Option<String>,
}

impl_kv_value_revisioned!(TableDefinition);

impl TableDefinition {
	pub fn new(
		namespace_id: NamespaceId,
		database_id: DatabaseId,
		table_id: TableId,
		name: TableName,
	) -> Self {
		let now = Uuid::now_v7();
		Self {
			namespace_id,
			database_id,
			table_id,
			name,
			drop: false,
			schemafull: false,
			view: None,
			permissions: Permissions::none(),
			changefeed: None,
			comment: None,
			table_type: TableType::default(),
			cache_fields_ts: now,
			cache_events_ts: now,
			cache_tables_ts: now,
			cache_indexes_ts: now,
			cache_lives_ts: now,
			graphql_alias: None,
			graphql_deprecated: None,
		}
	}

	/// Checks if this table allows normal records / documents
	pub fn allows_normal(&self) -> bool {
		matches!(self.table_type, TableType::Normal | TableType::Any)
	}
	/// Checks if this table allows graph edges / relations
	pub fn allows_relation(&self) -> bool {
		matches!(self.table_type, TableType::Relation(_) | TableType::Any)
	}

	fn to_sql_definition(&self) -> DefineTableStatement {
		DefineTableStatement {
			id: Some(self.table_id.0),
			name: sql::Expr::Table(self.name.clone()),
			drop: self.drop,
			full: self.schemafull,
			view: self.view.clone().map(|v| v.to_sql_definition()),
			permissions: self.permissions.clone().into(),
			changefeed: self.changefeed.map(|v| v.into()),
			comment: self
				.comment
				.clone()
				.map(|v| sql::Expr::Literal(sql::Literal::String(v.into())))
				.unwrap_or(sql::Expr::Literal(sql::Literal::None)),
			table_type: self.table_type.clone().into(),
			graphql_alias: self.graphql_alias.clone(),
			graphql_deprecated: self.graphql_deprecated.clone(),
			..Default::default()
		}
	}
}

impl ToSql for TableDefinition {
	fn fmt_sql(&self, f: &mut String, sql_fmt: SqlFormat) {
		self.to_sql_definition().fmt_sql(f, sql_fmt)
	}
}

impl InfoStructure for TableDefinition {
	fn structure(self) -> Value {
		Value::from(map! {
			"name" => Value::String(self.name.into()),
			"drop" => self.drop.into(),
			"schemafull" => self.schemafull.into(),
			"kind" => self.table_type.structure(),
			"view", if let Some(v) = self.view => v.structure(),
			"changefeed", if let Some(v) = self.changefeed => v.structure(),
			"permissions" => self.permissions.structure(),
			"comment", if let Some(v) = self.comment => v.into(),
			"graphql_alias", if let Some(v) = self.graphql_alias => v.into(),
			"graphql_deprecated", if let Some(v) = self.graphql_deprecated => v.into(),
			"id" => self.table_id.0.into(),
		})
	}
}

/// How a table's auto-generated record ids are minted when a row is created
/// without an explicit `id` field.
///
/// - `Default`: upstream behaviour — a random string key, or whatever the `id` field's declared
///   kind implies (see `Document::generate_typed_id`).
/// - `Sid`: Dorsid `Sid`, a monotonic i64 with per-table warm-up from stored keys.
/// - `Rid`: Dorsid `Rid`, a stateless persistent i64 from CSPRNG entropy.
///
/// # Fork-owned revision namespace
///
/// This value is **not** a field on [`TableDefinition`]. It is persisted on its
/// own under the fork-owned `!ig` key (see [`crate::key::table::ig`]), which
/// means its revision number belongs solely to this fork: upstream can never
/// collide with it, and bumping it is a local decision with no rebase risk.
///
/// It briefly *was* a `TableDefinition` field, and that is exactly what went
/// wrong — upstream took the same revision number for a different field in the
/// 3.2 line and the two layouts became indistinguishable. See
/// `customware/README.md` for the rule that came out of it.
///
/// Per-policy configuration should therefore ride the variants (for example a
/// future `Rid { warmup: u32 }`) and bump this type to revision 2, rather than
/// being bolted onto an upstream-owned struct.
///
/// An absent `!ig` key means [`IdGeneration::Default`]; no key is written for
/// the default policy.
#[revisioned(revision = 1)]
#[derive(Debug, Default, Hash, Clone, Copy, Eq, PartialEq)]
pub enum IdGeneration {
	#[default]
	Default,
	Sid,
	Rid,
}

impl_kv_value_revisioned!(IdGeneration);

impl IdGeneration {
	/// The SurrealQL spelling of this policy, for diagnostics.
	pub(crate) fn as_clause(self) -> &'static str {
		match self {
			IdGeneration::Default => "DEFAULT",
			IdGeneration::Sid => "SID",
			IdGeneration::Rid => "RID",
		}
	}

	/// Whether a key minted by this policy can satisfy a declared `id` kind.
	///
	/// `Sid` and `Rid` mint `RecordIdKey::Number(i64)`, which then flows through
	/// `Document::coerce_id_key` like any other key. If the declared kind cannot
	/// hold an integer, that coercion fails on *every* insert — so the
	/// contradiction is rejected when the schema is defined rather than left to
	/// surface per write.
	///
	/// Mirrors `coerce_id_key`: record kinds constrain the outer record rather
	/// than the key, so they impose no constraint here.
	pub(crate) fn accepts_id_kind(self, kind: &Kind) -> bool {
		match self {
			// The default policy defers to upstream's kind-aware synthesis,
			// which handles (or rejects) every kind on its own terms.
			IdGeneration::Default => true,
			IdGeneration::Sid | IdGeneration::Rid => Self::kind_holds_integer(kind),
		}
	}

	fn kind_holds_integer(kind: &Kind) -> bool {
		match kind {
			Kind::Any | Kind::Int | Kind::Number => true,
			// Constrains the record, not the key.
			k if k.is_record() => true,
			// A union is satisfiable if any branch is.
			Kind::Either(kinds) => kinds.iter().any(Self::kind_holds_integer),
			_ => false,
		}
	}
}

impl InfoStructure for IdGeneration {
	fn structure(self) -> Value {
		match self {
			IdGeneration::Default => "DEFAULT".into(),
			IdGeneration::Sid => "SID".into(),
			IdGeneration::Rid => "RID".into(),
		}
	}
}

/// Fork-local renderers that join a table definition with its sidecar
/// [`IdGeneration`] policy.
///
/// These are kept in their own `impl` block, separate from the upstream one, so
/// the upstream methods stay textually identical and drop out of the rebase
/// conflict surface. Callers that have transaction access (INFO, export) fetch
/// the `!ig` key and use these; everything else keeps rendering upstream's shape.
///
/// Both omit the policy entirely when it is [`IdGeneration::Default`], so a
/// table that does not use Dorsid ids produces output byte-identical to
/// upstream's.
impl TableDefinition {
	/// Like `to_sql_definition`, but carries the table's id-generation policy so
	/// the rendered DDL includes the `ID SID` / `ID RID` clause.
	pub(crate) fn to_sql_definition_with_id_generation(
		&self,
		id_generation: IdGeneration,
	) -> DefineTableStatement {
		DefineTableStatement {
			id_generation: id_generation.into(),
			..self.to_sql_definition()
		}
	}

	/// Like [`InfoStructure::structure`], but includes an `id_generation` key
	/// when the policy is not the default.
	pub(crate) fn structure_with_id_generation(self, id_generation: IdGeneration) -> Value {
		let structured = self.structure();
		if id_generation == IdGeneration::Default {
			return structured;
		}
		let Value::Object(mut object) = structured else {
			return structured;
		};
		object.insert("id_generation".to_string(), id_generation.structure());
		Value::Object(object)
	}
}

/// The type of records stored by a table
#[revisioned(revision = 1)]
#[derive(Debug, Default, Hash, Clone, Eq, PartialEq)]
pub enum TableType {
	#[default]
	Any,
	Normal,
	Relation(Relation),
}

impl ToSql for TableType {
	fn fmt_sql(&self, f: &mut String, sql_fmt: SqlFormat) {
		match self {
			TableType::Any => f.push_str("ANY"),
			TableType::Normal => f.push_str("NORMAL"),
			TableType::Relation(rel) => {
				f.push_str("RELATION");
				if !rel.from.is_empty() {
					f.push_str(" IN ");
					for (idx, k) in rel.from.iter().enumerate() {
						if idx != 0 {
							f.push_str(" | ");
						}
						write_sql!(f, sql_fmt, "{}", EscapeKwFreeIdent(k.as_str()));
					}
				}
				if !rel.to.is_empty() {
					f.push_str(" OUT ");
					for (idx, k) in rel.to.iter().enumerate() {
						if idx != 0 {
							f.push_str(" | ");
						}
						write_sql!(f, sql_fmt, "{}", EscapeKwFreeIdent(k.as_str()));
					}
				}
				if rel.enforced {
					f.push_str(" ENFORCED");
				}
			}
		}
	}
}

impl InfoStructure for TableType {
	fn structure(self) -> Value {
		match self {
			Self::Any => Value::from(map! {
				"kind" => "ANY".into(),
			}),
			Self::Normal => Value::from(map! {
				"kind" => "NORMAL".into(),
			}),
			Self::Relation(rel) => Value::from(map! {
				"kind" => "RELATION".into(),
				"in", if !rel.from.is_empty() =>
					rel.from.into_iter().map(Value::Table).collect::<Vec<_>>().into(),
				"out", if !rel.to.is_empty() =>
					rel.to.into_iter().map(Value::Table).collect::<Vec<_>>().into(),
				"enforced" => rel.enforced.into()
			}),
		}
	}
}

#[revisioned(revision = 2)]
#[derive(Debug, Hash, Clone, Eq, PartialEq)]
pub struct Relation {
	#[revision(end = 2, convert_fn = "rev_convert_from")]
	pub old_from: Option<Kind>,
	/// Contains the tables the relation originates from,
	/// if empty then there was no `IN` clause
	#[revision(start = 2)]
	pub from: Vec<TableName>,
	#[revision(end = 2, convert_fn = "rev_convert_to")]
	pub old_to: Option<Kind>,
	/// Contains the tables the relation goes to,
	/// if empty then there was no `OUT` clause
	#[revision(start = 2)]
	pub to: Vec<TableName>,
	pub enforced: bool,
}

impl Relation {
	fn rev_convert_from(&mut self, _rev: u16, value: Option<Kind>) -> Result<(), revision::Error> {
		if let Some(x) = value {
			let Kind::Record(x) = x else {
				return Err(revision::Error::Conversion(format!(
					"Invalid kind within table relation, should have been a record, found: {:#?}",
					x,
				)));
			};
			self.from = x
		}
		Ok(())
	}
	fn rev_convert_to(&mut self, _rev: u16, value: Option<Kind>) -> Result<(), revision::Error> {
		if let Some(x) = value {
			let Kind::Record(x) = x else {
				return Err(revision::Error::Conversion(format!(
					"Invalid kind within table relation, should have been a record, found: {:#?}",
					x,
				)));
			};
			self.to = x
		}
		Ok(())
	}
}
