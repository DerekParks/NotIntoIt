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
    // Resolve before using an existing copy or creating directories: a symlinked
    // year/forms directory must not let fetch read or write outside the workspace.
    std::fs::create_dir_all(workspace)?;
    let root = workspace.canonicalize()?;
    let mut ancestor = working.parent().context("form path has no parent")?;
    while !ancestor.exists() {
        ancestor = ancestor.parent().context("form path has no existing ancestor")?;
    }
    crate::documents::contained(&root, ancestor)?;
    if working.exists() {
        crate::documents::contained(&root, &working)?;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn fetch_refuses_symlinked_directories_and_existing_forms_outside_workspace() {
        use std::os::unix::fs::symlink;
        let ws = crate::testutil::scratch("fetch-symlink");
        let outside = crate::testutil::scratch("fetch-symlink-outside");
        std::fs::write(outside.join("f1040.pdf"), b"private").unwrap();
        symlink(&outside, ws.join("2025")).unwrap();
        for overwrite in [false, true] {
            let err = fetch(&outside, &ws, "1040", 2025, overwrite).err().unwrap();
            assert!(err.to_string().contains("outside the tax workspace"));
        }
        std::fs::create_dir_all(ws.join("2024/forms")).unwrap();
        symlink(outside.join("f1040.pdf"), ws.join("2024/forms/f1040.pdf")).unwrap();
        let err = fetch(&outside, &ws, "1040", 2024, false).err().unwrap();
        assert!(err.to_string().contains("outside the tax workspace"));
        assert_eq!(std::fs::read(outside.join("f1040.pdf")).unwrap(), b"private");
    }

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
