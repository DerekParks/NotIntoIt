//! Builds the index of Title 26 and IRS instructions/publications, or fetches a prebuilt one,
//! into `<db>.partial`, renamed into place under an exclusive `<db>.lock`.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs::{File, TryLockError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// File name of the prebuilt index attached to each GitHub release.
pub const ASSET: &str = "nii-index.db.zst";
const PACK_LEVEL: i32 = 19;

/// Where this version's prebuilt index lives, unless `NII_INDEX_URL` overrides it ("none" disables).
pub fn prebuilt_url() -> Option<String> {
    match std::env::var("NII_INDEX_URL") {
        Ok(url) if url == "none" => None,
        Ok(url) => Some(url),
        Err(_) => Some(format!(
            "https://github.com/DerekParks/NotIntoIt/releases/download/v{}/{ASSET}",
            env!("CARGO_PKG_VERSION")
        )),
    }
}

pub enum Progress {
    Fetching { bytes: u64, total: Option<u64> },
    Downloading,
    Parsing,
    Embedding { done: usize, total: usize },
}

pub struct BuildOptions {
    pub limit_chunks: Option<usize>,
    pub force_download: bool,
    pub batch_size: usize,
    pub cuda_device: Option<i32>,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            limit_chunks: None,
            force_download: false,
            batch_size: 64,
            cuda_device: None,
        }
    }
}

pub struct Built {
    pub chunks: usize,
    pub release_point: String,
    pub elapsed: Duration,
}

