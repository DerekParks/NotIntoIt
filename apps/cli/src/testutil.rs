//! Fixtures shared by the CLI's tests: a data dir with pdfium and a cached Form 1040, and
//! throwaway workspaces.

use std::path::{Path, PathBuf};

pub use nii_testkit::{scratch, serve};

pub const F1040: &[u8] = include_bytes!("../../../crates/pdf/tests/fixtures/f1040--2025.pdf");

/// Shared with the nii-pdf tests, so pdfium is downloaded once per checkout.
pub fn data_dir() -> PathBuf {
    static SEED: std::sync::Once = std::sync::Once::new();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-data");
    SEED.call_once(|| {
        let forms = dir.join("forms");
        std::fs::create_dir_all(&forms).unwrap();
        let staged = forms.join(format!(".f1040--2025.pdf.{}", std::process::id()));
        std::fs::write(&staged, F1040).unwrap();
        std::fs::rename(staged, forms.join("f1040--2025.pdf")).unwrap();
    });
    dir
}

/// A complete, empty index at `path`.
pub fn tiny_index(path: &Path) {
    let conn = nii_search::open(path).unwrap();
    nii_search::init_schema(&conn).unwrap();
    nii_search::set_meta(&conn, "chunks", "0").unwrap();
}
