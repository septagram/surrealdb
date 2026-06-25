use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use clap::ArgMatches;
use semver::Version;
use sha2::{Digest, Sha256};
use surrealdb_core::channel;
use surrealdb_core::dbs::capabilities::Targets;
use surrealdb_core::dbs::{Capabilities, Session};
use surrealdb_core::env::VERSION;
use surrealdb_core::kvs::{Builder, Datastore};

use crate::cli::{Backend, ColorMode};
use crate::cmd::bench::stats::{ComparisonData, MeasurementData};
use crate::cmd::bench::store::{BenchMarkRun, StoreConfig, get_store};
use crate::cmd::bench::{DEFAULT_NOISE_THRESHOLD, DEFAULT_SIGNIFICANCE_THRESHOLD};
use crate::cmd::util::ImportFailure;
use crate::cmd::{graphql, util};
use crate::tests::case::Dialect;
use crate::tests::run::{CaseImports, RunConfig, TestRunId};
use crate::tests::schema::{BoolOr, NewPlannerStrategyConfig, TestConfig};
use crate::tests::{CaseSet, RunSetBuilder, TestRun};

struct BenchRunConfig {
	cache_id: String,
	/// Short label of the dataset variant this run uses (from `[bench].datasets`),
	/// or `None` when the bench runs once against its `[env].imports`.
	variant: Option<String>,
	/// Per-variant import chain to run instead of `case.imports`. `None` falls
	/// back to the case's own resolved imports.
	imports: Option<Vec<Arc<crate::tests::case::TestCase>>>,
}

impl RunConfig for BenchRunConfig {
	fn name(&self, case: &CaseImports) -> String {
		match &self.variant {
			Some(v) => format!("{} [{v}]", case.test.origin.path),
			None => case.test.origin.path.clone(),
		}
	}
}

fn calc_cache_id(case: &CaseImports) -> String {
	static BYTES: &[u8] = b"0123456789abcdef";
	let mut hasher = Sha256::new();
	for i in case.imports.iter() {
		hasher.update(i.origin.path.as_bytes());
		let Ok(epoch) = i.origin.modified.duration_since(SystemTime::UNIX_EPOCH) else {
			continue;
		};
		hasher.update(epoch.as_secs().to_le_bytes());
		hasher.update(epoch.subsec_nanos().to_le_bytes());
	}
	let bytes = hasher.finalize();
	let mut res = String::new();
	for b in bytes.iter() {
		res.push(BYTES[(b & 0b1111) as usize] as char);
		res.push(BYTES[(b >> 4) as usize] as char);
	}
	res
}

struct BenchConfig {
	// Placeholder for the deferred dataset-cache work; scoped allow so the rest
	// of the module stays lint-checked for dead code.
	#[allow(dead_code)]
	ds_cache: String,
	backend: Backend,
	/// Temp directory under which file-backed datastores (rocksdb/surrealkv) are
	/// built; `None` for the in-memory backend. Removed when `run()` returns.
	bench_root: Option<PathBuf>,
	new_planner: NewPlannerStrategyConfig,
}

/// Counter for unique per-datastore subdirectories under `bench_root`.
static DS_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Connection string for the configured backend. File-backed engines get a fresh
/// subdirectory each call, so every `prepare()` builds a clean datastore.
fn datastore_conn(config: &BenchConfig) -> String {
	let scheme = match config.backend {
		Backend::Memory => return "mem://".to_string(),
		Backend::RocksDb => "rocksdb",
		Backend::SurrealKv => "surrealkv",
		Backend::TikV => unreachable!("TiKV is rejected before prepare()"),
	};
	let root = config.bench_root.as_ref().expect("file backend requires a bench root");
	let id = DS_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
	format!("{scheme}://{}", root.join(format!("store_{id}")).display())
}

/// Removes its directory tree on drop, cleaning up file-backed datastores.
struct CleanupDir(PathBuf);

impl Drop for CleanupDir {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.0);
	}
}

/// Creates (clean) the temp root for this run's file-backed datastores.
fn make_bench_root() -> Result<PathBuf> {
	let root = std::env::temp_dir().join(format!("surreal_bench_{}", std::process::id()));
	let _ = std::fs::remove_dir_all(&root);
	std::fs::create_dir_all(&root).context("Could not create bench datastore directory")?;
	Ok(root)
}

