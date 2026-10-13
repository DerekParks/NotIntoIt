//! Year-pinned IRS instruction PDFs, kept separate from return forms.
use anyhow::{Context, Result, bail};
use std::path::Path;

fn mapping(form: &str) -> Result<(String, &'static str)> {
    let stem = nii_pdf::irs::form_stem(form)?;
    for line in include_str!("../instruction-mappings.tsv")
        .lines()
        .filter(|l| !l.starts_with('#'))
    {
        let mut columns = line.split('\t');
        if columns.next() == Some(stem.as_str()) {
            let document = columns.next().context("mapping missing document")?;
            let kind = match columns.next() {
                Some("shared") => "shared instructions; check the title for covered forms",
                Some("embedded") => "instructions embedded in the form PDF",
                Some("separate") => "separate instructions",
                _ => bail!("invalid instruction mapping kind"),
            };
            return Ok((document.to_owned(), kind));
        }
    }
    Ok((
        format!("i{}", &stem[1..]),
        "candidate instruction filename; verify the document title and applicability",
    ))
}

/// Exact-year retrieval deliberately never substitutes an earlier edition.
pub fn read(
    data_dir: &Path,
    workspace: &Path,
    form: &str,
    year: u16,
    page: Option<u32>,
    search: Option<&str>,
) -> Result<String> {
    if !(1900..=2100).contains(&year) {
        bail!("tax year must be between 1900 and 2100");
    }
    if page == Some(0) {
        bail!("page is 1-based");
    }
    if search.is_some_and(|s| s.trim().is_empty()) {
        bail!("search must not be empty");
    }
    let (stem, kind) = mapping(form)?;
    let url = nii_pdf::irs::form_url(&stem, year);
    let relative_dir = format!("{year}/instructions");
    std::fs::create_dir_all(workspace)?;
    let root = workspace.canonicalize()?;
    let dir = root.join(&relative_dir);
    // Check every existing ancestor before creating directories.
    let mut ancestor = dir.as_path();
    while ancestor.symlink_metadata().is_err() {
        ancestor = ancestor.parent().context("instruction directory has no ancestor")?;
    }
    crate::documents::contained(&root, ancestor)?;
    std::fs::create_dir_all(&dir)?;
    let path = crate::documents::contained_new(&root, &dir.join(format!("{stem}--{year}.pdf")))?;
    let cache = data_dir.join("instructions").join(format!("{stem}--{year}.pdf"));
    let bytes = if cache.exists() {
        std::fs::read(&cache)?
    } else {
        let bytes = download(&url)?;
        std::fs::create_dir_all(cache.parent().unwrap())?;
        std::fs::write(&cache, &bytes)?;
        bytes
    };
    if !bytes.starts_with(b"%PDF") {
        bail!("cached instructions are not a PDF: {}", cache.display());
    }
    let pages = nii_pdf::text::page_texts(nii_pdf::engine::pdfium(data_dir)?, &bytes)?;
    let output = render(&pages, page, search)?;
    std::fs::write(&path, &bytes)?;
    let supplement = if stem == "f1040sei" {
        "\nFor EIC eligibility and credit calculations, also get Form 1040 instructions and search for line 27a."
    } else {
        ""
    };
    Ok(format!(
        "Requested form: {form}; tax year: {year}\nResolution: {kind}\nSource: {url}\nPDF: {}\nExact-year archive retrieved; verify the title's tax year/revision and applicability, especially for continuous-use forms. No prior-year fallback.{supplement}\n{output}",
        path.display()
    ))
}

fn download(url: &str) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut response = ureq::get(url).call().with_context(|| format!(
        "Could not resolve exact-year instructions at {url}. Instructions may be shared, embedded, or continuous-use with an earlier revision. Check https://www.irs.gov/forms-instructions; no older or current edition was substituted."))?;
    let mut bytes = Vec::new();
    response.body_mut().as_reader().read_to_end(&mut bytes)?;
    if !bytes.starts_with(b"%PDF") {
        bail!("{url} did not return a PDF");
    }
    Ok(bytes)
}

