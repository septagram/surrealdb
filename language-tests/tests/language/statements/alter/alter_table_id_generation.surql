/**
[test]
reason = """
`ALTER TABLE ... ID DEFAULT|SID|RID` (customware/001) plus the definition-time
rejection of a policy that contradicts the table's declared `id` kind.

Because the policy lives in a sidecar key rather than on `TableDefinition`, the
ALTER path writes the key directly instead of patching the definition, so it
needs its own coverage.
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
value = "true"

[[test.results]]
value = "NONE"

[[test.results]]
value = "NONE"

[[test.results]]
value = "true"

[[test.results]]
value = "false"

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
match = "$error CONTAINS 'which generates integer record ids'"
error = true

[[test.results]]
value = "NONE"

[[test.results]]
match = "$error CONTAINS 'which generates integer record ids'"
error = true

[[test.results]]
value = "NONE"

[[test.results]]
value = "NONE"

[[test.results]]
value = "true"

*/

-- ALTER switches an existing table onto a Dorsid policy.
DEFINE TABLE t;
LET $before = (CREATE ONLY t RETURN VALUE id);
type::is_string(record::id($before));
ALTER TABLE t ID SID;
LET $after = (CREATE ONLY t RETURN VALUE id);
type::is_number(record::id($after));
string::contains((INFO FOR DB).tables.t, "ID SID");

-- ALTER ... ID DEFAULT switches it back off.
ALTER TABLE t ID DEFAULT;
LET $reverted = (CREATE ONLY t RETURN VALUE id);
type::is_string(record::id($reverted));
string::contains((INFO FOR DB).tables.t, " ID ");

-- An ALTER that does not mention ID leaves the policy alone.
ALTER TABLE t ID RID;
ALTER TABLE t COMMENT "unrelated";
LET $still_rid = (CREATE ONLY t RETURN VALUE id);
type::is_number(record::id($still_rid));

-- Definition-time rejection, from the DEFINE TABLE side.
DEFINE TABLE conflict SCHEMAFULL;
DEFINE FIELD id ON conflict TYPE uuid;
DEFINE TABLE OVERWRITE conflict SCHEMAFULL ID SID;

-- ...and from the DEFINE FIELD side.
DEFINE TABLE conflict2 SCHEMAFULL ID SID;
DEFINE FIELD id ON conflict2 TYPE uuid;

-- An integer-compatible id kind is accepted.
DEFINE TABLE ok_int SCHEMAFULL ID SID;
DEFINE FIELD id ON ok_int TYPE int;
type::is_number(record::id((CREATE ONLY ok_int RETURN VALUE id)));