struct CmdConfig<'a> {
	path: &'a String,
	filter: Option<&'a String>,
	dataset: Option<&'a String>,
	backend: Backend,
	ds_cache: &'a String,
	save: bool,
	store: StoreConfig<'a>,
}

impl<'a> CmdConfig<'a> {
	fn from_matches(parent: &'a ArgMatches, current: &'a ArgMatches) -> Self {
		let path: &String = current.get_one("path").unwrap();

		let filter = current.get_one::<String>("filter");
		let dataset = current.get_one::<String>("dataset");
		let backend = *current.get_one::<Backend>("backend").unwrap();
		let ds_cache = current.get_one::<String>("ds-cache").unwrap();
		let save = current.get_flag("save");

		Self {
			path,
			filter,
			dataset,
			backend,
			ds_cache,
			save,
			store: StoreConfig::from_matches(parent),
		}
	}
}

/// Main subcommand function, runs the actual subcommand.
pub async fn run(color: ColorMode, parent: &ArgMatches, current: &ArgMatches) -> Result<()> {
	if cfg!(debug_assertions) {
		println!(
			"Warning, debug assertions are enabled, it is likely the benchmarking suite is build without optimization"
		)
	}

	let mut load_errors = Vec::new();

	let cfg = CmdConfig::from_matches(parent, current);
	let set = CaseSet::load_surrealql_files(cfg.path, &mut load_errors).await?;

	// Resolve the backend. The in-memory engine needs no storage; file-backed
	// engines (rocksdb/surrealkv) build into a temporary directory (a fresh
	// subdir per datastore). TiKV needs an external cluster, so it's unsupported.
	let bench_root = match cfg.backend {
		Backend::Memory => None,
		#[cfg(feature = "backend-rocksdb")]
		Backend::RocksDb => Some(make_bench_root()?),
		#[cfg(not(feature = "backend-rocksdb"))]
		Backend::RocksDb => {
			bail!("RocksDb benchmarking requires building with `--features backend-rocksdb`")
		}
		// surrealkv is always available under the `bench` feature (kv-surrealkv).
		Backend::SurrealKv => Some(make_bench_root()?),
		Backend::TikV => {
			bail!("TiKV is not supported for benchmarking (it needs an external cluster)")
		}
	};
	// Removes the whole temp tree when `run()` returns; declared here so it drops
	// after every datastore created during the run.
	let _bench_root_cleanup = bench_root.clone().map(CleanupDir);

	let config = BenchConfig {
		ds_cache: cfg.ds_cache.clone(),
		backend: cfg.backend,
		bench_root,
		new_planner: NewPlannerStrategyConfig::BestEffortRo,
	};

	let mut store = get_store(&cfg.store).await?;

	let core_version = Version::parse(VERSION).unwrap();
	let set_builder = RunSetBuilder::new(&set, &mut load_errors)
		// Only run test for which run is enabled.
		.with_filter(|x| x.test.config.parsed.bench.run)
		// Only run benches whose path matches the optional filter.
		.with_filter(|x| {
			cfg.filter.is_none_or(|filter| x.test.origin.path.contains(filter.as_str()))
		})
		// Only run test for this backend.
		.with_filter(|x| {
			let config_backend = &x.test.config.parsed.env.backend;
			config_backend.is_empty() || config_backend.contains(&cfg.backend)
		})
		// Only run for the right version.
		.with_filter(|x| {
			if let Some(x) = &x.test.config.parsed.test.version
				&& !x.matches(&core_version)
			{
				return false;
			}

			if let Some(x) = &x.test.config.parsed.test.importing_version
				&& !x.matches(&core_version)
			{
				return false;
			}

			for i in x.imports.iter() {
				if let Some(x) = &i.config.parsed.test.version
					&& !x.matches(&core_version)
				{
					return false;
				}
			}

			true
		})
		// One config per bench; dataset-matrix expansion happens after build()
		// (below) where the CaseSet is available for resolving import chains.
		.with_expander(|x| {
			vec![BenchRunConfig {
				cache_id: calc_cache_id(x),
				variant: None,
				imports: None,
			}]
		});

	let built = set_builder.build();

	// Expand each bench that declares `[bench].datasets` into one run per dataset,
	// resolving each dataset's (transitive) import chain. Benches without
	// `datasets` pass through unchanged and use their `[env].imports`.
	let mut runs = Vec::with_capacity(built.len());
	for run in built {
		let datasets = run.case.test.config.parsed.bench.datasets.clone();
		if datasets.is_empty() {
			runs.push(run);
			continue;
		}
		for (name, path) in &datasets {
			// `--dataset` selects a single matrix variant by its (exact) name.
			if let Some(want) = cfg.dataset
				&& name != want.as_str()
			{
				continue;
			}
			let chain = set.resolve_imports(
				std::slice::from_ref(path),
				run.case.test.id,
				&run.case.test.origin,
				&mut load_errors,
			);
			let Some(chain) = chain else {
				continue;
			};
			runs.push(TestRun {
				// Reassigned to a unique value below.
				id: TestRunId::new(0),
				case: run.case.clone(),
				config: BenchRunConfig {
					cache_id: run.config.cache_id.clone(),
					variant: Some(name.clone()),
					imports: Some(chain),
				},
			});
		}
	}

	// Passthrough runs keep their original `build()` ids while expanded runs are
	// freshly created, so renumber to keep ids unique across the final set.
	for (idx, run) in runs.iter_mut().enumerate() {
		run.id = TestRunId::new(idx);
	}

	if runs.is_empty() {
		println!("No benchmarks found, exiting");
		return Ok(());
	}

	let mut measurements = Vec::new();
	for i in runs.into_iter() {
		// `rebuild = true` benches rebuild the datastore and re-run their imports
		// every iteration. On a file backend that means re-importing the dataset
		// to disk each time, which is impractically slow — so restrict mutating
		// benches to the in-memory backend and skip them elsewhere.
		if cfg.backend != Backend::Memory && i.case.test.config.parsed.bench.rebuild {
			println!(
				"Skipping {} (`rebuild = true` benches only run on the `mem` backend)",
				i.name()
			);
			continue;
		}

		// Key the baseline on the run name, not just the file path, so the two
		// variants of a dataset-matrix bench (`[unindexed]`/`[indexed]`) don't
		// share — and overwrite — one baseline slot. For non-matrix benches the
		// name is just the path, so this is unchanged.
		let baseline = store
			.fetch_latest(&i.name(), cfg.backend)
			.await
			.context("Could not fetch latest measurement data")?;

		let measurement = thread::scope(|scope| {
			scope
				.spawn(|| {
					tokio::runtime::Builder::new_multi_thread()
						.enable_all()
						.build()
						.unwrap()
						.block_on(run_bench(&i, &config, baseline))
				})
				.join()
		})
		.map_err(|e| {
			if let Some(x) = e.downcast_ref::<String>() {
				anyhow!("Measurement thread paniced: {x}")
			} else {
				anyhow!("Measurement thread paniced")
			}
		})??;

		match measurement {
			BenchRunResult::Import(imp_fail) => {
				println!(
					"Error, import `{}` returned an error: {}",
					imp_fail.path, imp_fail.message
				);
			}
			BenchRunResult::InsufficientSamples(collected) => {
				println!(
					"Skipping {}: collected only {collected} sample(s) within max_time (need at least 2 to compute statistics)",
					i.name()
				);
			}
			BenchRunResult::Ok(measurement, compare) => {
				// Persist each result the moment its bench finishes, not in a trailing
				// pass over the whole suite, so a run killed partway (job timeout,
				// runner eviction, panic) keeps every completed bench instead of
				// discarding the lot. The store is a per-bench time series, so a
				// partial run is well-formed. Non-fatal: a transient store error must
				// not sink an otherwise-good run.
				if cfg.save {
					if let Err(e) = store
						.add(BenchMarkRun {
							measurement: measurement.clone(),
							path: i.name(),
							backend: cfg.backend,
						})
						.await
					{
						eprintln!("Warning: could not store measurement for {}: {e:#}", i.name());
					}
				}
				measurements.push((i, measurement, compare));
			}
		}

		// The bench's datastore has dropped (run_bench returned), so reclaim the
		// file-backed store dir(s) before the next bench rather than letting them
		// accumulate until the whole tree is removed at the end of the run.
		if let Some(root) = config.bench_root.as_deref() {
			let _ = std::fs::remove_dir_all(root);
			let _ = std::fs::create_dir_all(root);
		}
	}

	for (i, m, compare) in &measurements {
		println!(" - {}", i.name());

		if let Some(compare) = compare {
			let signficant = compare.p_value < DEFAULT_SIGNIFICANCE_THRESHOLD;
			if !signficant {
				println!("       No change in performance detected")
			} else {
				let noise = DEFAULT_NOISE_THRESHOLD;
				if compare.dist_mean.lower_bound < -noise && compare.dist_mean.upper_bound < -noise
				{
					println!("       Performance has improved")
				} else if compare.dist_mean.lower_bound > noise
					&& compare.dist_mean.upper_bound > noise
				{
					println!("       Performance has regressed")
				} else {
					println!("       Performance difference within noise threshold")
				}
			}

			fn sign(negative: bool) -> &'static str {
				if negative {
					"-"
				} else {
					""
				}
			}

			let lb = Duration::from_secs_f64(compare.dist_mean.lower_bound.abs());
			let lb_sign = sign(compare.dist_mean.lower_bound.is_sign_negative());
			let ub = Duration::from_secs_f64(compare.dist_mean.upper_bound.abs());
			let ub_sign = sign(compare.dist_mean.upper_bound.is_sign_negative());
			let p = Duration::from_secs_f64(compare.dist_mean.point.abs());
			let p_sign = sign(compare.dist_mean.point.is_sign_negative());

			println!(
				" {:>24} : [{}{:?} {}{:?} {}{:?}] (p = {:.2} {} {:.2})",
				"change",
				lb_sign,
				lb,
				p_sign,
				p,
				ub_sign,
				ub,
				compare.p_value,
				if signficant {
					"<"
				} else {
					">"
				},
				DEFAULT_SIGNIFICANCE_THRESHOLD
			);
		};

		println!(
			" {:>24} : [{:?} {:?} {:?}]",
			"time",
			Duration::from_secs_f64(m.mean.lower_bound),
			Duration::from_secs_f64(m.mean.point),
			Duration::from_secs_f64(m.mean.upper_bound),
		);
		println!(
			" {:>24} : [{:?} {:?} {:?}]",
			"median",
			Duration::from_secs_f64(m.median.lower_bound),
			Duration::from_secs_f64(m.median.point),
			Duration::from_secs_f64(m.median.upper_bound),
		);
		println!(
			" {:>24} : [{:?} {:?} {:?}]",
			"std dev",
			Duration::from_secs_f64(m.std_dev.lower_bound),
			Duration::from_secs_f64(m.std_dev.point),
			Duration::from_secs_f64(m.std_dev.upper_bound),
		);
		println!(
			" {:>24} : [{:?} {:?} {:?}]",
			"mad",
			Duration::from_secs_f64(m.abs_dev.lower_bound),
			Duration::from_secs_f64(m.abs_dev.point),
			Duration::from_secs_f64(m.abs_dev.upper_bound),
		);
		let outliers = m.labels.iter().filter(|x| x.is_outlier()).count();
		if outliers != 0 {
			println!(
				"   Found {} outliers, among {} measurements ({:.2}%)",
				outliers,
				m.labels.len(),
				((outliers as f64 / m.labels.len() as f64) * 100.0).round()
			);

			println!(
				"    Low severe  {}",
				m.labels.iter().filter(|x| x.is_low() && x.is_severe()).count()
			);
			println!(
				"    Low mild    {}",
				m.labels.iter().filter(|x| x.is_low() && !x.is_severe()).count()
			);
			println!(
				"    High mild   {}",
				m.labels.iter().filter(|x| x.is_high() && !x.is_severe()).count()
			);
			println!(
				"    High severe {}",
				m.labels.iter().filter(|x| x.is_high() && x.is_severe()).count()
			);
		}
	}

	for e in load_errors.iter() {
		e.display(color);
	}

	store.close().await.context("Failed to close benchmark store")?;

	if !load_errors.is_empty() {
		bail!("Could not load all tests")
	}

	Ok(())
}

