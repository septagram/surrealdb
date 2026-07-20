/**
[env]
namespace = true
database = true
auth = { level = "owner" }
# The deferred build completes asynchronously shortly after the block's
# transaction commits; the sleep gives it time before INFO is checked.
timeout = 15000
context-timeout = 15000

[test]
reason = """
DEFINE INDEX in the same transaction as the row writes must not publish an
empty index. The build is deferred until after commit: right after the block
the index reports building and queries fall back to a table scan (correct
results), and once the deferred build completes the index is ready and fully
populated.
"""
issue = 637

[[test.results]]
value = "'OK'"

# Immediately after the block: correct counts regardless of build progress
# (the index is still building, so the planner does not use it).
[[test.results]]
value = "[{ count: 500 }]"

[[test.results]]
value = "[{ count: 500 }]"

# SLEEP
[[test.results]]
value = "NONE"

# After the deferred build completes, the index is ready and fully populated.
[[test.results]]
value = "{ building: { initial: 500, pending: 0, status: 'ready', updated: 0 } }"

[[test.results]]
value = "[{ count: 500 }]"
*/

{
	CREATE |t:500| SET col = 42 RETURN NONE;
	DEFINE INDEX idx ON t FIELDS col;
	RETURN 'OK';
};
SELECT count() FROM t WHERE col = 42 GROUP ALL;
SELECT count() FROM t WITH NOINDEX WHERE col = 42 GROUP ALL;
SLEEP 3s;
INFO FOR INDEX idx ON t;
SELECT count() FROM t WHERE col = 42 GROUP ALL;
