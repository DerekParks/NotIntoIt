//! The pdfium shared library: downloaded once from bblanchon/pdfium-binaries
//! into the data dir, then bound for the life of the process.

use anyhow::{Context, Result, bail};
use pdfium_render::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const PDFIUM_RELEASE: u32 = 7934;

static PDFIUM: OnceLock<Pdfium> = OnceLock::new();
static INIT: Mutex<()> = Mutex::new(());

/// The process-wide pdfium instance, installing the library under
/// `<data_dir>/pdfium/` on first use.
pub fn pdfium(data_dir: &Path) -> Result<&'static Pdfium> {
    if let Some(p) = PDFIUM.get() {
        return Ok(p);
    }
    let _guard = INIT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = PDFIUM.get() {
        return Ok(p);
    }
    let lib = install(data_dir)?;
    let bindings = Pdfium::bind_to_library(&lib).with_context(|| format!("loading pdfium from {}", lib.display()))?;
    Ok(PDFIUM.get_or_init(|| Pdfium::new(bindings)))
}

/// Path to the platform pdfium library, downloading it if absent.
pub fn install(data_dir: &Path) -> Result<PathBuf> {
    let (asset, lib_in_archive) = platform_asset()?;
    let dir = data_dir.join("pdfium").join(format!("chromium-{PDFIUM_RELEASE}"));
    let lib = dir.join(lib_in_archive);
    if lib.exists() {
        return Ok(lib);
    }

    let url =
        format!("https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/{PDFIUM_RELEASE}/{asset}.tgz");
    let mut response = ureq::get(&url).call().with_context(|| format!("downloading {url}"))?;
    let staging = dir.with_extension(format!("partial-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let gz = flate2::read::GzDecoder::new(response.body_mut().as_reader());
    tar::Archive::new(gz)
        .unpack(&staging)
        .with_context(|| format!("unpacking {url}"))?;
    if !staging.join(lib_in_archive).exists() {
        bail!("{url} has no {lib_in_archive}");
    }
    let renamed = std::fs::rename(&staging, &dir);
    let _ = std::fs::remove_dir_all(&staging);
    match renamed {
        Ok(()) => Ok(lib),
        Err(_) if lib.exists() => Ok(lib),
        Err(e) => Err(e).with_context(|| format!("installing pdfium into {}", dir.display())),
    }
}

fn platform_asset() -> Result<(&'static str, &'static str)> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => ("pdfium-linux-x64", "lib/libpdfium.so"),
        ("linux", "aarch64") => ("pdfium-linux-arm64", "lib/libpdfium.so"),
        ("macos", "aarch64") => ("pdfium-mac-arm64", "lib/libpdfium.dylib"),
        ("macos", "x86_64") => ("pdfium-mac-x64", "lib/libpdfium.dylib"),
        ("windows", "x86_64") => ("pdfium-win-x64", "bin/pdfium.dll"),
        ("windows", "aarch64") => ("pdfium-win-arm64", "bin/pdfium.dll"),
        (os, arch) => bail!("no prebuilt pdfium for {os}/{arch}"),
    })
}
