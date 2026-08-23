/**
[test]
reason = """
Lifecycle and precedence rules for the fork's `ID SID` / `ID RID` policy
(customware/001, sidecar storage from customware/005).

The policy lives in its own `!ig` key under the table root rather than on
`TableDefinition`, so its lifecycle is not automatic — this pins the cases where
it must be reset, and the two interactions with upstream 3.2's id-field
handling, which are currently unguarded by design.
"""

[[test.results]]
value = "NONE"

[[test.results]]
value = "NONE"

[[test.results]]
value = "true"

[[test.results]]
value = "NONE"

[[test.results]]
value = "NONE"

[[test.results]]
value = "true"

[[test.results]]
value = "NONE"

[[test.results]]
value = "NONE"

[[test.results]]
value = "NONE"

[[test.results]]
value = "NONE"

[[test.results]]
value = "true"

[[test.results]]
value = "NONE"

[[test.results]]
value = "NONE"

[[test.results]]
value = "NONE"

[[test.results]]
value = "'pinned'"

[[test.results]]
value = "NONE"

[[test.results]]
value = "NONE"

# The minted Sid is time-dependent, so match on the shape of the coercion
# failure rather than the literal key.
[[test.results]]
match = "$error CONTAINS 'Expected `uuid` but found'"
error = true

*/

-- DEFINE TABLE OVERWRITE without an `ID` clause resets the policy to default.
DEFINE TABLE reset_tb ID SID;
LET $sid_id = (CREATE ONLY reset_tb RETURN VALUE id);
type::is_number(record::id($sid_id));
DEFINE TABLE OVERWRITE reset_tb;
LET $after_overwrite = (CREATE ONLY reset_tb RETURN VALUE id);
type::is_string(record::id($after_overwrite));

-- REMOVE TABLE drops the policy with the rest of the table prefix, so a table
-- redefined under the same name starts from the default again.
DEFINE TABLE removed_tb ID RID;
REMOVE TABLE removed_tb;
DEFINE TABLE removed_tb;
LET $after_remove = (CREATE ONLY removed_tb RETURN VALUE id);
type::is_string(record::id($after_remove));

-- An `id` field `DEFAULT` is evaluated before the table's `IdGeneration`
-- policy, so it silently wins over `ID SID`. Field-level beats table-level.
DEFINE TABLE default_wins ID SID SCHEMAFULL;
DEFINE FIELD id ON default_wins TYPE string DEFAULT "pinned";
LET $defaulted = (CREATE ONLY default_wins RETURN VALUE id);
record::id($defaulted);

-- A declared `id` kind that cannot hold an i64 makes `ID SID` fail on every
-- insert: the minted integer key is rejected by `coerce_id_key`. The
-- contradiction is visible at DEFINE time but not currently rejected there.
DEFINE TABLE contradiction ID SID SCHEMAFULL;
DEFINE FIELD id ON contradiction TYPE uuid;
CREATE contradiction;
