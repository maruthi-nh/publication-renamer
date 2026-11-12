use crate::metadata::BookMetadata;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MissingPolicy {
    Skip,
    Omit,
}

fn clean_component(input: &str, limit_bytes: usize) -> String {
    let normalized = input
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                ' '
            } else {
                c
            }
        })
        .collect::<String>();
    let collapsed = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    let collapsed = collapsed.trim_matches(|c| c == '.' || c == ' ').to_string();

    // Truncate at UTF-8 character boundaries, not arbitrary byte offsets.
    let mut result = String::new();
    for ch in collapsed.chars() {
        if result.len() + ch.len_utf8() > limit_bytes {
            break;
        }
        result.push(ch);
    }
    result.trim_end_matches(['.', ' ']).to_string()
}

pub fn target_filename(meta: &BookMetadata, missing: MissingPolicy) -> Result<String, String> {
    let title = meta
        .title
        .as_ref()
        .map(|f| clean_component(&f.value, 100))
        .filter(|s| !s.is_empty())
        .ok_or("missing title")?;
    let publisher = meta
        .publisher
        .as_ref()
        .map(|f| clean_component(&f.value, 50))
        .filter(|s| !s.is_empty());
    let year = meta.year.as_ref().map(|f| f.value.to_string());
    let authors = meta
        .authors
        .iter()
        .map(|a| clean_component(a, 65))
        .filter(|a| !a.is_empty())
        .collect::<Vec<_>>();

    if missing == MissingPolicy::Skip {
        let mut absent = vec![];
        if publisher.is_none() {
            absent.push("publisher");
        }
        if year.is_none() {
            absent.push("publication year");
        }
        if authors.is_empty() {
            absent.push("authors");
        }
        if !absent.is_empty() {
            return Err(format!("missing {}", absent.join(", ")));
        }
    }

    let mut parts = vec![title];
    if let Some(publisher) = publisher {
        parts.push(publisher);
    }
    if let Some(year) = year {
        parts.push(year);
    }
    if !authors.is_empty() {
        parts.push(authors.join(", "));
    }
    let mut stem = parts.join(" - ");
    // A conservative cross-platform cap; reserve four bytes for .pdf.
    while stem.len() > 235 {
        stem.pop();
    }
    stem = stem.trim_end_matches(['.', ' ']).to_string();
    if stem.is_empty() {
        return Err("empty filename after sanitization".into());
    }
    Ok(format!("{stem}.pdf"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::Field;

    fn sample() -> BookMetadata {
        BookMetadata {
            title: Some(Field {
                value: "Rust: A Guide / Intro".into(),
                source: "test",
            }),
            publisher: Some(Field {
                value: "No Starch Press".into(),
                source: "test",
            }),
            year: Some(Field {
                value: 2023,
                source: "test",
            }),
            authors: vec!["Jane Doe".into(), "John Smith".into()],
            ..Default::default()
        }
    }
    #[test]
    fn formats_and_sanitizes() {
        assert_eq!(
            target_filename(&sample(), MissingPolicy::Skip).unwrap(),
            "Rust A Guide Intro - No Starch Press - 2023 - Jane Doe, John Smith.pdf"
        );
    }
    #[test]
    fn missing_fields_are_not_guessed() {
        let mut m = sample();
        m.publisher = None;
        assert!(target_filename(&m, MissingPolicy::Skip).is_err());
        assert_eq!(
            target_filename(&m, MissingPolicy::Omit).unwrap(),
            "Rust A Guide Intro - 2023 - Jane Doe, John Smith.pdf"
        );
    }
    #[test]
    fn unicode_safe_truncation() {
        let result = clean_component(&"日本語".repeat(150), 100);
        assert!(result.len() <= 100);
        assert!(!result.is_empty());
    }
}
