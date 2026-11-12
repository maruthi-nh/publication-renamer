# pdfname — publication-aware PDF renamer

Rust CLI that reads PDF Info and embedded XMP metadata, then previews or performs non-overwriting filename changes.

**Target format:** `Title - Publisher - Year - Author One, Author Two.pdf`

## Build

Requires Rust 1.85+:

```bash
cargo build --release
cargo test
```

Binary: `target/release/pdfname` (`.exe` on Windows).

## Commands

```bash
# Discover metadata (including each field's source)
pdfname inspect ~/Downloads/example.pdf
pdfname inspect ~/Downloads/example.pdf --json

# Extract a DOI from the first three pages and resolve it through Crossref
pdfname inspect ~/Downloads/article.pdf --enrich

# Offline fallback; OCR image-only pages when necessary
pdfname inspect ~/Downloads/scanned-article.pdf --offline --ocr

# Preview; NO files are modified by default
pdfname rename ~/Downloads/example.pdf
pdfname rename ~/Documents/PDFs --recursive

# Enrich papers with blank embedded metadata before planning the rename
pdfname rename ~/Downloads/article.pdf --enrich

# OCR is still preview-first and never enables network access
pdfname rename ~/Downloads/scanned-article.pdf --offline --ocr

# Execute a previewed plan
pdfname rename ~/Documents/PDFs --recursive --apply

# Strict default requires title + publisher + publication year + authors.
# Allow partial filename if some fields are missing:
pdfname rename ~/Downloads/example.pdf --missing omit

# Correct incomplete or wrong metadata for a single PDF:
pdfname rename ~/Downloads/example.pdf \
  --title 'The Rust Programming Language' \
  --publisher 'No Starch Press' --year 2023 \
  --author 'Steve Klabnik' --author 'Carol Nichols' --apply

# Automatically suffix collisions with (2), (3), etc.
pdfname rename ~/Documents/PDFs --recursive --conflict suffix --apply
```

## Metadata precedence

- Without `--enrich`, operation is offline. The first three pages are still
  examined locally for a DOI, with an 8 MiB decompression limit per page.
- When embedded metadata is incomplete, layout-preserving first-page text is
  parsed conservatively for journal articles. This requires Poppler's
  `pdftotext` command.
- `--ocr` renders at most the first three pages with `pdftoppm` and processes
  them with Tesseract (`eng`). OCR runs only if metadata remains incomplete.
  Install the `poppler-utils` and `tesseract-ocr` system packages to use it.
- Locally inferred fields include their provenance and produce a warning. Always
  review the default dry run before combining `--ocr` with `--apply`.
- With `--enrich`, a detected DOI is resolved through Crossref. Crossref title,
  publisher, publication date, and authors replace incomplete embedded values.
- Crossref requests have short connection/read timeouts. Lookup failures become
  warnings and never prevent embedded metadata from being inspected.
- Title: XMP `dc:title`, then PDF Info `/Title`.
- Authors: XMP `dc:creator`, then PDF Info `/Author` (semicolon-delimited).
- Publisher: XMP `dc:publisher`, then custom PDF Info `/Publisher`.
- Publication year: XMP `prism:publicationDate`, XMP `dcterms:issued`, XMP `dc:date` (ambiguous, warning), then PDF Info custom `/PublicationYear` or `/Year`.
- `PDF /CreationDate`, XMP `xmp:CreateDate`, PDF `/Producer` are NEVER treated as publication year or publisher.
- Explicit command-line overrides win.

PDF Info, XMP, and metadata embedded by publishers are often incomplete or
inaccurate. DOI/Crossref enrichment is opt-in. Offline first-page parsing is
deliberately conservative and currently targets common journal layouts and
publishers. ISBN resolution and broader cross-validation are not yet included.

## Safety

- Default mode is dry-run (preview).
- Existing destination PDFs are never replaced.
- Files with missing required metadata are skipped unless `--missing omit`.
- No writes to PDF bytes: only filesystem directory entries change.
- Filename punctuation is sanitized; filename length is bounded.
- Symbolic links are not followed and file overrides do not apply to whole directories.
- Rename implementation uses a same-directory hard-link-then-unlink strategy. That strategy prevents destination overwrites and preserves file bytes, but is not an atomic single-operation rename. If original deletion fails, both names can remain. Filesystems without hard-link support will reject the operation safely. A production tool may eventually use OS-specific no-replace rename APIs.
- On case-insensitive filesystems, case-only renames are intentionally not supported using this mechanism.
- On untrusted PDFs, each compressed stream has a 32 MiB expansion limit and embedded XMP a 2 MiB size limit. Whole-file memory usage is not bounded; do not treat this program as a hostile-input sandbox.
- XMP `dc:date` is not guaranteed to mean publication year; inspect its warning before applying.
- Directory rename loops process one PDF at a time and report errors without halting other files.

## Roadmap

1. Filename template support, configuration (`~/.config/pdfname/config.toml`), Unicode normalization.
2. Broader first-page layouts and interactive review of ambiguous candidates.
3. ISBN identification; OpenAlex / Open Library adapters; source confidence and validation.
4. Optional persistent cache, undo manifest, planned batches, safe Windows/macOS/Linux no-replace moves, and JSON batch plans for agent integration.

## Note on builds

This source archive was prepared in an environment without `cargo` or `rustc`; it has not been compilation-tested here. Run `cargo test` on your machine before using `--apply` on important files.
