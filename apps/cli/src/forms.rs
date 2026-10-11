//! IRS form workflow on disk: raw downloads cached in the data dir, normalized
//! working copies in the tax workspace (`<workspace>/<year>/forms/<stem>.pdf`).

use anyhow::{Context, Result, bail};
use nii_pdf::fill::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub struct Fetched {
    pub path: PathBuf,
    /// Set when this call downloaded the form.
    pub downloaded: Option<nii_pdf::irs::Download>,
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
            created: false,
        });
    }

    let cached = data_dir.join("forms").join(format!("{stem}--{tax_year}.pdf"));
    let (raw, downloaded) = if cached.exists() {
        (std::fs::read(&cached)?, None)
    } else {
        let download = nii_pdf::irs::download(&stem, tax_year)?;
        write_atomic(&cached, &download.bytes)?;
        (download.bytes.clone(), Some(download))
    };
    write_atomic(&working, &nii_pdf::acroform::normalize(&raw)?)?;
    Ok(Fetched {
        path: working,
        downloaded,
        created: true,
    })
}

pub fn fill(pdf: &Path, values: &BTreeMap<String, Value>, output: Option<&Path>) -> Result<PathBuf> {
    let output = output.unwrap_or(pdf);
    if output.extension().and_then(|e| e.to_str()) != Some("pdf") {
        bail!("output must be a .pdf path: {}", output.display());
    }
    let bytes = std::fs::read(pdf).with_context(|| format!("reading {}", pdf.display()))?;
    let filled = nii_pdf::fill::fill(&bytes, values)?;
    write_atomic(output, &filled)?;
    Ok(output.to_path_buf())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_values_map_to_text_and_checkboxes() {
        let v = |j: serde_json::Value| value_from_json("f", &j);
        assert_eq!(v(serde_json::json!("Jane")).unwrap(), Value::Text("Jane".into()));
        assert_eq!(v(serde_json::json!(72805)).unwrap(), Value::Text("72805".into()));
        assert_eq!(v(serde_json::json!(12.5)).unwrap(), Value::Text("12.5".into()));
        assert_eq!(v(serde_json::json!(true)).unwrap(), Value::Bool(true));
        assert_eq!(
            v(serde_json::json!({})).unwrap_err().to_string(),
            "f: expected a string, number, or boolean, got {}"
        );
    }
}
