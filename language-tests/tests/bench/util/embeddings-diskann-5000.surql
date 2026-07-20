/**
# 5000 8-dimensional random embeddings with a DiskANN index, for indexed
# nearest-neighbour benches. `run = false`.

[test]
run = false

[[test.results]]
value = "'OK'"

[[test.results]]
value = "NONE"
*/

{
	DEFINE TABLE embeddings SCHEMALESS;
	CREATE |embeddings:5000| SET vec = array::map(array::range(0, 8), |$i| rand::float(0.0, 1.0)) RETURN NONE;
	RETURN 'OK';
};
-- Separate statement (own transaction) so the rows above are committed when
-- the synchronous build scans the table: the index is populated and ready
-- when the import completes. Inside the block the build would be deferred
-- until after commit and benches could race it (see issue #637).
DEFINE INDEX emb_diskann ON embeddings FIELDS vec DISKANN DIMENSION 8 DIST EUCLIDEAN TYPE F32;