/// Held for the duration of a build.
pub struct BuildLock(#[allow(dead_code)] File);

/// Take the build lock, or `None` if another process holds it.
pub fn try_lock(db_path: &Path) -> Result<Option<BuildLock>> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = File::create(sibling(db_path, "lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(BuildLock(file))),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e.into()),
    }
}

/// True when `db_path` holds a complete index of the current schema.
pub fn is_ready(db_path: &Path) -> bool {
    db_path.exists()
        && nii_search::open(db_path)
            .and_then(|conn| {
                Ok((
                    nii_search::get_meta(&conn, "schema_version")?,
                    nii_search::get_meta(&conn, "chunks")?,
                ))
            })
            .is_ok_and(|(schema, chunks)| schema == Some(nii_search::SCHEMA_VERSION.to_string()) && chunks.is_some())
}

pub fn build(
    data_dir: &Path,
    db_path: &Path,
    _lock: &BuildLock,
    options: &BuildOptions,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<Built> {
    if options.cuda_device.is_some() && !nii_embed::CUDA_ENABLED {
        anyhow::bail!("a CUDA device was requested but this binary was built without the cuda feature");
    }
    let started = Instant::now();

    on_progress(Progress::Downloading);
    let usc = nii_corpus::download::fetch_usc26(data_dir, options.force_download)?;

    let guidance = nii_corpus::irs_html::fetch_guidance(data_dir, options.force_download)?;

    on_progress(Progress::Parsing);
    let sections = nii_corpus::parse_usc(&usc.xml_path)?;
    let mut chunks: Vec<_> = sections.iter().flat_map(nii_corpus::section_to_chunks).collect();
    let mut titles = Vec::new();
    for g in &guidance {
        let doc = nii_corpus::irs_html::guidance_chunks(g)?;
        if let Some(first) = doc.first() {
            titles.push(first.citation.clone());
        }
        chunks.extend(doc);
    }
    if let Some(n) = options.limit_chunks {
        chunks.truncate(n);
    }

    let mut embedder = nii_embed::Embedder::new(
        &data_dir.join("models"),
        nii_embed::Device {
            cuda_device_id: options.cuda_device,
        },
    )?;

    let partial = sibling(db_path, "partial");
    remove_db(&partial);
    let mut conn = nii_search::open(&partial)?;
    nii_search::init_schema(&conn)?;

    on_progress(Progress::Embedding {
        done: 0,
        total: chunks.len(),
    });
    for (i, batch) in chunks.chunks(options.batch_size).enumerate() {
        let texts: Vec<String> = batch.iter().map(|c| c.embed_text()).collect();
        let embeddings = embedder.embed_docs(&texts, options.batch_size)?;
        nii_search::add_chunks(&mut conn, batch, &embeddings)?;
        let done = (i * options.batch_size + batch.len()).min(chunks.len());
        on_progress(Progress::Embedding {
            done,
            total: chunks.len(),
        });
    }

    let unix_now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    nii_search::set_meta(&conn, "model", "bge-small-en-v1.5-q")?;
    nii_search::set_meta(&conn, "dim", &nii_search::DIM.to_string())?;
    nii_search::set_meta(&conn, "usc_release_point", &usc.release_point)?;
    nii_search::set_meta(&conn, "guidance", &titles.join("; "))?;
    nii_search::set_meta(&conn, "built_at_unix", &unix_now.to_string())?;
    nii_search::set_meta(&conn, "chunks", &chunks.len().to_string())?;
    conn.close().map_err(|(_, e)| e).context("closing new index")?;

    remove_db(db_path);
    std::fs::rename(&partial, db_path).with_context(|| format!("moving {} into place", partial.display()))?;
    Ok(Built {
        chunks: chunks.len(),
        release_point: usc.release_point,
        elapsed: started.elapsed(),
    })
}

/// Download `url` and its `.sha256`, verify, and install the index at `db_path`.
pub fn fetch(url: &str, db_path: &Path, _lock: &BuildLock, on_progress: &mut dyn FnMut(Progress)) -> Result<u64> {
    let checksum_url = format!("{url}.sha256");
    let mut checksum = String::new();
    ureq::get(&checksum_url)
        .call()
        .with_context(|| format!("downloading {checksum_url}"))?
        .body_mut()
        .as_reader()
        .read_to_string(&mut checksum)?;
    let expected = checksum
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();

    let mut response = ureq::get(url).call().with_context(|| format!("downloading {url}"))?;
    let total = response.body().content_length();
    let mut bytes = 0u64;
    let body = ProgressReader {
        inner: response.body_mut().as_reader(),
        on_read: &mut |n| {
            bytes += n as u64;
            on_progress(Progress::Fetching { bytes, total });
        },
    };
    install(body, &expected, db_path)
}

/// Decompress a packed index from `packed`, check its SHA-256 and schema, and move it into place.
pub fn install(packed: impl Read, expected_sha256: &str, db_path: &Path) -> Result<u64> {
    let partial = sibling(db_path, "partial");
    remove_db(&partial);
    let mut hashing = HashingReader {
        inner: packed,
        hasher: Sha256::new(),
        bytes: 0,
    };
    let mut decoder = zstd::stream::read::Decoder::new(&mut hashing)?;
    let mut out = File::create(&partial)?;
    std::io::copy(&mut decoder, &mut out).context("decompressing index")?;
    std::io::copy(&mut decoder.finish(), &mut std::io::sink())?;
    out.sync_all()?;
    let actual = hex(&hashing.hasher.finalize());
    if actual != expected_sha256 {
        remove_db(&partial);
        bail!("index checksum mismatch: expected {expected_sha256}, got {actual}");
    }
    if !is_ready(&partial) {
        remove_db(&partial);
        bail!(
            "downloaded index is incomplete or for another schema (this build needs schema {})",
            nii_search::SCHEMA_VERSION
        );
    }
    remove_db(db_path);
    std::fs::rename(&partial, db_path).with_context(|| format!("moving {} into place", partial.display()))?;
    Ok(hashing.bytes)
}

/// Write a compacted, zstd-compressed copy of the index and its `.sha256` into `out_dir`.
pub fn pack(db_path: &Path, out_dir: &Path) -> Result<PathBuf> {
    if !is_ready(db_path) {
        bail!("{} is not a complete index", db_path.display());
    }
    std::fs::create_dir_all(out_dir)?;
    let compact = out_dir.join("nii-index.db");
    let _ = std::fs::remove_file(&compact);
    nii_search::open(db_path)?.execute("VACUUM INTO ?1", [compact.to_string_lossy()])?;

    let asset = out_dir.join(ASSET);
    let mut encoder = zstd::stream::write::Encoder::new(File::create(&asset)?, PACK_LEVEL)?;
    std::io::copy(&mut File::open(&compact)?, &mut encoder)?;
    encoder.finish()?.sync_all()?;
    std::fs::remove_file(&compact)?;

    let mut hashing = HashingReader {
        inner: File::open(&asset)?,
        hasher: Sha256::new(),
        bytes: 0,
    };
    std::io::copy(&mut hashing, &mut std::io::sink())?;
    let mut sum = File::create(out_dir.join(format!("{ASSET}.sha256")))?;
    writeln!(sum, "{}  {ASSET}", hex(&hashing.hasher.finalize()))?;
    Ok(asset)
}

struct HashingReader<R> {
    inner: R,
    hasher: Sha256,
    bytes: u64,
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
}

struct ProgressReader<'a, R> {
    inner: R,
    on_read: &'a mut dyn FnMut(usize),
}

impl<R: Read> Read for ProgressReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        (self.on_read)(n);
        Ok(n)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sibling(db_path: &Path, suffix: &str) -> PathBuf {
    let mut name = db_path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(suffix);
    db_path.with_file_name(name)
}

fn remove_db(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_is_exclusive_across_handles() {
        let dir = std::env::temp_dir().join(format!("nii-lock-test-{}", std::process::id()));
        let db = dir.join("index.db");
        let held = try_lock(&db).unwrap();
        assert!(held.is_some());
        assert!(
            try_lock(&db).unwrap().is_none(),
            "second lock must fail while first is held"
        );
        drop(held);
        assert!(try_lock(&db).unwrap().is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn tiny_index(path: &Path) {
        let conn = nii_search::open(path).unwrap();
        nii_search::init_schema(&conn).unwrap();
        nii_search::set_meta(&conn, "chunks", "0").unwrap();
    }

    #[test]
    fn pack_then_install_round_trips_and_rejects_bad_checksums() {
        let dir = std::env::temp_dir().join(format!("nii-pack-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        tiny_index(&dir.join("source.db"));

        let asset = pack(&dir.join("source.db"), &dir.join("out")).unwrap();
        let sum = std::fs::read_to_string(dir.join("out").join(format!("{ASSET}.sha256"))).unwrap();
        let sha = sum.split_whitespace().next().unwrap();
        assert_eq!(sum.trim_end(), format!("{sha}  {ASSET}"));

        let target = dir.join("index.db");
        let err = install(File::open(&asset).unwrap(), &"0".repeat(64), &target).unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
        assert!(!target.exists() && !dir.join("index.db.partial").exists());

        install(File::open(&asset).unwrap(), sha, &target).unwrap();
        assert!(is_ready(&target));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_or_incomplete_index_is_not_ready() {
        let dir = std::env::temp_dir().join(format!("nii-ready-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("index.db");
        assert!(!is_ready(&db));
        let conn = nii_search::open(&db).unwrap();
        nii_search::init_schema(&conn).unwrap();
        assert!(!is_ready(&db), "schema without a finished build is not ready");
        nii_search::set_meta(&conn, "chunks", "0").unwrap();
        assert!(is_ready(&db));
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }
}
