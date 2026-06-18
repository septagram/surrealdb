/**
[test]
reason = "Import: defines the restricted `secret` table and functions (as root) whose body sorts `secret` by the restricted `code` field. Wrapping the SELECT in a DEFINE FUNCTION body forces the runtime-resolved DynamicScan access path (the same construct as reproduction 7130), so the caller exercises the execute-time ORDER BY guard in operators/scan/dynamic.rs rather than the plan-time guard. The `secret` table is defined inline (mirroring order_by_field_select_perm_leak_import.surql) because this release branch's test harness does not resolve imports-of-imports."
run = false
*/

-- The table itself is fully selectable by record users, but the `code` field is
-- only visible to its owner. An index covers `code` so the planner could be
-- tempted to satisfy `ORDER BY code` via an index walk (true value order).
DEFINE TABLE secret SCHEMALESS PERMISSIONS FOR select FULL;
DEFINE FIELD code ON secret TYPE string PERMISSIONS FOR select WHERE owner = $auth.id;
DEFINE FIELD owner ON secret TYPE record;
DEFINE INDEX code_idx ON secret FIELDS code;

-- Rows owned by the victim. Hidden-value order (a < c < m) deliberately differs
-- from record-id order (secret:1 < secret:2 < secret:3).
CREATE secret:1 SET code = 'm', owner = users:victim;
CREATE secret:2 SET code = 'a', owner = users:victim;
CREATE secret:3 SET code = 'c', owner = users:victim;

DEFINE FUNCTION OVERWRITE fn::secret_ids_by_code() {
    RETURN SELECT VALUE id FROM secret ORDER BY code;
};

DEFINE FUNCTION OVERWRITE fn::secret_ids_by_code_desc() {
    RETURN SELECT VALUE id FROM secret ORDER BY code DESC;
};

DEFINE FUNCTION OVERWRITE fn::secret_ids_by_code_limited() {
    RETURN SELECT VALUE id FROM secret ORDER BY code LIMIT 2;
};