fn render(pages: &[String], page: Option<u32>, search: Option<&str>) -> Result<String> {
    if let Some(p) = page
        && (p == 0 || p as usize > pages.len())
    {
        bail!("page {p} out of range; document has {} pages (1-based)", pages.len());
    }
    let query = search.map(|s| s.to_lowercase());
    let matches: Vec<_> = pages
        .iter()
        .enumerate()
        .filter(|(i, t)| {
            page.is_none_or(|p| p as usize == i + 1) && query.as_ref().is_none_or(|q| t.to_lowercase().contains(q))
        })
        .collect();
    if matches.is_empty() {
        return Ok(format!("No matching pages in this {}-page document.", pages.len()));
    }
    let limit = if search.is_some() { 3 } else { 1 };
    let mut out = format!(
        "{} pages total; {} matching pages. Call again with page or search to navigate.\n",
        pages.len(),
        matches.len()
    );
    for (i, t) in matches.into_iter().take(limit) {
        out.push_str(&format!("\n--- page {} of {} ---\n", i + 1, pages.len()));
        if t.trim().is_empty() {
            out.push_str("No text layer; use view_pdf with this PDF and page.\n");
        } else {
            out.extend(t.chars().take(12000));
            if t.chars().count() > 12000 {
                out.push_str("\n[Page text truncated; use view_pdf for the full page.]\n");
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_embedded_and_irregular_mappings() {
        for f in ["1040", "1040-SR", "Schedule 1", "Schedule 1-A", "Schedule 3"] {
            assert_eq!(mapping(f).unwrap().0, "i1040gi");
        }
        assert_eq!(mapping("Schedule A").unwrap().0, "i1040sca");
        assert_eq!(mapping("Schedule SE").unwrap().0, "i1040sse");
        assert_eq!(mapping("8812").unwrap().0, "i1040s8");
        assert_eq!(mapping("1099-NEC").unwrap().0, "i1099mec");
        assert_eq!(mapping("W-2").unwrap().0, "iw2w3");
        assert_eq!(mapping("W-9").unwrap().0, "fw9");
        for (form, document) in [
            ("f1040s", "i1040gi"),
            ("Schedule R", "i1040sr"),
            ("f1040sr", "i1040sr"),
            ("Schedule EIC", "f1040sei"),
            ("1099-PATR", "i1099ptr"),
            ("1098-E", "i1098et"),
            ("1098-T", "i1098et"),
            ("5498", "i1099r"),
            ("5498-SA", "i1099sa"),
            ("1099-C", "i1099ac"),
            ("5754", "iw2g"),
            ("4868", "f4868"),
            ("8880", "f8880"),
            ("8332", "f8332"),
            ("W-2c", "iw2w3"),
            ("8862", "i8862"),
            ("8919", "f8919"),
        ] {
            assert_eq!(mapping(form).unwrap().0, document, "{form}");
        }
        assert!(mapping("../").is_err());
    }
    #[test]
    fn catalog_is_unique_normalized_and_has_irs_source_evidence() {
        let evidence: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/irs-instruction-audit.json")).unwrap();
        let sources = evidence["sources"].as_array().unwrap();
        let mut seen = std::collections::BTreeSet::new();
        for line in include_str!("../instruction-mappings.tsv")
            .lines()
            .filter(|l| !l.starts_with('#'))
        {
            let columns: Vec<_> = line.split('\t').collect();
            assert_eq!(columns.len(), 3);
            assert!(seen.insert(columns[0]));
            assert_eq!(nii_pdf::irs::form_stem(columns[0]).unwrap(), columns[0]);
            assert_eq!(mapping(columns[0]).unwrap().0, columns[1]);
            assert!(
                sources
                    .iter()
                    .any(|s| s["stem"] == columns[1]
                        && s["url"].as_str().unwrap().starts_with("https://www.irs.gov/pub/")),
                "missing evidence for {}",
                columns[1]
            );
        }
    }
    #[test]
    fn pagination_search_and_bounds() {
        let pages = vec![
            "Introduction".into(),
            "Rental expenses".into(),
            "Rental depreciation".into(),
        ];
        assert!(render(&pages, None, None).unwrap().contains("Introduction"));
        assert!(!render(&pages, None, None).unwrap().contains("Rental expenses"));
        assert!(render(&pages, None, Some("RENTAL")).unwrap().contains("page 3 of 3"));
        assert!(render(&pages, Some(4), None).is_err());
        assert!(
            render(&pages, None, Some("absent"))
                .unwrap()
                .contains("No matching pages")
        );
    }
    #[cfg(unix)]
    #[test]
    fn refuses_external_instruction_directory() {
        let ws = crate::testutil::scratch("instruction-escape");
        let outside = crate::testutil::scratch("instruction-outside");
        std::os::unix::fs::symlink(&outside, ws.join("2025")).unwrap();
        assert!(
            read(&ws, &ws, "1040", 2025, None, None)
                .unwrap_err()
                .to_string()
                .contains("outside the tax workspace")
        );
    }
    #[test]
    fn no_annual_fallback_and_invalid_download() {
        let root = nii_testkit::scratch("instructions-download");
        nii_testkit::put(&root, "i1040gi--2024.pdf", b"%PDF previous year");
        nii_testkit::put(&root, "i1040sc--2025.pdf", b"not PDF");
        let base = nii_testkit::serve(root);
        assert!(
            download(&format!("{base}/i1040gi--2025.pdf"))
                .unwrap_err()
                .to_string()
                .contains("no older or current edition")
        );
        assert!(
            download(&format!("{base}/i1040sc--2025.pdf"))
                .unwrap_err()
                .to_string()
                .contains("did not return a PDF")
        );
    }
}
