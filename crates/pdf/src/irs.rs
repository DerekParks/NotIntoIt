//! IRS form PDFs from irs.gov, by year-pinned URL (`irs-prior/f1040--2025.pdf`).

use anyhow::{Context, Result, bail};
use std::io::Read;

/// Normalize a form name to its IRS file stem: "1040" -> "f1040", "W-2" -> "fw2",
/// "Form 1040-SR" -> "f1040sr". Schedules need the stem itself ("f1040s1").
pub fn form_stem(form: &str) -> Result<String> {
    let mut s: String = form
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    if let Some(rest) = s.strip_prefix("form") {
        s = rest.to_string();
    }
    if s.is_empty() {
        bail!("empty form name");
    }
    if !s.starts_with('f') {
        s.insert(0, 'f');
    }
    Ok(s)
}

/// Where irs.gov keeps every published revision of each form.
pub const IRS_PRIOR: &str = "https://www.irs.gov/pub/irs-prior";

pub fn form_url(stem: &str, tax_year: u16) -> String {
    revision_url(IRS_PRIOR, stem, tax_year)
}

fn revision_url(base: &str, stem: &str, revision: u16) -> String {
    format!("{base}/{stem}--{revision}.pdf")
}

/// Revisions older than this many years before the tax year are not considered.
pub const REVISION_LOOKBACK: u16 = 5;

pub struct Download {
    pub bytes: Vec<u8>,
    pub url: String,
    /// Year of the revision used; earlier than the tax year for continuous-use
    /// forms like 1099-INT ("Rev. January 2024").
    pub revision: u16,
}

/// Download the newest revision of a form published at or before `tax_year`.
pub fn download(stem: &str, tax_year: u16) -> Result<Download> {
    download_from(IRS_PRIOR, stem, tax_year)
}

/// [`download`] from a mirror laid out like [`IRS_PRIOR`].
pub fn download_from(base: &str, stem: &str, tax_year: u16) -> Result<Download> {
    for revision in (tax_year.saturating_sub(REVISION_LOOKBACK)..=tax_year).rev() {
        let url = revision_url(base, stem, revision);
        let mut response = match ureq::get(&url).call() {
            Ok(r) => r,
            Err(ureq::Error::StatusCode(404)) => continue,
            Err(e) => return Err(e).with_context(|| format!("downloading {url}")),
        };
        let mut bytes = Vec::new();
        response.body_mut().as_reader().read_to_end(&mut bytes)?;
        if !bytes.starts_with(b"%PDF") {
            bail!("{url} did not return a PDF");
        }
        return Ok(Download { bytes, url, revision });
    }
    bail!(
        "irs.gov has no {stem} revision for tax year {tax_year} or the {REVISION_LOOKBACK} years before; \
         check the form name (IRS file stems look like f1040, f1040s1, f1040sb, fw2, f1099int, f8949)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems() {
        assert_eq!(form_stem("1040").unwrap(), "f1040");
        assert_eq!(form_stem("f1040s1").unwrap(), "f1040s1");
        assert_eq!(form_stem("W-2").unwrap(), "fw2");
        assert_eq!(form_stem("Form 1040-SR").unwrap(), "f1040sr");
        assert_eq!(form_stem(" 8949 ").unwrap(), "f8949");
        assert!(form_stem(" - ").is_err());
    }

    #[test]
    fn downloads_fall_back_to_the_newest_earlier_revision() {
        let root = nii_testkit::scratch("irs");
        nii_testkit::put(&root, "f1099int--2024.pdf", b"%PDF-1.7 int");
        nii_testkit::put(&root, "f1040--2025.pdf", b"%PDF-1.7 current");
        nii_testkit::put(&root, "fbroken--2025.pdf", b"<html>not a pdf</html>");
        let base = nii_testkit::serve(root);

        let current = download_from(&base, "f1040", 2025).unwrap();
        assert_eq!(
            (current.revision, current.bytes.as_slice()),
            (2025, b"%PDF-1.7 current".as_slice())
        );
        assert_eq!(current.url, format!("{base}/f1040--2025.pdf"));

        let continuous = download_from(&base, "f1099int", 2025).unwrap();
        assert_eq!(continuous.revision, 2024);

        let err = download_from(&base, "fbroken", 2025).err().unwrap().to_string();
        assert_eq!(err, format!("{base}/fbroken--2025.pdf did not return a PDF"));

        let err = download_from(&base, "fnope", 2025).err().unwrap().to_string();
        assert!(
            err.starts_with("irs.gov has no fnope revision for tax year 2025 or the 5 years before"),
            "{err}"
        );
    }

    #[test]
    fn unreachable_mirrors_report_the_url() {
        let err = download_from("http://127.0.0.1:9", "f1040", 2025).err().unwrap();
        assert!(
            format!("{err:#}").starts_with("downloading http://127.0.0.1:9/f1040--2025.pdf"),
            "{err:#}"
        );
    }

    #[test]
    fn url_is_year_pinned() {
        assert_eq!(
            form_url("f1040", 2025),
            "https://www.irs.gov/pub/irs-prior/f1040--2025.pdf"
        );
    }
}
