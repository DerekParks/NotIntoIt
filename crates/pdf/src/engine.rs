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

/// Where bblanchon/pdfium-binaries publishes each Chromium release's builds.
pub const RELEASES: &str = "https://github.com/bblanchon/pdfium-binaries/releases/download";

/// Path to the platform pdfium library, downloading it if absent.
pub fn install(data_dir: &Path) -> Result<PathBuf> {
    install_from(RELEASES, data_dir)
}

/// [`install`] from a mirror laid out like [`RELEASES`].
pub fn install_from(base: &str, data_dir: &Path) -> Result<PathBuf> {
    let (asset, lib_in_archive) = platform_asset()?;
    let dir = data_dir.join("pdfium").join(format!("chromium-{PDFIUM_RELEASE}"));
    let lib = dir.join(lib_in_archive);
    if lib.exists() {
        return Ok(lib);
    }

    let url = format!("{base}/chromium/{PDFIUM_RELEASE}/{asset}.tgz");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_library_is_found_without_downloading() {
        let dir = std::env::temp_dir().join(format!("nii-pdfium-cache-{}", std::process::id()));
        let (_, lib) = platform_asset().unwrap();
        let path = dir.join("pdfium").join(format!("chromium-{PDFIUM_RELEASE}")).join(lib);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"").unwrap();
        assert_eq!(install(&dir).unwrap(), path);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A .tgz holding `files` (archive path, contents), as pdfium-binaries publishes.
    fn tgz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for (path, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            archive.append_data(&mut header, path, *bytes).unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn installs_from_a_mirror_and_rejects_archives_without_the_library() {
        let (asset, lib) = platform_asset().unwrap();
        let mirror = nii_testkit::scratch("pdfium-mirror");
        let release = format!("chromium/{PDFIUM_RELEASE}/{asset}.tgz");
        nii_testkit::put(&mirror, &release, tgz(&[(lib, b"library"), ("LICENSE", b"BSD")]));
        let base = nii_testkit::serve(mirror.clone());

        let data = nii_testkit::scratch("pdfium-data");
        let installed = install_from(&base, &data).unwrap();
        assert_eq!(std::fs::read(&installed).unwrap(), b"library");
        assert_eq!(
            installed,
            data.join("pdfium").join(format!("chromium-{PDFIUM_RELEASE}")).join(lib)
        );
        assert_eq!(
            std::fs::read_dir(data.join("pdfium")).unwrap().count(),
            1,
            "staging dir left behind"
        );

        nii_testkit::put(&mirror, &release, tgz(&[("README", b"no library here")]));
        let empty = nii_testkit::scratch("pdfium-empty");
        let err = install_from(&base, &empty).unwrap_err().to_string();
        assert_eq!(err, format!("{base}/{release} has no {lib}"));

        let err = install_from("http://127.0.0.1:9", &nii_testkit::scratch("pdfium-down")).unwrap_err();
        assert!(
            format!("{err:#}").starts_with("downloading http://127.0.0.1:9/chromium/"),
            "{err:#}"
        );
    }

    #[test]
    fn this_platform_has_a_prebuilt_library() {
        let (asset, lib) = platform_asset().unwrap();
        assert!(asset.starts_with("pdfium-") && lib.contains("pdfium"));
    }
}
