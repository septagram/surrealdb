#[cfg(target_family = "wasm")]
use std::fs::File;
#[cfg(target_family = "wasm")]
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Result, bail, ensure};
#[cfg(not(target_family = "wasm"))]
use tokio::fs::File;
#[cfg(not(target_family = "wasm"))]
use tokio::io::{AsyncBufReadExt, BufReader};
use vart::VariableSizeKey;
use vart::art::Tree;

use crate::err::Error;
use crate::iam::file::check_is_path_allowed;
use crate::idx::ft::analyzer::filter::{FilterResult, Term};

#[derive(Clone, Default)]
pub(in crate::idx) struct Mapper {
	terms: Arc<Tree<VariableSizeKey, String>>,
}

impl Mapper {
	pub(in crate::idx) async fn new(path: &Path, allow_list: &[PathBuf]) -> Result<Self> {
		let mut terms = Tree::new();
		let path = check_is_path_allowed(path, allow_list)?;
		Self::iterate_file(&mut terms, &path).await?;
		Ok(Self {
			terms: Arc::new(terms),
		})
	}

	fn add_line_tree(
		terms: &mut Tree<VariableSizeKey, String>,
		line: &str,
		line_number: usize,
	) -> Result<()> {
		// Error messages must not echo the file content: the mapped file is
		// operator-controlled and embedding its bytes in the response would
		// leak file contents to the caller (see SECURITY_GUIDE.md).
		let Some((word, rest)) = line.split_once('\t') else {
			bail!(Error::AnalyzerError(format!(
				"Expected two terms separated by a tab on line {line_number}"
			)));
		};

		ensure!(
			!rest.contains('\t'),
			Error::AnalyzerError(format!(
				"Expected two terms to not contain more than one tab on line {line_number}"
			))
		);

		let key = VariableSizeKey::from_str(rest.trim()).map_err(|_| {
			Error::AnalyzerError(format!("Can't create key from term on line {line_number}"))
		})?;
		terms
			.insert_unchecked(&key, word.trim().to_string(), 0, 0)
			.map_err(|e| Error::AnalyzerError(e.to_string()))?;

		Ok(())
	}

	#[cfg(not(target_family = "wasm"))]
	async fn iterate_file(terms: &mut Tree<VariableSizeKey, String>, path: &Path) -> Result<()> {
		let file = File::open(path).await?;
		let reader = BufReader::new(file);
		let mut lines = reader.lines();
		let mut line_number = 0;
		while let Some(line) = lines.next_line().await? {
			yield_now!();
			Self::add_line_tree(terms, &line, line_number)?;
			line_number += 1;
		}
		Ok(())
	}

	#[cfg(target_family = "wasm")]
	async fn iterate_file(terms: &mut Tree<VariableSizeKey, String>, path: &Path) -> Result<()> {
		let file = File::open(path)?;
		let reader = BufReader::new(file);
		let mut line_number = 0;
		for line_result in reader.lines() {
			let line = line_result?;
			Self::add_line_tree(terms, &line, line_number)?;
			line_number += 1;
		}
		Ok(())
	}

	pub(super) fn map(&self, token: &str) -> FilterResult {
		if let Ok(key) = VariableSizeKey::from_str(token)
			&& let Some((lemme, _, _)) = self.terms.get(&key, 0)
		{
			return FilterResult::Term(Term::NewTerm(lemme, 0));
		}
		FilterResult::Term(Term::Unchanged)
	}
}

#[cfg(test)]
mod tests {
	use vart::VariableSizeKey;
	use vart::art::Tree;

	use super::Mapper;

	/// A malformed mapper line must not have its raw content echoed back in the
	/// error: doing so would leak the contents of the mapped file to the caller.
	#[test]
	fn test_parse_error_does_not_leak_file_content() {
		let secret = "root:x:0:0:super-secret-passwd-content:/root:/bin/sh";
		let mut terms: Tree<VariableSizeKey, String> = Tree::new();
		// A line without a tab separator triggers the parse error path.
		let err = Mapper::add_line_tree(&mut terms, secret, 0)
			.expect_err("a line without a tab must fail to parse");
		let msg = err.to_string();
		assert!(
			!msg.contains("super-secret-passwd-content"),
			"parser error leaked file content: {msg}"
		);

		// A line with too many tabs must also not echo its content.
		let secret_tabs = "a\tb\tsuper-secret-passwd-content";
		let err = Mapper::add_line_tree(&mut terms, secret_tabs, 1)
			.expect_err("a line with multiple tabs must fail to parse");
		let msg = err.to_string();
		assert!(
			!msg.contains("super-secret-passwd-content"),
			"parser error leaked file content: {msg}"
		);
	}
}
