//! Builds the index of Title 26 and IRS instructions/publications into `<db>.partial`,
//! renamed into place under an exclusive `<db>.lock`.

use anyhow::{Context, Result};
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub enum Progress {
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

    std::fs::rename(&partial, db_path).with_context(|| format!("moving {} into place", partial.display()))?;
    Ok(Built {
        chunks: chunks.len(),
        release_point: usc.release_point,
        elapsed: started.elapsed(),
    })
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
