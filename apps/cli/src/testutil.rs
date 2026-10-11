//! Fixtures shared by the CLI's tests: a data dir with pdfium and a cached Form 1040, and
//! throwaway workspaces.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

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

/// A fresh, empty directory under the system temp dir.
pub fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "nii-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A complete, empty index at `path`.
pub fn tiny_index(path: &Path) {
    let conn = nii_search::open(path).unwrap();
    nii_search::init_schema(&conn).unwrap();
    nii_search::set_meta(&conn, "chunks", "0").unwrap();
}

/// Serve the files in `root` over HTTP on 127.0.0.1 until the process exits; returns the base URL.
/// Missing files get a 404.
pub fn serve(root: PathBuf) -> String {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut request = String::new();
            BufReader::new(&stream).read_line(&mut request).unwrap_or_default();
            let path = request
                .split_whitespace()
                .nth(1)
                .unwrap_or("/")
                .trim_start_matches('/')
                .to_string();
            let mut stream = stream;
            match std::fs::read(root.join(&path)) {
                Ok(body) => {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(&body);
                }
                Err(_) => {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                }
            }
        }
    });
    base
}
