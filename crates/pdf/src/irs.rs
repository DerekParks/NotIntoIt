//! IRS form PDFs from irs.gov, by year-pinned URL (`irs-prior/f1040--2025.pdf`).

use anyhow::{Context, Result, bail};
use std::io::Read;

/// Normalize a form name to its IRS file stem: "1040" -> "f1040", "W-2" -> "fw2",
/// "Form 1040-SR" -> "f1040sr", "Schedule D" or "sd" -> "f1040sd", "8812" -> "f1040s8".
pub fn form_stem(form: &str) -> Result<String> {
    let mut s: String = form
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    for prefix in ["form", "f"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
        }
    }
    if s.is_empty() {
        bail!("empty form name");
    }
    if s == "se" || s == "8812" {
        return Ok(if s == "se" { "f1040sse" } else { "f1040s8" }.to_string());
    }
    let explicit = ["1040schedule", "1040sch", "schedule", "sch"];
    let mut prefixes = explicit.iter().chain(["1040s", "s"].iter());
    if let Some((prefix, rest)) = prefixes.find_map(|p| s.strip_prefix(p).map(|rest| (*p, rest))) {
        let rest = rest
            .strip_suffix("form1040")
            .or_else(|| rest.strip_suffix("1040"))
            .unwrap_or(rest);
        let rest = if rest == "8812" { "8" } else { rest };
        if SCHEDULES.contains(&rest) {
            return Ok(format!("f1040s{rest}"));
        }
        if explicit.contains(&prefix) {
            bail!(
                "unknown Form 1040 schedule {rest:?}; the schedules are {}",
                SCHEDULES.join(", ")
            );
        }
    }
    Ok(format!("f{s}"))
}

/// Form 1040 schedules, by the suffix of their stem (Schedule 8812 is f1040s8).
const SCHEDULES: [&str; 13] = ["1", "2", "3", "8", "a", "b", "c", "d", "e", "f", "h", "j", "se"];

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
         check the form name (IRS file stems look like f1040, f1040s1, f1040sd, f1040s8 for Schedule 8812, \
         fw2, f1099int, f8949, f2441)"
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
        for schedule_d in [
            "Schedule D",
            "sd",
            "fsd",
            "sch d",
            "f1040sd",
            "1040 Schedule D",
            "sd1040",
            "schedule-d",
        ] {
            assert_eq!(form_stem(schedule_d).unwrap(), "f1040sd", "{schedule_d}");
        }
        for s8812 in ["8812", "f8812", "s8812", "f1040s8812", "Schedule 8812", "f1040s8"] {
            assert_eq!(form_stem(s8812).unwrap(), "f1040s8", "{s8812}");
        }
        assert_eq!(form_stem("Schedule SE").unwrap(), "f1040sse");
        assert_eq!(form_stem("SE").unwrap(), "f1040sse");
        assert_eq!(form_stem("1040 Sch A").unwrap(), "f1040sa");
        assert_eq!(form_stem("Schedule 3 (Form 1040)").unwrap(), "f1040s3");
        assert!(
            form_stem("Schedule C-EZ")
                .unwrap_err()
                .to_string()
                .contains("unknown Form 1040 schedule")
        );
        assert!(form_stem("Schedule K-1").is_err());
        assert_eq!(form_stem("2441").unwrap(), "f2441");
        assert_eq!(form_stem("f1099int").unwrap(), "f1099int");
        assert_eq!(form_stem("1099-INT").unwrap(), "f1099int");
        assert_eq!(form_stem("fw2").unwrap(), "fw2");
        assert_eq!(form_stem("1040-SR").unwrap(), "f1040sr");
        assert_eq!(form_stem("1040-ES").unwrap(), "f1040es");
        assert_eq!(form_stem("SS-4").unwrap(), "fss4");
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
