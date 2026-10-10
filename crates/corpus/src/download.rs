//! Fetching corpus sources. Files are cached under a data dir; re-download
//! only with `force`.

use anyhow::{Context, Result};
use std::io::Read;
use std::path::{Path, PathBuf};

const USC_DOWNLOAD_PAGE: &str = "https://uscode.house.gov/download/download.shtml";
/// Fallback release point, verified 2026-07-06.
const USC26_FALLBACK: (u32, u32) = (119, 100);

pub struct Usc26 {
    pub xml_path: PathBuf,
    /// e.g. "119-100" (public-law release point).
    pub release_point: String,
}

/// Download (or reuse cached) Title 26 USLM XML. Returns the XML path and the
/// release point it corresponds to.
pub fn fetch_usc26(data_dir: &Path, force: bool) -> Result<Usc26> {
    let dir = data_dir.join("corpus");
    std::fs::create_dir_all(&dir)?;
    let xml_path = dir.join("usc26.xml");
    let rp_path = dir.join("usc26.release.txt");

    if !force && xml_path.exists() && rp_path.exists() {
        let release_point = std::fs::read_to_string(&rp_path)?.trim().to_string();
        return Ok(Usc26 {
            xml_path,
            release_point,
        });
    }

    let (congress, law) = latest_usc26_release_point().unwrap_or(USC26_FALLBACK);
    let release_point = format!("{congress}-{law}");
    let url =
        format!("https://uscode.house.gov/download/releasepoints/us/pl/{congress}/{law}/xml_usc26@{release_point}.zip");
    let zip_bytes = get_bytes(&url).with_context(|| format!("downloading {url}"))?;

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))?;
    let mut xml = Vec::new();
    archive
        .by_name("usc26.xml")
        .context("usc26.xml missing from release zip")?
        .read_to_end(&mut xml)?;
    std::fs::write(&xml_path, &xml)?;
    std::fs::write(&rp_path, &release_point)?;

    Ok(Usc26 {
        xml_path,
        release_point,
    })
}

/// Scrape the download page for the newest usc26 release point.
fn latest_usc26_release_point() -> Option<(u32, u32)> {
    let html = get_string(USC_DOWNLOAD_PAGE).ok()?;
    let mut best: Option<(u32, u32)> = None;
    for m in html.match_indices("xml_usc26@") {
        let tail = &html[m.0 + "xml_usc26@".len()..];
        let end = tail.find(".zip")?;
        let (congress, law) = tail[..end].split_once('-')?;
        let point = (congress.parse().ok()?, law.parse().ok()?);
        best = Some(best.map_or(point, |b| b.max(point)));
    }
    best
}

pub fn get_bytes(url: &str) -> Result<Vec<u8>> {
    let mut response = ureq::get(url).call()?;
    let mut bytes = Vec::new();
    response.body_mut().as_reader().read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub fn get_string(url: &str) -> Result<String> {
    Ok(String::from_utf8(get_bytes(url)?)?)
}
