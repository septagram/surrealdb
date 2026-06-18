/**
[env]
namespace = true
database = true

[test]
reason = "DEFINE ANALYZER mapper() must enforce the configured file_allowlist. The test harness allows only the `../tests` tree, so a mapper path outside it must be denied rather than read off the host filesystem."

[[test.results]]
error = true
match = "$error = /File access denied/"

*/

-- A mapper() path outside the configured file_allowlist must be rejected at
-- definition time, before the file is opened. Reading it would otherwise leak
-- arbitrary host file content to the caller.
DEFINE ANALYZER pwn TOKENIZERS blank FILTERS mapper('../Cargo.toml');
