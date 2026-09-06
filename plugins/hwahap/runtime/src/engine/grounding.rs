//! Check cited repository locations, not whether a fact's prose follows from the cited text.

use std::path::Path;

use crate::error::{Error, Result};
use crate::git::Git;
use crate::plan::Plan;

/// Every citation must name lines of a regular text file in the commit observed on entry.
/// The caller owns PLAN eligibility, source-head binding and working-tree cleanliness checks.
pub(super) fn verify_sources(plan: &Plan, cwd: &Path) -> Result<()> {
    let git = Git::open(cwd)?;
    let head = git.head_sha()?;
    for fact in &plan.facts {
        if fact.sources.is_empty() {
            return Err(Error::Rejected(format!(
                "{} has no source locations",
                fact.id
            )));
        }
        for source in &fact.sources {
            verify_source(&git, &head, source).map_err(|error| {
                Error::Rejected(format!("{} source {source:?}: {error}", fact.id))
            })?;
        }
    }
    if git.head_sha()? != head {
        return Err(Error::BoundaryViolation(
            "HEAD changed while verifying fact sources".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;

fn verify_source(git: &Git, head: &str, source: &str) -> Result<()> {
    let reject = |detail: &str| Error::Rejected(detail.into());
    let (path, range) = source
        .rsplit_once(':')
        .ok_or_else(|| reject("expected path:line or path:start-end"))?;
    if path.is_empty()
        || path.contains(['\\', ':'])
        || path.chars().any(char::is_control)
        || path.split('/').any(|part| {
            part.is_empty() || part == "." || part == ".." || part.eq_ignore_ascii_case(".git")
        })
    {
        return Err(reject(
            "source path must be a safe repository-relative file path",
        ));
    }
    let positive = |text: &str| -> Result<usize> {
        if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
            return Err(reject("line numbers must be positive integers"));
        }
        text.parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| reject("line number is zero or out of range"))
    };
    let (first, last) = range.split_once('-').unwrap_or((range, range));
    let (first, last) = (positive(first)?, positive(last)?);
    if first > last {
        return Err(reject("line range is reversed"));
    }
    let listing = git.stdout_of(
        git.root(),
        &[
            "--literal-pathspecs",
            "ls-tree",
            "-z",
            "--full-tree",
            head,
            "--",
            path,
        ],
    )?;
    let entry = listing
        .strip_suffix(&[0])
        .ok_or_else(|| reject("file is not tracked in the current commit"))?;
    let separator = entry
        .iter()
        .position(|byte| *byte == b'\t')
        .ok_or_else(|| reject("invalid tree entry"))?;
    let (metadata, name) = (&entry[..separator], &entry[separator + 1..]);
    if name != path.as_bytes()
        || !(metadata.starts_with(b"100644 blob ") || metadata.starts_with(b"100755 blob "))
    {
        return Err(reject(
            "citation must name a regular tracked file, not a directory, symlink or submodule",
        ));
    }
    let object = format!("{head}:{path}");
    let size = git
        .run(&["cat-file", "-s", &object])?
        .parse::<usize>()
        .map_err(|_| reject("invalid blob size"))?;
    if size > 4 * 1024 * 1024 {
        return Err(reject("cited file exceeds the 4 MiB verification limit"));
    }
    // Raw bytes preserve leading/trailing blank lines; Git::run trims them and shifts line bounds.
    let bytes = git.stdout_of(
        git.root(),
        &["show", "--no-ext-diff", "--no-textconv", &object],
    )?;
    let text = std::str::from_utf8(&bytes).map_err(|_| reject("cited file is not UTF-8 text"))?;
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(reject("cited file contains binary control bytes"));
    }
    let lines = text.lines().count();
    if last > lines {
        return Err(reject(&format!(
            "line {last} exceeds the committed file's {lines} lines"
        )));
    }
    Ok(())
}
