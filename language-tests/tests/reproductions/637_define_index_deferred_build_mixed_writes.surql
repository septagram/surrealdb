/**
[env]
namespace = true
database = true
auth = { level = "owner" }
timeout = 15000
context-timeout = 15000

[test]
reason = """
Writes issued after a deferred DEFINE INDEX in the same transaction (creates,
updates, deletes) skip index maintenance and are picked up by the post-commit
initial scan. The final index must exactly reflect the committed table.
"""
issue = 637

[[test.results]]
value = "'OK'"

# Immediately after the block, before the deferred build completes: the
# planner does not use the building index, so results are correct.
[[test.results]]
value = "[{ count: 496 }]"

[[test.results]]
value = "[{ count: 496 }]"

# SLEEP
[[test.results]]
value = "NONE"

# 1..300 and 301..500 are end-exclusive ranges: 498 created - 1 deleted = 497 records scanned into the index.
[[test.results]]
value = "{ building: { initial: 497, pending: 0, status: 'ready', updated: 0 } }"

# Indexed counts after the build: col = 1 (496) and col = 2 (1).
[[test.results]]
value = "[{ count: 496 }]"

[[test.results]]
value = "[{ count: 1 }]"
*/

{
	CREATE |t:1..300| SET col = 1 RETURN NONE;
	DEFINE INDEX idx ON t FIELDS col;
	-- Writes after the deferred definition, in the same transaction:
	CREATE |t:301..500| SET col = 1 RETURN NONE;
	UPDATE t:1 SET col = 2;
	DELETE t:2;
	RETURN 'OK';
};
SELECT count() FROM t WHERE col = 1 GROUP ALL;
SELECT count() FROM t WITH NOINDEX WHERE col = 1 GROUP ALL;
SLEEP 3s;
INFO FOR INDEX idx ON t;
SELECT count() FROM t WHERE col = 1 GROUP ALL;
SELECT count() FROM t WHERE col = 2 GROUP ALL;
