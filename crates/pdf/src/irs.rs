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

pub fn form_url(stem: &str, tax_year: u16) -> String {
    format!("https://www.irs.gov/pub/irs-prior/{stem}--{tax_year}.pdf")
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
    for revision in (tax_year.saturating_sub(REVISION_LOOKBACK)..=tax_year).rev() {
        let url = form_url(stem, revision);
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
    fn url_is_year_pinned() {
        assert_eq!(
            form_url("f1040", 2025),
            "https://www.irs.gov/pub/irs-prior/f1040--2025.pdf"
        );
    }
}