pub fn builder_from_config(config: &TestConfig) -> Builder {
	let capabilities = match &config.env.capabilities {
		BoolOr::Bool(true) => Capabilities::all().with_experimental(Targets::All),
		BoolOr::Bool(false) => Capabilities::none(),
		BoolOr::Value(x) => util::core_capabilities_from_test_config(x),
	};

	let builder = Datastore::builder();
	let builder = if capabilities.allows_live_query_notifications() {
		let (send, _) = channel::bounded(15_000);
		builder.with_notify(send)
	} else {
		builder
	};
	builder.with_capabilities(capabilities)
}

#[allow(clippy::large_enum_variant)]
enum BenchRunResult {
	Import(ImportFailure),
	Ok(MeasurementData, Option<ComparisonData>),
	/// Too few samples were collected to compute statistics — a bench whose single
	/// iteration exceeds `max_time` collects only one sample, and the stats need at
	/// least two. Reported and skipped instead of panicking.
	InsufficientSamples(usize),
}

/// The executable form of a bench case: raw SurrealQL source, a lowered OpenGQL
/// plan, or a GraphQL request executed against the schema generated for the
/// prepared datastore.
enum BenchStatement {
	SurrealQl,
	/// A parsed-and-lowered OpenGQL plan. Parse + lowering happen once in
	/// `prepare`, so the timed iterations measure plan execution only (mirroring
	/// the GraphQL arm, which generates its schema up front). The plan is cloned
	/// per iteration because `process_opengql` consumes it by value.
	OpenGql {
		plan: surrealdb_core::opengql::PreparedGqlQuery,
	},
	GraphQl {
		schema: async_graphql::dynamic::Schema,
		/// The case source with the config comment blanked out, computed once.
		source: String,
		session: Arc<Session>,
		variables: async_graphql::Variables,
		operation: Option<String>,
	},
}

