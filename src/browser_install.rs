use std::fs::File;
use std::io::{copy, Cursor};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;
use zip::ZipArchive;

use crate::cdp::{
    list_detected_browsers, playwright_cache_dir, playwright_chromium_binary_in_dir,
    playwright_chromium_layout,
};

const BROWSERS_JSON_URL: &str =
    "https://raw.githubusercontent.com/microsoft/playwright/main/packages/playwright-core/browsers.json";
const FALLBACK_CHROMIUM_REVISION: &str = "1229";

const CDN_URLS: &[&str] = &[
    "https://cdn.playwright.dev/dbazure/download/playwright/builds/chromium/{revision}/{archive}.zip",
    "https://cdn.playwright.dev/builds/chromium/{revision}/{archive}.zip",
    "https://playwright.azureedge.net/builds/chromium/{revision}/{archive}.zip",
];

/// Install a Chromium build suitable for CDP token capture.
/// Tries `npx playwright install chromium` first, then downloads from Playwright CDN.
pub fn install_chromium_browser(progress: &dyn Fn(&str)) -> Result<PathBuf, String> {
    if let Some(path) = existing_playwright_chromium() {
        progress(&format!("Chromium already installed at {}", path.display()));
        return Ok(path);
    }

    progress("Installing Chromium for token capture...");
    if try_npx_playwright_install(progress) {
        if let Some(path) = existing_playwright_chromium() {
            progress(&format!("Installed at {}", path.display()));
            return Ok(path);
        }
    }

    progress("Downloading Chromium from Playwright CDN (this may take a few minutes)...");
    native_playwright_chromium_install(progress)
}

fn existing_playwright_chromium() -> Option<PathBuf> {
    list_detected_browsers()
        .into_iter()
        .find(|b| b.path.to_string_lossy().contains("ms-playwright"))
        .map(|b| b.path)
}

fn try_npx_playwright_install(progress: &dyn Fn(&str)) -> bool {
    if !command_on_path("npx") {
        progress("npx not found; using built-in downloader");
        return false;
    }
    progress("Trying: npx playwright install chromium");
    Command::new("npx")
        .args(["--yes", "playwright", "install", "chromium"])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn native_playwright_chromium_install(progress: &dyn Fn(&str)) -> Result<PathBuf, String> {
    let revision = fetch_chromium_revision().unwrap_or_else(|_| {
        progress(&format!(
            "Could not fetch latest revision; using Chromium {FALLBACK_CHROMIUM_REVISION}"
        ));
        FALLBACK_CHROMIUM_REVISION.to_string()
    });
    let layout = playwright_chromium_layout()?;
    let archive = layout.download_archive;
    let cache = playwright_cache_dir().ok_or("could not resolve browser cache directory")?;
    std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;

    let install_dir = cache.join(format!("chromium-{revision}"));
    if let Some(path) = playwright_chromium_binary_in_dir(&install_dir) {
        return Ok(path);
    }

    let zip_bytes = download_chromium_archive(&revision, archive, progress)?;
    progress("Extracting Chromium...");
    extract_chromium_zip(&zip_bytes, &install_dir)?;

    playwright_chromium_binary_in_dir(&install_dir).ok_or_else(|| {
        format!(
            "download finished but browser binary not found under {}",
            install_dir.display()
        )
    })
}

fn fetch_chromium_revision() -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let json: Value = client
        .get(BROWSERS_JSON_URL)
        .send()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    json.get("browsers")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .find(|b| b.get("name").and_then(|n| n.as_str()) == Some("chromium"))
        .and_then(|b| b.get("revision"))
        .and_then(|r| r.as_u64())
        .map(|r| r.to_string())
        .ok_or_else(|| "chromium revision not found in browsers.json".into())
}

fn download_chromium_archive(
    revision: &str,
    archive: &str,
    progress: &dyn Fn(&str),
) -> Result<Vec<u8>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())?;

    let mut last_err = String::new();
    for template in CDN_URLS {
        let url = template
            .replace("{revision}", revision)
            .replace("{archive}", archive);
        progress(&format!("Downloading from {url}"));
        match client.get(&url).send() {
            Ok(resp) if resp.status().is_success() => {
                return resp.bytes().map(|b| b.to_vec()).map_err(|e| e.to_string());
            }
            Ok(resp) => {
                last_err = format!("HTTP {}", resp.status());
            }
            Err(e) => {
                last_err = e.to_string();
            }
        }
    }
    Err(format!("Chromium download failed: {last_err}"))
}

fn extract_chromium_zip(data: &[u8], dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let reader = Cursor::new(data);
    let mut archive = ZipArchive::new(reader).map_err(|e| e.to_string())?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        let outpath = match file.enclosed_name() {
            Some(path) => dest.join(path),
            None => continue,
        };
        if file.name().ends_with('/') {
            std::fs::create_dir_all(&outpath).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = outpath.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut outfile = File::create(&outpath).map_err(|e| e.to_string())?;
        copy(&mut file, &mut outfile).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = file.unix_mode() {
                let _ = std::fs::set_permissions(&outpath, std::fs::Permissions::from_mode(mode));
            }
            let _ = outfile.flush();
        }
    }
    Ok(())
}

fn command_on_path(program: &str) -> bool {
    #[cfg(windows)]
    {
        Command::new("where")
            .arg(program)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        Command::new("which")
            .arg(program)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdp::playwright_chromium_layout;
    use std::io::Write;
    use zip::write::ZipWriter;

    #[test]
    fn playwright_layout_maps_archive_to_extracted_paths() {
        let layout = playwright_chromium_layout().expect("platform layout");
        assert!(
            layout.download_archive.starts_with("chromium-"),
            "CDN zip uses chromium-* prefix: {}",
            layout.download_archive
        );
        assert!(
            !layout.binary_relative_paths.is_empty(),
            "expected at least one binary path"
        );
        for path in layout.binary_relative_paths {
            assert!(
                path.starts_with("chrome-"),
                "extracted folder uses chrome-* prefix, not {path}"
            );
        }
    }

    #[test]
    fn extract_zip_matches_playwright_layout() {
        let dir = tempfile::tempdir().unwrap();
        let install_dir = dir.path().join("chromium-9999");
        let relative = playwright_chromium_layout()
            .expect("platform layout")
            .binary_relative_paths[0];
        let relative = PathBuf::from(relative);
        let binary_path = install_dir.join(&relative);

        let mut zip_data = Vec::new();
        {
            let cursor = Cursor::new(&mut zip_data);
            let mut zip = ZipWriter::new(cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            let dir_name = relative
                .parent()
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
                + "/";
            zip.add_directory(dir_name, options).unwrap();
            zip.start_file(relative.to_string_lossy(), options).unwrap();
            zip.write_all(b"fake").unwrap();
            let _ = zip.finish();
        }

        extract_chromium_zip(&zip_data, &install_dir).unwrap();
        assert!(binary_path.exists());
    }
}
