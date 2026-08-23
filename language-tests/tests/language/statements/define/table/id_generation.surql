/**
[test]
reason = """
End-to-end coverage for the fork's `DEFINE TABLE ... ID DEFAULT|SID|RID` clause
(customware/001). Until this file existed the feature had only a parser
round-trip unit test and no behavioural coverage at all, while the dispatch
underneath it was rewritten twice: once when upstream 3.2 introduced the
id-field DEFAULT path and `coerce_id_key`, and once when the policy moved to a
fork-owned sidecar key (customware/005).

Sid and Rid values are time- and entropy-dependent, so nothing here asserts a
literal id. Every check is a boolean.
"""

[[test.results]]
value = "NONE"

[[test.results]]
value = "NONE"

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
value = "true"

[[test.results]]
value = "NONE"

[[test.results]]
value = "true"

[[test.results]]
value = "NONE"

[[test.results]]
value = "true"

[[test.results]]
value = "NONE"

[[test.results]]
value = "true"

[[test.results]]
value = "NONE"

[[test.results]]
value = "true"

[[test.results]]
value = "true"

[[test.results]]
value = "true"

[[test.results]]
value = "NONE"

[[test.results]]
value = "'SID'"

[[test.results]]
value = "'RID'"

[[test.results]]
value = "NONE"

*/

-- One table per policy, plus the implicit (no clause) case.
DEFINE TABLE sid_tb ID SID;
DEFINE TABLE rid_tb ID RID;
DEFINE TABLE default_tb ID DEFAULT;
DEFINE TABLE implicit_tb;

-- `ID SID` mints integer keys that strictly increase.
LET $a = (CREATE ONLY sid_tb RETURN VALUE id);
LET $b = (CREATE ONLY sid_tb RETURN VALUE id);
type::is_number(record::id($a));
record::id($b) > record::id($a);

-- `ID RID` mints integer keys.
LET $r = (CREATE ONLY rid_tb RETURN VALUE id);
type::is_number(record::id($r));

-- `ID DEFAULT` and the implicit form both keep upstream's random string key.
LET $d = (CREATE ONLY default_tb RETURN VALUE id);
type::is_string(record::id($d)) AND string::len(record::id($d)) == 20;

LET $i = (CREATE ONLY implicit_tb RETURN VALUE id);
type::is_string(record::id($i)) AND string::len(record::id($i)) == 20;

-- The DDL round-trips the clause, and omits it for the default policy so that
-- non-Dorsid tables render exactly as upstream renders them.
LET $tables = (INFO FOR DB).tables;
string::contains($tables.sid_tb, "ID SID");
string::contains($tables.rid_tb, "ID RID");
!string::contains($tables.default_tb, " ID ") AND !string::contains($tables.implicit_tb, " ID ");

-- INFO ... STRUCTURE surfaces the policy only for the tables that use it; a
-- default-policy table carries no `id_generation` key at all.
LET $structured = (INFO FOR DB STRUCTURE).tables;
$structured[WHERE name = "sid_tb"][0].id_generation;
$structured[WHERE name = "rid_tb"][0].id_generation;
$structured[WHERE name = "default_tb"][0].id_generation;
