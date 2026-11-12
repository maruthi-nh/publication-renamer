use crate::metadata::{extract, BookMetadata, Field};
use crate::naming::{target_filename, MissingPolicy};
use anyhow::{bail, Context, Result};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

#[derive(Debug, Default)]
pub struct Overrides {
    pub title: Option<String>,
    pub publisher: Option<String>,
    pub year: Option<u16>,
    pub authors: Vec<String>,
}

impl Overrides {
    pub fn has_any(&self) -> bool {
        self.title.is_some()
            || self.publisher.is_some()
            || self.year.is_some()
            || !self.authors.is_empty()
    }

    fn apply(&self, meta: &mut BookMetadata) {
        if let Some(title) = &self.title {
            meta.title = Some(Field {
                value: title.clone(),
                source: "cli-override",
            });
        }
        if let Some(publisher) = &self.publisher {
            meta.publisher = Some(Field {
                value: publisher.clone(),
                source: "cli-override",
            });
        }
        if let Some(year) = self.year {
            meta.year = Some(Field {
                value: year,
                source: "cli-override",
            });
        }
        if !self.authors.is_empty() {
            meta.authors = self.authors.clone();
            meta.authors_source = Some("cli-override");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    Skip,
    Suffix,
}

pub struct RenameOptions {
    pub apply: bool,
    pub recursive: bool,
    pub missing: MissingPolicy,
    pub conflict: ConflictPolicy,
    pub overrides: Overrides,
    pub enrich: bool,
}

fn is_pdf(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("pdf"))
}

fn collect_pdfs(input: &Path, recursive: bool) -> Result<Vec<PathBuf>> {
    let meta = fs::symlink_metadata(input)
        .with_context(|| format!("cannot inspect {}", input.display()))?;
    if meta.file_type().is_symlink() {
        bail!("refusing symbolic link: {}", input.display());
    }
    if meta.is_file() {
        if !is_pdf(input) {
            bail!("not a .pdf file: {}", input.display());
        }
        return Ok(vec![input.to_path_buf()]);
    }
    if !meta.is_dir() {
        bail!("not a regular file or directory: {}", input.display());
    }

    let walker = WalkDir::new(input).follow_links(false).sort_by_file_name();
    let walker = if recursive {
        walker
    } else {
        walker.max_depth(1)
    };
    let mut files = Vec::new();
    for entry in walker {
        let entry = entry?;
        if entry.file_type().is_file() && is_pdf(entry.path()) {
            files.push(entry.into_path());
        }
    }
    Ok(files)
}

fn casefold_path(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

fn find_destination(
    source: &Path,
    desired: &Path,
    policy: ConflictPolicy,
    reserved: &HashSet<String>,
) -> Option<PathBuf> {
    if source == desired {
        return Some(desired.to_path_buf());
    }
    if !desired.exists() && !reserved.contains(&casefold_path(desired)) {
        return Some(desired.to_path_buf());
    }
    if policy == ConflictPolicy::Skip {
        return None;
    }
    let parent = desired.parent()?;
    let stem = desired.file_stem()?.to_str()?;
    for index in 2..=9999 {
        let file = format!("{stem} ({index}).pdf");
        // Avoid overlong paths if the suffix pushes the component past 255 bytes.
        if file.len() > 250 {
            return None;
        }
        let candidate = parent.join(file);
        if source == candidate {
            return Some(candidate);
        }
        if !candidate.exists() && !reserved.contains(&casefold_path(&candidate)) {
            return Some(candidate);
        }
    }
    None
}

fn safe_move_without_overwrite(source: &Path, destination: &Path) -> Result<()> {
    // Same-directory hard link creates a new name atomically only if it is vacant.
    // This avoids fs::rename silently replacing an existing destination on Unix.
    fs::hard_link(source, destination).with_context(|| {
        format!(
            "cannot claim destination {} (existing file or no hard-link support)",
            destination.display()
        )
    })?;
    if let Err(error) = fs::remove_file(source) {
        // Keep the destination link as well: both names still point to intact bytes.
        bail!(
            "destination was created but original could not be removed: {error}; both names exist"
        );
    }
    Ok(())
}

pub fn run_rename(input: &Path, options: RenameOptions) -> Result<()> {
    if options.overrides.has_any() && fs::metadata(input)?.is_dir() {
        bail!("metadata overrides (--title/--publisher/--year/--author) require a single PDF");
    }
    let sources = collect_pdfs(input, options.recursive)?;
    if sources.is_empty() {
        println!("No PDFs found under {}", input.display());
        return Ok(());
    }

    // Reserve target names before applying any changes to avoid intra-batch conflicts.
    let mut reserved = HashSet::new();
    let mut renamed = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;
    for source in sources {
        let mut meta = match extract(&source, options.enrich) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("ERROR {}: {e:#}", source.display());
                errors += 1;
                continue;
            }
        };
        options.overrides.apply(&mut meta);
        let filename = match target_filename(&meta, options.missing) {
            Ok(s) => s,
            Err(reason) => {
                println!("SKIP  {}: {reason}", source.display());
                skipped += 1;
                continue;
            }
        };
        let desired = source.with_file_name(filename);
        let Some(destination) = find_destination(&source, &desired, options.conflict, &reserved)
        else {
            println!(
                "SKIP  {}: destination exists or was reserved",
                source.display()
            );
            skipped += 1;
            continue;
        };
        if source == destination {
            println!("SAME  {}", source.display());
            skipped += 1;
            continue;
        }
        // A destination conflict can also arise between files while processing a batch.
        reserved.insert(casefold_path(&destination));
        if !meta.warnings.is_empty() {
            for warning in &meta.warnings {
                eprintln!("WARN  {}: {warning}", source.display());
            }
        }
        if options.apply {
            match safe_move_without_overwrite(&source, &destination) {
                Ok(()) => {
                    println!("DONE  {} -> {}", source.display(), destination.display());
                    renamed += 1;
                }
                Err(e) => {
                    eprintln!("ERROR {}: {e:#}", source.display());
                    errors += 1;
                }
            }
        } else {
            println!("PLAN  {} -> {}", source.display(), destination.display());
            renamed += 1;
        }
    }
    println!(
        "Summary: {} {}, {} skipped, {} errors",
        renamed,
        if options.apply { "renamed" } else { "planned" },
        skipped,
        errors
    );
    if errors > 0 {
        bail!("{errors} PDF(s) could not be processed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn safe_move_does_not_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.pdf");
        let dest = dir.path().join("b.pdf");
        fs::write(&source, b"original").unwrap();
        fs::write(&dest, b"do-not-overwrite").unwrap();
        assert!(safe_move_without_overwrite(&source, &dest).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"original");
        assert_eq!(fs::read(&dest).unwrap(), b"do-not-overwrite");
    }
    #[test]
    fn safe_move_preserves_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.pdf");
        let dest = dir.path().join("b.pdf");
        fs::write(&source, b"pdf-bytes").unwrap();
        safe_move_without_overwrite(&source, &dest).unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read(&dest).unwrap(), b"pdf-bytes");
    }
}
