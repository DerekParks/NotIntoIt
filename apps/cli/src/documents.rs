//! The user's source documents: listing and reading files inside the tax
//! workspace only. PDFs are read through their text layer.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

const TEXT_EXTENSIONS: &[&str] = &["txt", "md", "csv", "tsv", "json"];
const MAX_TEXT_BYTES: u64 = 256 * 1024;
/// Pages with fewer characters than this are probably scans.
const MIN_PAGE_TEXT: usize = 20;

pub struct Entry {
    pub relative: PathBuf,
    pub bytes: u64,
}

/// Every file under `dir` (relative to `workspace`), sorted, hidden files skipped.
pub fn list(workspace: &Path, dir: &Path) -> Result<Vec<Entry>> {
    let root = contained(workspace, dir)?;
    let workspace = workspace.canonicalize()?;
    let mut out = Vec::new();
    let mut pending = vec![root];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).with_context(|| format!("listing {}", dir.display()))? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let meta = entry.path().symlink_metadata()?;
            if meta.is_symlink() {
                continue;
            }
            if meta.is_dir() {
                pending.push(entry.path());
            } else {
                let relative = entry.path().strip_prefix(&workspace)?.to_path_buf();
                out.push(Entry {
                    relative,
                    bytes: meta.len(),
                });
            }
        }
    }
    out.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(out)
}

pub enum Document {
    Text(String),
    Pdf { pages: Vec<String> },
}

impl Document {
    /// Render for a model, marking pages that need `view_pdf` instead.
    pub fn render(&self, path: &Path) -> String {
        match self {
            Document::Text(t) => t.clone(),
            Document::Pdf { pages } => {
                pages
                    .iter()
                    .enumerate()
                    .map(|(i, text)| {
                        let n = i + 1;
                        if text.trim().chars().count() < MIN_PAGE_TEXT {
                            format!(
                                "--- page {n} of {}: no text layer (scanned?); look at it with view_pdf page={n} ---",
                                pages.len()
                            )
                        } else {
                            format!("--- page {n} of {} ---\n{}", pages.len(), text.trim())
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n")
                    + &format!("\n\n(Source: {})", path.display())
            }
        }
    }
}

pub fn read(workspace: &Path, path: &Path, pdf_text: impl FnOnce(&[u8]) -> Result<Vec<String>>) -> Result<Document> {
    let path = contained(workspace, path)?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if ext == "pdf" {
        return Ok(Document::Pdf {
            pages: pdf_text(&std::fs::read(&path)?)?,
        });
    }
    if !TEXT_EXTENSIONS.contains(&ext.as_str()) {
        bail!(
            "can't read .{ext} files; supported: pdf, {}",
            TEXT_EXTENSIONS.join(", ")
        );
    }
    let size = std::fs::metadata(&path)?.len();
    if size > MAX_TEXT_BYTES {
        bail!("{} is {size} bytes; the limit is {MAX_TEXT_BYTES}", path.display());
    }
    Ok(Document::Text(
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?,
    ))
}

/// `path` (absolute or workspace-relative) resolved, refusing anything outside the workspace.
pub fn contained(workspace: &Path, path: &Path) -> Result<PathBuf> {
    let root = workspace
        .canonicalize()
        .with_context(|| format!("workspace {} does not exist", workspace.display()))?;
    let full = root
        .join(path)
        .canonicalize()
        .with_context(|| format!("{} does not exist", path.display()))?;
    inside(&root, full, path)
}

/// Like `contained` for a file that may not exist yet: its directory must be in the workspace.
pub fn contained_new(workspace: &Path, path: &Path) -> Result<PathBuf> {
    let root = workspace
        .canonicalize()
        .with_context(|| format!("workspace {} does not exist", workspace.display()))?;
    let full = root.join(path);
    let name = full.file_name().context("path has no file name")?.to_owned();
    let dir = full
        .parent()
        .context("path has no parent directory")?
        .canonicalize()
        .with_context(|| format!("the directory of {} does not exist", path.display()))?;
    inside(&root, dir.join(name), path)
}

fn inside(root: &Path, full: PathBuf, shown: &Path) -> Result<PathBuf> {
    if !full.starts_with(root) {
        bail!(
            "{} is outside the tax workspace {}; only files in the workspace can be used",
            shown.display(),
            root.display()
        );
    }
    Ok(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nii-docs-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("2025/documents")).unwrap();
        std::fs::write(dir.join("2025/intake.md"), "Jane Doe notes").unwrap();
        std::fs::write(dir.join("2025/documents/w2.pdf"), "%PDF fake").unwrap();
        std::fs::write(dir.join("2025/.hidden"), "x").unwrap();
        dir
    }

    #[test]
    fn lists_and_reads_inside_the_workspace() {
        let ws = workspace();
        let names: Vec<_> = list(&ws, Path::new("2025"))
            .unwrap()
            .into_iter()
            .map(|e| e.relative)
            .collect();
        assert_eq!(
            names,
            vec![PathBuf::from("2025/documents/w2.pdf"), PathBuf::from("2025/intake.md")]
        );

        let Document::Text(t) = read(&ws, Path::new("2025/intake.md"), |_| unreachable!()).unwrap() else {
            panic!()
        };
        assert_eq!(t, "Jane Doe notes");
        let pdf = read(&ws, &ws.join("2025/documents/w2.pdf"), |_| {
            Ok(vec!["Wages 72805.00 box 1".into(), " ".into()])
        })
        .unwrap();
        let shown = pdf.render(Path::new("w2.pdf"));
        assert!(shown.contains("--- page 1 of 2 ---\nWages 72805.00 box 1"));
        assert!(shown.contains("page 2 of 2: no text layer"));
        let _ = std::fs::remove_dir_all(ws);
    }

    #[test]
    fn refuses_paths_outside_the_workspace() {
        let ws = workspace();
        for escape in ["../", "/etc/hostname", "2025/../../"] {
            let err = read(&ws, Path::new(escape), |_| unreachable!())
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default();
            let listed = list(&ws, Path::new(escape))
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default();
            assert!(
                err.contains("outside the tax workspace") || err.contains("can't read"),
                "{escape}: {err}"
            );
            assert!(listed.contains("outside the tax workspace"), "{escape}: {listed}");
        }
        assert!(read(&ws, Path::new("2025/.hidden"), |_| unreachable!()).is_err());
        let new = contained_new(&ws, Path::new("2025/documents/../new.pdf")).unwrap();
        assert_eq!(new, ws.canonicalize().unwrap().join("2025/new.pdf"));
        assert!(contained_new(&ws, Path::new("2025/../../escape.pdf")).is_err());
        assert!(contained_new(&ws, Path::new("2025/missing-dir/x.pdf")).is_err());
        let _ = std::fs::remove_dir_all(ws);
    }
}
