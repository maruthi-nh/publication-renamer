use lopdf::{dictionary, Document, Object, Stream};
use std::{fs, path::Path, process::Command};

fn pdf_fixture(path: &Path) {
    let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
    <x:xmpmeta xmlns:x="adobe:ns:meta/" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
               xmlns:dc="http://purl.org/dc/elements/1.1/"
               xmlns:prism="http://prismstandard.org/namespaces/basic/2.0/">
      <rdf:RDF><rdf:Description>
       <dc:title><rdf:Alt><rdf:li xml:lang="x-default">Computational Fluid Dynamics</rdf:li></rdf:Alt></dc:title>
       <dc:creator><rdf:Seq><rdf:li>Alice Researcher</rdf:li><rdf:li>Bob Engineer</rdf:li></rdf:Seq></dc:creator>
       <dc:publisher><rdf:Bag><rdf:li>Science Press</rdf:li></rdf:Bag></dc:publisher>
       <prism:publicationDate>2024-04-01</prism:publicationDate>
      </rdf:Description></rdf:RDF>
    </x:xmpmeta>"#;
    let mut pdf = Document::with_version("1.5");
    let pages_id = pdf.add_object(dictionary! {
        "Type" => "Pages", "Kids" => Vec::<Object>::new(), "Count" => 0,
    });
    let xmp_id = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "Metadata", "Subtype" => "XML",
        },
        xmp.as_bytes().to_vec(),
    ));
    let catalog_id = pdf.add_object(dictionary! {
        "Type" => "Catalog", "Pages" => pages_id, "Metadata" => xmp_id,
    });
    let info_id = pdf.add_object(dictionary! {
        "Title" => Object::string_literal("Old PDF Title"),
        "Author" => Object::string_literal("Old Author"),
        "CreationDate" => Object::string_literal("D:20261026000000"),
    });
    pdf.trailer.set("Root", catalog_id);
    pdf.trailer.set("Info", info_id);
    pdf.save(path).unwrap();
}

#[test]
fn inspect_preview_and_apply() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("download-123.pdf");
    pdf_fixture(&source);
    let expected = dir.path().join(
        "Computational Fluid Dynamics - Science Press - 2024 - Alice Researcher, Bob Engineer.pdf",
    );

    let inspect = Command::new(env!("CARGO_BIN_EXE_pdfname"))
        .args(["inspect", source.to_str().unwrap(), "--json", "--offline"])
        .output()
        .unwrap();
    assert!(
        inspect.status.success(),
        "{}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(json["title"]["value"], "Computational Fluid Dynamics");
    assert_eq!(json["year"]["value"], 2024);
    assert_eq!(json["authors"].as_array().unwrap().len(), 2);

    let dry_run = Command::new(env!("CARGO_BIN_EXE_pdfname"))
        .args(["rename", source.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(dry_run.status.success());
    assert!(source.exists());
    assert!(!expected.exists());

    let original_bytes = fs::read(&source).unwrap();
    let apply = Command::new(env!("CARGO_BIN_EXE_pdfname"))
        .args(["rename", source.to_str().unwrap(), "--apply"])
        .output()
        .unwrap();
    assert!(
        apply.status.success(),
        "{}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert!(!source.exists());
    assert_eq!(fs::read(&expected).unwrap(), original_bytes);
}
