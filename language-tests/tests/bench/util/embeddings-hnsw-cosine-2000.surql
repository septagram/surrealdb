/**
# 2000 768-dimensional random embeddings with a cosine HNSW index, for indexed
# nearest-neighbour benches. A realistic embedding dimension (768) is used so
# the per-distance magnitude cost is representative. `run = false`.

[test]
run = false

[[test.results]]
value = "'OK'"

[[test.results]]
value = "NONE"
*/

{
	DEFINE TABLE embeddings SCHEMALESS;
	CREATE |embeddings:2000| SET vec = array::map(array::range(0, 768), |$i| rand::float(0.0, 1.0)) RETURN NONE;
	RETURN 'OK';
};
-- Separate statement (own transaction) so the rows above are committed when
-- the synchronous build scans the table: the index is populated and ready
-- when the import completes. Inside the block the build would be deferred
-- until after commit and benches could race it (see issue #637).
DEFINE INDEX emb_hnsw ON embeddings FIELDS vec HNSW DIMENSION 768 DIST COSINE TYPE F32 EFC 200 M 12;