impl BenchStatement {
	/// Builds the statement for a freshly prepared datastore. For GraphQL this
	/// generates the schema and request parts up front — mirroring the
	/// server's schema cache — so the timed iterations measure query execution
	/// only.
	async fn prepare(
		run: &TestRun<BenchRunConfig>,
		dbs: &Arc<Datastore>,
		session: &Session,
	) -> Result<Self> {
		match run.case.test.dialect {
			Dialect::SurrealQl => Ok(Self::SurrealQl),
			Dialect::OpenGql => {
				// Parse + lower the `.gql` source once, exactly as the run path
				// does (see `cmd::run::run_test_with_dbs`), so the timed loop
				// measures only `process_opengql`.
				let settings = surrealdb_core::opengql::GqlParserSettings::default();
				let source = run.case.test.source.as_bytes();
				let plan = surrealdb_core::opengql::parse_to_plan_with_settings(
					&run.case.test.source,
					settings,
				)
				.map_err(|e| {
					anyhow!(
						"Failed to parse/lower OpenGQL bench statement: {}",
						e.render_on_bytes(source)
					)
				})?;
				Ok(Self::OpenGql {
					plan,
				})
			}
			Dialect::GraphQl => {
				let schema = graphql::generate_schema(dbs, session)
					.await
					.map_err(|e| anyhow!("Failed to generate GraphQL schema: {e}"))?;
				Ok(Self::GraphQl {
					schema,
					source: graphql::case_source(&run.case.test),
					session: Arc::new(session.clone()),
					variables: graphql::request_variables(&run.case.test)?,
					operation: run.case.test.config.parsed.graphql.operation.clone(),
				})
			}
		}
	}

