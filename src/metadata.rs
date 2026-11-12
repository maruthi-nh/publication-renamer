use anyhow::{Context, Result};
use lopdf::{Document, LoadOptions, Object};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{fs, path::Path, process::Command};

const DC: &str = "http://purl.org/dc/elements/1.1/";
const DCTERMS: &str = "http://purl.org/dc/terms/";
const PRISM: &str = "http://prismstandard.org/namespaces/basic/2.0/";
const XMP_LIMIT: usize = 2 * 1024 * 1024;
const STREAM_LIMIT: usize = 32 * 1024 * 1024;
const PAGE_TEXT_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Field<T> {
    pub value: T,
    pub source: &'static str,
}

impl<T> Field<T> {
    fn new(value: T, source: &'static str) -> Self {
        Self { value, source }
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct BookMetadata {
    pub title: Option<Field<String>>,
    pub publisher: Option<Field<String>>,
    pub year: Option<Field<u16>>,
    pub authors: Vec<String>,
    pub authors_source: Option<&'static str>,
    pub doi: Option<Field<String>>,
    pub warnings: Vec<String>,
}

fn normalize_doi(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("https://doi.org/")
        .trim_start_matches("http://doi.org/")
        .trim_start_matches("doi:")
        .trim_end_matches(['.', ',', ';', ')', ']', '}'])
        .to_ascii_lowercase()
}

/// Extract a DOI conservatively. The prefix and registrant are numeric and the
/// suffix is stopped at whitespace or characters that cannot occur in a DOI.
fn find_doi(text: &str) -> Option<String> {
    let re = Regex::new(
        r"(?i)(?:https?://(?:dx\.)?doi\.org/|doi\s*:\s*)?(10\.\d{4,9}/[-._;()/:a-z0-9]+)",
    )
    .ok()?;
    re.captures(text)
        .and_then(|c| c.get(1))
        .map(|m| normalize_doi(m.as_str()))
        .filter(|s| !s.is_empty())
}

fn extract_first_pages(pdf: &Document, count: usize) -> Result<String> {
    let pages = pdf.get_pages();
    let numbers = pages.keys().copied().take(count).collect::<Vec<_>>();
    if numbers.is_empty() {
        return Ok(String::new());
    }
    pdf.extract_text_with_limit(&numbers, PAGE_TEXT_LIMIT)
        .context("cannot extract text from the first pages")
}

fn layout_text_first_pages(path: &Path, count: usize) -> Result<String> {
    let output = Command::new("pdftotext")
        .args(["-f", "1", "-l", &count.to_string(), "-layout"])
        .arg(path)
        .arg("-")
        .output()
        .context("cannot run pdftotext; install Poppler for offline first-page inference")?;
    if !output.status.success() {
        anyhow::bail!(
            "pdftotext failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    if output.stdout.len() > 2 * 1024 * 1024 {
        anyhow::bail!("first-page text exceeded the 2 MiB safety limit");
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn ocr_first_pages(path: &Path, count: usize) -> Result<String> {
    let temp = tempfile::tempdir().context("cannot create OCR temporary directory")?;
    let prefix = temp.path().join("page");
    let rendered = Command::new("pdftoppm")
        .args(["-f", "1", "-l", &count.to_string(), "-r", "200", "-png"])
        .arg(path)
        .arg(&prefix)
        .output()
        .context("cannot run pdftoppm; install Poppler to use --ocr")?;
    if !rendered.status.success() {
        anyhow::bail!(
            "pdftoppm failed: {}",
            String::from_utf8_lossy(&rendered.stderr).trim()
        );
    }

    let mut images = fs::read_dir(temp.path())?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "png"))
        .collect::<Vec<_>>();
    images.sort();
    let mut text = String::new();
    for image in images {
        let output = Command::new("tesseract")
            .arg(&image)
            .arg("stdout")
            .arg("-l")
            .arg("eng")
            .output()
            .context("cannot run tesseract; install Tesseract OCR to use --ocr")?;
        if !output.status.success() {
            anyhow::bail!(
                "tesseract failed for {}: {}",
                image.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        if text.len() + output.stdout.len() > 2 * 1024 * 1024 {
            anyhow::bail!("OCR output exceeded the 2 MiB safety limit");
        }
        text.push_str(&String::from_utf8_lossy(&output.stdout));
        text.push('\n');
    }
    if text.trim().is_empty() {
        anyhow::bail!("OCR produced no text");
    }
    Ok(text)
}

fn looks_like_author_line(line: &str) -> bool {
    if !line.contains(',') || line.len() > 350 {
        return false;
    }
    authors_from_line(line).len() >= 2
}

fn is_title_candidate(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    line.len() >= 12
        && line.len() <= 300
        && line.chars().any(char::is_alphabetic)
        && !lower.contains("journal homepage")
        && !lower.contains("contents lists available")
        && !lower.starts_with("http")
        && !lower.starts_with("doi:")
}

fn authors_from_line(line: &str) -> Vec<String> {
    let author = Regex::new(
        r"(?x)
        (?:[A-Z]\.?(?:\s*)){1,4}[A-Z][\p{L}'’-]{2,}(?:\s+[A-Z][\p{L}'’-]{2,}){0,2}
        |
        [A-Z][\p{L}'’-]{2,}(?:\s+[A-Z][\p{L}'’-]{2,}){1,2}",
    )
    .expect("valid regex");
    author
        .find_iter(line)
        .filter_map(|value| nonempty(value.as_str()))
        .collect()
}

/// Conservative journal-front-page fallback. It only fills absent fields and
/// marks every value as inferred so users can inspect the dry run before apply.
fn merge_first_page_text(meta: &mut BookMetadata, text: &str, source: &'static str) {
    if meta.doi.is_none() {
        meta.doi = find_doi(text).map(|doi| Field::new(doi, source));
    }
    let lines = text.lines().filter_map(nonempty).collect::<Vec<_>>();
    let author_index = lines.iter().position(|line| looks_like_author_line(line));
    if let Some(index) = author_index {
        if meta.title.is_none() {
            if let Some(title) = lines[..index]
                .iter()
                .rev()
                .find(|line| is_title_candidate(line))
            {
                meta.title = Some(Field::new(title.clone(), source));
            }
        }
        if meta.authors.is_empty() {
            let authors = authors_from_line(&lines[index]);
            if authors.len() >= 2 {
                meta.authors = authors;
                meta.authors_source = Some(source);
            }
        }
    }
    if meta.year.is_none() {
        let year_re = Regex::new(r"(?:^|[^0-9])((?:19|20)\d{2})(?:[^0-9]|$)").expect("valid regex");
        meta.year = lines.iter().take(25).find_map(|line| {
            year_re
                .captures(line)?
                .get(1)?
                .as_str()
                .parse()
                .ok()
                .map(|year| Field::new(year, source))
        });
    }
    if meta.publisher.is_none() {
        let publisher_re = Regex::new(
            r"(?i)\b(Elsevier(?:\s+BV)?|Springer(?: Nature)?|Wiley|IEEE|Taylor & Francis|SAGE|Oxford University Press|Cambridge University Press|ACM)\b",
        ).expect("valid regex");
        meta.publisher = lines.iter().find_map(|line| {
            let value = publisher_re.captures(line)?.get(1)?.as_str();
            let publisher = match value.to_ascii_lowercase().as_str() {
                value if value.starts_with("elsevier") => "Elsevier".to_string(),
                value if value.starts_with("springer") => "Springer Nature".to_string(),
                "wiley" => "Wiley".to_string(),
                "ieee" => "IEEE".to_string(),
                "sage" => "SAGE".to_string(),
                "acm" => "ACM".to_string(),
                _ => value.to_string(),
            };
            Some(Field::new(publisher, source))
        });
    }
}

fn metadata_incomplete(meta: &BookMetadata) -> bool {
    meta.title.is_none()
        || meta.publisher.is_none()
        || meta.year.is_none()
        || meta.authors.is_empty()
}

#[derive(Debug, Deserialize)]
struct CrossrefEnvelope {
    message: CrossrefWork,
}

#[derive(Debug, Deserialize)]
struct CrossrefWork {
    #[serde(default)]
    title: Vec<String>,
    publisher: Option<String>,
    #[serde(default)]
    author: Vec<CrossrefAuthor>,
    published: Option<CrossrefDate>,
    #[serde(rename = "published-print")]
    published_print: Option<CrossrefDate>,
    #[serde(rename = "published-online")]
    published_online: Option<CrossrefDate>,
}

#[derive(Debug, Deserialize)]
struct CrossrefAuthor {
    given: Option<String>,
    family: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CrossrefDate {
    #[serde(rename = "date-parts", default)]
    date_parts: Vec<Vec<u16>>,
}

fn crossref_year(work: &CrossrefWork) -> Option<u16> {
    [
        &work.published,
        &work.published_print,
        &work.published_online,
    ]
    .into_iter()
    .flatten()
    .find_map(|date| date.date_parts.first()?.first().copied())
}

fn merge_crossref(meta: &mut BookMetadata, work: CrossrefWork) {
    let year = crossref_year(&work);
    if let Some(title) = work.title.into_iter().find_map(|s| nonempty(&s)) {
        meta.title = Some(Field::new(title, "crossref:title"));
    }
    if let Some(publisher) = work.publisher.and_then(|s| nonempty(&s)) {
        meta.publisher = Some(Field::new(publisher, "crossref:publisher"));
    }
    let authors = work
        .author
        .into_iter()
        .filter_map(|author| {
            author.name.and_then(|s| nonempty(&s)).or_else(|| {
                let name = [author.given.as_deref(), author.family.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" ");
                nonempty(&name)
            })
        })
        .collect::<Vec<_>>();
    if !authors.is_empty() {
        meta.authors = authors;
        meta.authors_source = Some("crossref:author");
    }
    if let Some(year) = year {
        meta.year = Some(Field::new(year, "crossref:published"));
    }
}

fn encode_path_segment(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn enrich_crossref(meta: &mut BookMetadata) -> Result<()> {
    let doi = meta.doi.as_ref().context("no DOI was found")?.value.clone();
    let url = format!(
        "https://api.crossref.org/works/{}",
        encode_path_segment(&doi)
    );
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(5))
        .timeout_read(std::time::Duration::from_secs(10))
        .user_agent(concat!(
            "pdfname/",
            env!("CARGO_PKG_VERSION"),
            " (mailto:unknown@example.invalid)"
        ))
        .build();
    let envelope: CrossrefEnvelope = agent
        .get(&url)
        .call()
        .context("Crossref request failed")?
        .into_json()
        .context("Crossref returned invalid JSON")?;
    merge_crossref(meta, envelope.message);
    Ok(())
}

fn nonempty(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty()
        || ["untitled", "unknown", "none", "null"].contains(&trimmed.to_ascii_lowercase().as_str())
    {
        None
    } else {
        Some(trimmed.to_string())
    }
}

// Use lopdf's PDFDocEncoding / UTF-16BE / UTF-8 aware decoder.
fn pdf_string(doc: &Document, obj: &Object) -> Option<String> {
    let resolved = doc.dereference(obj).ok()?.1;
    lopdf::decode_text_string(resolved)
        .ok()
        .and_then(|s| nonempty(&s))
}

fn info_field(doc: &Document, dict: &lopdf::Dictionary, key: &[u8]) -> Option<String> {
    pdf_string(doc, dict.get(key).ok()?)
}

fn publication_year(input: &str) -> Option<u16> {
    // Only a leading ISO-style year, not an arbitrary four-digit number from a title.
    let s = input.trim();
    let year = s.get(..4)?;
    if !year.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if let Some(next) = s.as_bytes().get(4) {
        if !matches!(next, b'-' | b'/' | b' ' | b'T') {
            return None;
        }
    }
    let value = year.parse::<u16>().ok()?;
    (1500..=2200).contains(&value).then_some(value)
}

fn split_info_authors(value: &str) -> Vec<String> {
    // Commas can mean "Last, First"; do not split on them automatically.
    value.split(';').filter_map(nonempty).collect()
}

fn parse_info(pdf: &Document, meta: &mut BookMetadata) {
    let Ok(info_ref) = pdf.trailer.get(b"Info") else {
        return;
    };
    let Ok((_, info_obj)) = pdf.dereference(info_ref) else {
        return;
    };
    let Ok(info) = info_obj.as_dict() else { return };

    meta.title = info_field(pdf, info, b"Title").map(|s| Field::new(s, "pdf-info"));
    if let Some(s) = info_field(pdf, info, b"Author") {
        meta.authors = split_info_authors(&s);
        meta.authors_source = Some("pdf-info");
    }
    meta.publisher = info_field(pdf, info, b"Publisher").map(|s| Field::new(s, "pdf-info-custom"));
    meta.year = [b"PublicationYear".as_slice(), b"Year".as_slice()]
        .iter()
        .find_map(|key| info_field(pdf, info, key).and_then(|s| publication_year(&s)))
        .map(|year| Field::new(year, "pdf-info-custom"));
}

// Find an XMP property regardless of the namespace prefix chosen by the PDF producer.
fn xmp_value(doc: &roxmltree::Document<'_>, ns: &str, name: &str) -> Option<String> {
    // XMP usually encodes properties as child elements, sometimes as RDF attributes.
    for node in doc.descendants().filter(|n| n.is_element()) {
        if node.tag_name().namespace() == Some(ns) && node.tag_name().name() == name {
            // rdf:Alt -> prefer x-default if present.
            if let Some(x_default) = node.descendants().find(|n| {
                n.is_element()
                    && n.attribute(("http://www.w3.org/XML/1998/namespace", "lang"))
                        == Some("x-default")
            }) {
                if let Some(s) = x_default
                    .descendants()
                    .find(|n| n.is_text())
                    .and_then(|n| n.text())
                    .and_then(nonempty)
                {
                    return Some(s);
                }
            }
            if let Some(s) = node
                .descendants()
                .find(|n| n.is_text())
                .and_then(|n| n.text())
                .and_then(nonempty)
            {
                return Some(s);
            }
        }
        if let Some(attribute) = node
            .attributes()
            .find(|a| a.namespace() == Some(ns) && a.name() == name)
        {
            if let Some(s) = nonempty(attribute.value()) {
                return Some(s);
            }
        }
    }
    None
}

fn xmp_authors(doc: &roxmltree::Document<'_>) -> Vec<String> {
    for node in doc.descendants().filter(|n| n.is_element()) {
        if node.tag_name().namespace() == Some(DC) && node.tag_name().name() == "creator" {
            let authors: Vec<String> = node
                .descendants()
                .filter(|n| n.is_element() && n.tag_name().name() == "li")
                .filter_map(|li| {
                    li.descendants()
                        .find(|n| n.is_text())
                        .and_then(|n| n.text())
                        .and_then(nonempty)
                })
                .collect();
            if !authors.is_empty() {
                return authors;
            }
            if let Some(s) = node
                .descendants()
                .find(|n| n.is_text())
                .and_then(|n| n.text())
                .and_then(nonempty)
            {
                return vec![s];
            }
        }
    }
    xmp_value(doc, DC, "creator")
        .map(|s| vec![s])
        .unwrap_or_default()
}

pub fn parse_xmp(xml: &str, meta: &mut BookMetadata) -> Result<()> {
    let doc = roxmltree::Document::parse(xml).context("invalid XMP XML")?;
    if let Some(title) = xmp_value(&doc, DC, "title") {
        meta.title = Some(Field::new(title, "xmp:dc:title"));
    }
    let authors = xmp_authors(&doc);
    if !authors.is_empty() {
        meta.authors = authors;
        meta.authors_source = Some("xmp:dc:creator");
    }
    if let Some(publisher) = xmp_value(&doc, DC, "publisher") {
        meta.publisher = Some(Field::new(publisher, "xmp:dc:publisher"));
    }
    for (ns, name, source) in [
        (PRISM, "publicationDate", "xmp:prism:publicationDate"),
        (
            "http://prismstandard.org/namespaces/basic/2.1/",
            "publicationDate",
            "xmp:prism:publicationDate",
        ),
        (
            "http://prismstandard.org/namespaces/basic/2.2/",
            "publicationDate",
            "xmp:prism:publicationDate",
        ),
        (
            "http://prismstandard.org/namespaces/basic/3.0/",
            "publicationDate",
            "xmp:prism:publicationDate",
        ),
        (DCTERMS, "issued", "xmp:dcterms:issued"),
        (DC, "date", "xmp:dc:date (ambiguous date)"),
    ] {
        if let Some(year) = xmp_value(&doc, ns, name).and_then(|s| publication_year(&s)) {
            // An explicitly labeled publication year takes precedence over ambiguous dc:date.
            if source.contains("ambiguous") && meta.year.is_some() {
                break;
            }
            meta.year = Some(Field::new(year, source));
            if source.contains("ambiguous") {
                meta.warnings.push(
                    "dc:date is not necessarily the publication date; please verify the year"
                        .into(),
                );
            }
            break;
        }
    }
    Ok(())
}

pub fn extract(path: &Path, enrich: bool, ocr: bool) -> Result<BookMetadata> {
    // 32 MiB per decoded stream bounds decompression bombs while loading.
    let options = LoadOptions::with_max_decompressed_size(STREAM_LIMIT);
    let pdf = Document::load_with_options(path, options)
        .with_context(|| format!("cannot read PDF {}", path.display()))?;
    let mut meta = BookMetadata::default();
    parse_info(&pdf, &mut meta);

    if let Ok(catalog) = pdf.catalog() {
        if let Ok(stream_ref) = catalog.get(b"Metadata") {
            match pdf
                .dereference(stream_ref)
                .ok()
                .and_then(|(_, obj)| obj.as_stream().ok())
            {
                Some(stream) => match stream.get_plain_content_with_limit(XMP_LIMIT) {
                    Ok(data) => match std::str::from_utf8(&data) {
                        Ok(xml) => {
                            if let Err(e) = parse_xmp(xml, &mut meta) {
                                meta.warnings.push(format!("XMP ignored: {e}"));
                            }
                        }
                        Err(_) => meta.warnings.push("XMP was not UTF-8; skipped".into()),
                    },
                    Err(e) => meta.warnings.push(format!("Cannot decode XMP stream: {e}")),
                },
                None => meta
                    .warnings
                    .push("/Metadata does not reference a stream".into()),
            }
        }
    }
    match extract_first_pages(&pdf, 3) {
        Ok(text) => {
            if let Some(doi) = find_doi(&text) {
                meta.doi = Some(Field::new(doi, "pdf-text:doi"));
            }
        }
        Err(e) => meta
            .warnings
            .push(format!("Internal text extraction ignored: {e:#}")),
    }

    if metadata_incomplete(&meta) {
        match layout_text_first_pages(path, 3) {
            Ok(text) => merge_first_page_text(&mut meta, &text, "poppler:first-page"),
            Err(e) => meta
                .warnings
                .push(format!("First-page inference unavailable: {e:#}")),
        }
    }
    if metadata_incomplete(&meta) && ocr {
        match ocr_first_pages(path, 3) {
            Ok(text) => merge_first_page_text(&mut meta, &text, "ocr:first-page"),
            Err(e) => meta.warnings.push(format!("OCR failed: {e:#}")),
        }
    }
    if meta
        .title
        .as_ref()
        .is_some_and(|field| matches!(field.source, "poppler:first-page" | "ocr:first-page"))
    {
        meta.warnings.push(
            "Metadata inferred from first-page text/OCR; verify the preview before --apply".into(),
        );
    }
    if enrich {
        if meta.doi.is_some() {
            if let Err(e) = enrich_crossref(&mut meta) {
                meta.warnings
                    .push(format!("Crossref enrichment failed: {e:#}"));
            }
        } else {
            meta.warnings.push(
                "Online enrichment requested, but no DOI was found in the first three pages".into(),
            );
        }
    }
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_year_must_be_explicit_and_leading() {
        assert_eq!(publication_year("2023-08-19"), Some(2023));
        assert_eq!(publication_year("D:20231212"), None);
        assert_eq!(publication_year("Published in 2023"), None);
    }

    #[test]
    fn xmp_fields_and_authors() {
        let xml = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:prism="http://prismstandard.org/namespaces/basic/2.0/">
          <rdf:RDF><rdf:Description>
          <dc:title><rdf:Alt><rdf:li xml:lang="x-default">Numerical Methods</rdf:li></rdf:Alt></dc:title>
          <dc:creator><rdf:Seq><rdf:li>Jane Doe</rdf:li><rdf:li>John Smith</rdf:li></rdf:Seq></dc:creator>
          <dc:publisher><rdf:Bag><rdf:li>Example Press</rdf:li></rdf:Bag></dc:publisher>
          <prism:publicationDate>2022-05-01</prism:publicationDate>
          </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let mut meta = BookMetadata::default();
        parse_xmp(xml, &mut meta).unwrap();
        assert_eq!(meta.title.unwrap().value, "Numerical Methods");
        assert_eq!(meta.authors, vec!["Jane Doe", "John Smith"]);
        assert_eq!(meta.publisher.unwrap().value, "Example Press");
        assert_eq!(meta.year.unwrap().value, 2022);
    }

    #[test]
    fn doi_is_detected_and_normalized() {
        assert_eq!(
            find_doi("https://doi.org/10.1016/j.compfluid.2022.105702."),
            Some("10.1016/j.compfluid.2022.105702".into())
        );
        assert_eq!(
            find_doi("DOI: 10.5555/ABC-123"),
            Some("10.5555/abc-123".into())
        );
    }

    #[test]
    fn crossref_fields_are_merged() {
        let work: CrossrefWork = serde_json::from_str(
            r#"{
          "title":["A Kinetic Method"], "publisher":"Elsevier BV",
          "author":[{"given":"K. S.","family":"Shrinath"}],
          "published":{"date-parts":[[2023,1]]}
        }"#,
        )
        .unwrap();
        let mut meta = BookMetadata::default();
        merge_crossref(&mut meta, work);
        assert_eq!(meta.title.unwrap().value, "A Kinetic Method");
        assert_eq!(meta.publisher.unwrap().value, "Elsevier BV");
        assert_eq!(meta.authors, ["K. S. Shrinath"]);
        assert_eq!(meta.year.unwrap().value, 2023);
    }

    #[test]
    fn first_page_fallback_recovers_article_fields() {
        let text = r#"
Computers and Fluids 250 (2023) 105702
Contents lists available at ScienceDirect
A Kinetic Flux Difference Splitting method for compressible flows
K.S. Shrinath, N.H. Maruthi, S.V. Raghurama Rao, Veeredhi Vasudeva Rao
https://doi.org/10.1016/j.compfluid.2022.105702
0045-7930 / © 2022 Elsevier Ltd. All rights reserved.
"#;
        let mut meta = BookMetadata::default();
        merge_first_page_text(&mut meta, text, "test:first-page");
        assert_eq!(
            meta.title.unwrap().value,
            "A Kinetic Flux Difference Splitting method for compressible flows"
        );
        assert_eq!(meta.authors.len(), 4);
        assert_eq!(meta.publisher.unwrap().value, "Elsevier");
        assert_eq!(meta.year.unwrap().value, 2023);
        assert_eq!(meta.doi.unwrap().value, "10.1016/j.compfluid.2022.105702");
    }
}
