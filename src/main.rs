mod metadata;
mod naming;
mod operations;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use naming::MissingPolicy;
use operations::{ConflictPolicy, Overrides, RenameOptions};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "pdfname",
    version,
    about = "Inspect and safely rename PDFs from publication metadata"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show metadata and provenance without modifying anything.
    Inspect {
        pdf: PathBuf,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Resolve identifiers such as DOI using online metadata services.
        #[arg(long, conflicts_with = "offline")]
        enrich: bool,
        /// Never make network requests (the default; useful for explicit scripts).
        #[arg(long, conflicts_with = "enrich")]
        offline: bool,
        /// OCR the first three pages locally when metadata remains incomplete.
        #[arg(long)]
        ocr: bool,
    },
    /// Preview renames (or execute with --apply). Accepts a PDF or directory.
    Rename {
        path: PathBuf,
        /// Actually rename files. Default is a safe preview.
        #[arg(long)]
        apply: bool,
        /// Include PDFs in nested directories.
        #[arg(long)]
        recursive: bool,
        /// How to handle missing publisher/year/authors.
        #[arg(long, value_enum, default_value_t = MissingArg::Skip)]
        missing: MissingArg,
        /// How to handle existing destination filenames.
        #[arg(long, value_enum, default_value_t = ConflictArg::Skip)]
        conflict: ConflictArg,
        /// Override title (single PDF only).
        #[arg(long)]
        title: Option<String>,
        /// Override publisher (single PDF only).
        #[arg(long)]
        publisher: Option<String>,
        /// Override publication year (single PDF only).
        #[arg(long)]
        year: Option<u16>,
        /// Override authors: repeat --author for multiple authors (single PDF only).
        #[arg(long = "author")]
        authors: Vec<String>,
        /// Resolve identifiers such as DOI using online metadata services.
        #[arg(long, conflicts_with = "offline")]
        enrich: bool,
        /// Never make network requests (the default; useful for explicit scripts).
        #[arg(long, conflicts_with = "enrich")]
        offline: bool,
        /// OCR the first three pages locally when metadata remains incomplete.
        #[arg(long)]
        ocr: bool,
    },
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, ValueEnum)]
enum MissingArg {
    Skip,
    Omit,
}
#[derive(Debug, Copy, Clone, Eq, PartialEq, ValueEnum)]
enum ConflictArg {
    Skip,
    Suffix,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Inspect {
            pdf,
            json,
            enrich,
            offline: _,
            ocr,
        } => {
            let meta = metadata::extract(&pdf, enrich, ocr)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&meta)?);
            } else {
                println!("File: {}", pdf.display());
                let show = |field: &Option<metadata::Field<String>>| {
                    field
                        .as_ref()
                        .map(|f| format!("{} [{}]", f.value, f.source))
                        .unwrap_or_else(|| "<missing>".into())
                };
                println!("Title:     {}", show(&meta.title));
                println!("Publisher: {}", show(&meta.publisher));
                println!(
                    "Year:      {}",
                    meta.year
                        .as_ref()
                        .map(|f| format!("{} [{}]", f.value, f.source))
                        .unwrap_or_else(|| "<missing>".into())
                );
                println!(
                    "Authors:   {}",
                    if meta.authors.is_empty() {
                        "<missing>".into()
                    } else {
                        format!(
                            "{} [{}]",
                            meta.authors.join(", "),
                            meta.authors_source.unwrap_or("unknown")
                        )
                    }
                );
                println!(
                    "DOI:       {}",
                    meta.doi
                        .as_ref()
                        .map(|f| format!("{} [{}]", f.value, f.source))
                        .unwrap_or_else(|| "<missing>".into())
                );
                for warning in &meta.warnings {
                    eprintln!("Warning: {warning}");
                }
            }
        }
        Command::Rename {
            path,
            apply,
            recursive,
            missing,
            conflict,
            title,
            publisher,
            year,
            authors,
            enrich,
            offline: _,
            ocr,
        } => {
            let options = RenameOptions {
                apply,
                recursive,
                missing: match missing {
                    MissingArg::Skip => MissingPolicy::Skip,
                    MissingArg::Omit => MissingPolicy::Omit,
                },
                conflict: match conflict {
                    ConflictArg::Skip => ConflictPolicy::Skip,
                    ConflictArg::Suffix => ConflictPolicy::Suffix,
                },
                overrides: Overrides {
                    title,
                    publisher,
                    year,
                    authors,
                },
                enrich,
                ocr,
            };
            operations::run_rename(&path, options)?;
        }
    }
    Ok(())
}