	/// Executes the bench statement once. GraphQL responses carry their
	/// errors in-band, so they are checked here — a bench that errors every
	/// iteration would otherwise silently measure nothing.
	async fn execute(
		&self,
		run: &TestRun<BenchRunConfig>,
		dbs: &Arc<Datastore>,
		session: &Session,
	) -> Result<()> {
		match self {
			Self::SurrealQl => {
				let _ = dbs.execute(&run.case.test.source, session, None).await?;
			}
			Self::OpenGql {
				plan,
			} => {
				// `process_opengql` takes the plan by value, so each iteration
				// executes a fresh clone of the once-lowered plan.
				let _ = dbs.process_opengql(plan.clone(), session, None).await?;
			}
			Self::GraphQl {
				schema,
				source,
				session,
				variables,
				operation,
			} => {
				let mut request = async_graphql::Request::new(source.clone())
					.data(Arc::clone(dbs))
					.data(Arc::clone(session));
				request.variables = variables.clone();
				if let Some(operation) = operation {
					request = request.operation_name(operation);
				}
				let response = schema.execute(request).await;
				if let Some(error) = response.errors.first() {
					bail!("GraphQL bench statement returned an error: {}", error.message);
				}
			}
		}
		Ok(())
	}
}

/// Seed used to make benchmark datasets deterministic. Each dataset build
/// reseeds the engine RNG (see `surrealdb_core::rnd`) so `rand::*` values and
/// `|record:N|` ids are identical across runs and independent of benchmark
/// order. Override with `SURREAL_RAND_SEED` to sanity-check results against a
/// different, but still fixed, dataset draw; the variable is parsed once by
/// `surrealdb_core::cnf::RAND_SEED` (which warns on a malformed value).
fn dataset_seed() -> u64 {
	const DEFAULT_DATASET_SEED: u64 = 0x5EED_B0A7;
	surrealdb_core::cnf::RAND_SEED.unwrap_or(DEFAULT_DATASET_SEED)
}

