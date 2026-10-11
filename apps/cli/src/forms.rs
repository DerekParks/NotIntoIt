//! IRS form workflow on disk: raw downloads cached in the data dir, normalized
//! working copies in the tax workspace (`<workspace>/<year>/forms/<stem>.pdf`).

use anyhow::{Context, Result, bail};
use nii_pdf::fill::{Filled, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub struct Fetched {
    pub path: PathBuf,
    /// Set when this call downloaded the form.
    pub downloaded: Option<nii_pdf::irs::Download>,
    /// Year of the IRS revision the working copy was made from, when known.
    pub revision: Option<u16>,
    /// False when an existing working copy was kept.
    pub created: bool,
}

pub fn fetch(data_dir: &Path, workspace: &Path, form: &str, tax_year: u16, overwrite: bool) -> Result<Fetched> {
    let stem = nii_pdf::irs::form_stem(form)?;
    let working = workspace
        .join(tax_year.to_string())
        .join("forms")
        .join(format!("{stem}.pdf"));
    if working.exists() && !overwrite {
        return Ok(Fetched {
            path: working,
            downloaded: None,
            revision: None,
            created: false,
        });
    }

    let cached = data_dir.join("forms").join(format!("{stem}--{tax_year}.pdf"));
    let revision_note = cached.with_extension("revision");
    let (raw, downloaded, revision) = if cached.exists() {
        let revision = std::fs::read_to_string(&revision_note)
            .ok()
            .and_then(|s| s.trim().parse().ok());
        (std::fs::read(&cached)?, None, revision)
    } else {
        let download = nii_pdf::irs::download(&stem, tax_year)?;
        write_atomic(&cached, &download.bytes)?;
        write_atomic(&revision_note, download.revision.to_string().as_bytes())?;
        (download.bytes.clone(), Some(download), None)
    };
    let revision = revision.or(downloaded.as_ref().map(|d| d.revision));
    write_atomic(&working, &nii_pdf::acroform::normalize(&raw)?)?;
    Ok(Fetched {
        path: working,
        downloaded,
        revision,
        created: true,
    })
}

/// Fill `values` into `pdf`, writing `output` (default: `pdf` itself) when any value was valid.
pub fn fill(pdf: &Path, values: &BTreeMap<String, Value>, output: Option<&Path>) -> Result<(PathBuf, Filled)> {
    if values.is_empty() {
        bail!("values is empty; pass field names from list_form_fields mapped to values");
    }
    let output = output.unwrap_or(pdf);
    if output.extension().and_then(|e| e.to_str()) != Some("pdf") {
        bail!("output must be a .pdf path: {}", output.display());
    }
    let bytes = std::fs::read(pdf).with_context(|| format!("reading {}", pdf.display()))?;
    let filled = nii_pdf::fill::fill(&bytes, values)?;
    if let Some(new) = &filled.pdf {
        write_atomic(output, new)?;
    }
    Ok((output.to_path_buf(), filled))
}

pub fn render(data_dir: &Path, pdf: &Path, page: u32, width: u32) -> Result<nii_pdf::render::RenderedPage> {
    let bytes = std::fs::read(pdf).with_context(|| format!("reading {}", pdf.display()))?;
    nii_pdf::render::render_page(nii_pdf::engine::pdfium(data_dir)?, &bytes, page, width)
}

/// JSON -> fill value: strings and numbers are text, booleans tick boxes.
pub fn value_from_json(name: &str, v: &serde_json::Value) -> Result<Value> {
    Ok(match v {
        serde_json::Value::String(s) => Value::Text(s.clone()),
        serde_json::Value::Number(n) => Value::Text(n.to_string()),
        serde_json::Value::Bool(b) => Value::Bool(*b),
        other => bail!("{name}: expected a string, number, or boolean, got {other}"),
    })
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("path has no parent directory")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}