/// Builds a fresh in-memory datastore, runs the bench's imports, and performs
/// index compaction so the datastore is ready to execute the timed statement.
///
/// Returns `Ok(Err(..))` when an import fails so the caller can surface it as an
/// [`BenchRunResult::Import`].
async fn prepare(
	run: &TestRun<BenchRunConfig>,
	config: &BenchConfig,
	token: &tokio_util::sync::CancellationToken,
) -> Result<std::result::Result<(Arc<Datastore>, Session), ImportFailure>> {
	let dbs = Arc::new(
		builder_from_config(&run.case.test.config.parsed)
			.build_with_path(&datastore_conn(config))
			.await?,
	);

	let session =
		util::session_from_test_config(&run.case.test.config.parsed, config.new_planner.into());

	// Reseed the engine RNG before populating the dataset so generated data is
	// identical on every build and independent of benchmark order, making
	// nightly run-to-run comparisons reflect code changes rather than the luck
	// of the dataset draw.
	surrealdb_core::rnd::reseed(dataset_seed());

	// Use the per-variant dataset import chain (from `[bench].datasets`) when one
	// was selected, otherwise fall back to the bench's own resolved imports.
	let imports = run.config.imports.as_deref().unwrap_or(&run.case.imports);
	if let Some(e) = util::run_imports_list(imports, session.clone(), &dbs).await? {
		return Ok(Err(e));
	}

	Datastore::index_compaction(dbs.clone(), Duration::from_secs(1), token.clone()).await?;

	Ok(Ok((dbs, session)))
}

/// Emits a sentinel line on stderr delimiting the measured region of a bench,
/// so an external profiler can attach for only the timed statements and skip the
/// dataset import + warmup (see `scripts/bench/profile.sh`). No-op unless the
/// `BENCH_MARKERS` env var is set, to keep normal `bench run` output clean.
fn bench_marker(name: &str) {
	if std::env::var_os("BENCH_MARKERS").is_some() {
		eprintln!("{name}");
	}
}

async fn run_bench(
	run: &TestRun<BenchRunConfig>,
	config: &BenchConfig,
	baseline: Option<MeasurementData>,
) -> Result<BenchRunResult> {
	println!("Running bench {}", run.name());
	println!("Warming up");

	let bench_config = &run.case.test.config.parsed.bench;

	let warmup_time = bench_config.warmup.0;
	let token = tokio_util::sync::CancellationToken::new();

	// When the bench is read-only (`rebuild = false`, the default) the datastore
	// is built and populated once, then reused across every warmup and measured
	// iteration so we time only the statement against a stable dataset. Mutating
	// benches (`rebuild = true`) instead get a fresh datastore per iteration.
	let shared = if bench_config.rebuild {
		None
	} else {
		match prepare(run, config, &token).await? {
			Ok((dbs, session)) => {
				let statement = BenchStatement::prepare(run, &dbs, &session).await?;
				Some((dbs, session, statement))
			}
			Err(e) => return Ok(BenchRunResult::Import(e)),
		}
	};

	let before_warmup = Instant::now();
	let mut count = 0usize;
	loop {
		if let Some((dbs, session, statement)) = shared.as_ref() {
			statement.execute(run, dbs, session).await?;
		} else {
			let (dbs, session) = match prepare(run, config, &token).await? {
				Ok(prepared) => prepared,
				Err(e) => return Ok(BenchRunResult::Import(e)),
			};
			let statement = BenchStatement::prepare(run, &dbs, &session).await?;
			statement.execute(run, &dbs, &session).await?;
		}

		count += 1;

		if before_warmup.elapsed() > warmup_time {
			break;
		}
	}
	let measured_warmup_time = before_warmup.elapsed();

	let expected_iteration_time = measured_warmup_time.as_secs_f64() / count as f64;

	let iterations_per_samples = ((bench_config.measurement_time.0.as_secs_f64()
		/ expected_iteration_time
		/ bench_config.sample_size as f64)
		.ceil() as u64)
		.max(1);

	if iterations_per_samples == 1 {
		println!(
			"Could not complete {} samples in set measurement_time of {:?}",
			bench_config.sample_size, bench_config.measurement_time.0
		);
	}

	let estimate =
		expected_iteration_time * iterations_per_samples as f64 * bench_config.sample_size as f64;

	println!(
		"Completed {count} iterations in {warmup_time:?}, estimated execution time is {:?}",
		Duration::from_secs_f64(estimate)
	);

	// Mark the measured region so a profiler attaching mid-run (after the dataset
	// import + warmup) samples only the timed statements.
	bench_marker("__BENCH_MEASURE_START__");

	let max_time = bench_config.max_time.0;
	let measure_start = Instant::now();
	let mut iterations = Vec::new();
	let mut samples = Vec::new();
	for _ in 0..bench_config.sample_size {
		let mut sample_duration = 0.0;
		for _ in 0..iterations_per_samples {
			if let Some((dbs, session, statement)) = shared.as_ref() {
				let start = Instant::now();
				statement.execute(run, dbs, session).await?;
				sample_duration += start.elapsed().as_secs_f64();
			} else {
				let (dbs, session) = match prepare(run, config, &token).await? {
					Ok(prepared) => prepared,
					Err(e) => return Ok(BenchRunResult::Import(e)),
				};
				let statement = BenchStatement::prepare(run, &dbs, &session).await?;

				let start = Instant::now();
				statement.execute(run, &dbs, &session).await?;
				sample_duration += start.elapsed().as_secs_f64();
			}
		}
		iterations.push(iterations_per_samples as f64);
		samples.push(sample_duration);

		// Wall-clock backstop: a bench whose real per-iteration cost dwarfs the
		// warmup estimate would otherwise collect every sample no matter how long
		// it takes, so one mis-scoped bench can consume the whole run. Stop once we
		// pass the cap; the sample we're in always completes, so we keep at least one.
		if measure_start.elapsed() >= max_time {
			println!(
				"Exceeded max_time of {max_time:?}; stopping after {} of {} samples",
				samples.len(),
				bench_config.sample_size
			);
			break;
		}
	}

	bench_marker("__BENCH_MEASURE_END__");

	// A bench whose single iteration exceeds `max_time` collects only one sample,
	// which is too few for the statistics (they need at least two). Report it as
	// skipped rather than panicking inside `Sample::new`.
	let collected = samples.len();
	let Some(measurement) = MeasurementData::from_iteration_times(iterations, samples) else {
		return Ok(BenchRunResult::InsufficientSamples(collected));
	};
	let comp = baseline.map(|baseline| ComparisonData::compare(&baseline, &measurement));

	Ok(BenchRunResult::Ok(measurement, comp))
}
