//! # Self-update module
//!
//! Checks GitHub Releases for a newer version of portview and, on supported
//! platforms, downloads and replaces the running binary in-place.
//!
//! Auto-update is supported on:
//! - Windows (`.exe` asset)
//! - Linux when the binary was **not** installed via `dpkg` or `rpm`
//!   (`.tar.gz` asset)
//!
//! On package-managed Linux installs (deb/rpm) and unsupported platforms,
//! the command checks for updates and prints a manual download URL.
//!
//! HTTP requests are delegated to `curl` (ships with Windows 10+ and
//! virtually all Linux distributions) to avoid pulling in a TLS library
//! that would break cross-platform clippy checks.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use anyhow::{Context, Result, bail};

/// GitHub repository owner.
const REPO_OWNER: &str = "ehsan18t";
/// GitHub repository name.
const REPO_NAME: &str = "portview";

/// Run the update command.
///
/// When `check_only` is true, only checks for a newer version and prints
/// the result without downloading or replacing anything.
pub fn run(check_only: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    eprintln!("Current version: {current}");
    eprint!("Checking for updates... ");

    let release = fetch_latest_release().context("failed to check for updates")?;

    let remote = &release.tag_name;

    match compare_versions(current, remote) {
        Ordering::Less => {}
        Ordering::Equal | Ordering::Greater => {
            eprintln!("up to date.");
            eprintln!("portview is already up to date ({current}).");
            return Ok(());
        }
    }

    eprintln!("new version available!");
    eprintln!("New version: {remote} (current: {current})");

    if check_only {
        print_manual_download_info(&release);
        return Ok(());
    }

    let platform = detect_platform()?;

    match platform {
        Platform::WindowsExe => {
            let asset_name = format!("portview-{remote}-x86_64.exe");
            let asset = find_asset(&release, &asset_name)?;
            let binary_path = current_exe_path()?;
            download_and_replace_windows(&asset.browser_download_url, &binary_path)?;
            eprintln!("Updated portview: {current} -> {remote}");
        }
        Platform::LinuxTarGz => {
            let asset_name = format!("portview-{remote}-x86_64.tar.gz");
            let asset = find_asset(&release, &asset_name)?;
            let binary_path = current_exe_path()?;
            download_and_replace_linux_tar(&asset.browser_download_url, &binary_path)?;
            eprintln!("Updated portview: {current} -> {remote}");
        }
        Platform::LinuxDeb | Platform::LinuxRpm => {
            eprintln!();
            eprintln!("WARNING: Auto-update is not available for your installation method.");
            eprintln!(
                "Your binary appears to be managed by {}.",
                if platform == Platform::LinuxDeb {
                    "dpkg (Debian/Ubuntu)"
                } else {
                    "rpm (Fedora/RHEL)"
                }
            );
            eprintln!("Please update using your package manager, or download manually:");
            print_manual_download_info(&release);
        }
        Platform::Unsupported => {
            eprintln!();
            eprintln!("WARNING: Auto-update is not available on this platform.");
            eprintln!("Please download the new version manually:");
            print_manual_download_info(&release);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Platform detection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Variants are platform-conditional; all used across targets.
enum Platform {
    WindowsExe,
    LinuxTarGz,
    LinuxDeb,
    LinuxRpm,
    Unsupported,
}

fn detect_platform() -> Result<Platform> {
    if cfg!(target_os = "windows") && cfg!(target_arch = "x86_64") {
        return Ok(Platform::WindowsExe);
    }

    if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        let binary_path = current_exe_path()?;
        return Ok(detect_linux_install_method(&binary_path));
    }

    Ok(Platform::Unsupported)
}

#[cfg(target_os = "linux")]
fn detect_linux_install_method(binary_path: &Path) -> Platform {
    let path_str = binary_path.to_string_lossy();

    // Check dpkg first (Debian/Ubuntu)
    if let Ok(output) = ProcessCommand::new("dpkg")
        .args(["-S", &path_str])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        && output.success()
    {
        return Platform::LinuxDeb;
    }

    // Check rpm (Fedora/RHEL)
    if let Ok(output) = ProcessCommand::new("rpm")
        .args(["-qf", &path_str])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        && output.success()
    {
        return Platform::LinuxRpm;
    }

    Platform::LinuxTarGz
}

#[cfg(not(target_os = "linux"))]
const fn detect_linux_install_method(_binary_path: &Path) -> Platform {
    Platform::Unsupported
}

// ---------------------------------------------------------------------------
// GitHub API (via curl)
// ---------------------------------------------------------------------------

/// Minimal representation of a GitHub release.
struct Release {
    tag_name: String,
    html_url: String,
    assets: Vec<Asset>,
}

/// Minimal representation of a GitHub release asset.
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Execute curl and return stdout as a string.
///
/// Fails with a descriptive message if curl is not installed or exits
/// with a non-zero status.
fn curl_get_string(url: &str) -> Result<String> {
    let version = env!("CARGO_PKG_VERSION");
    let output = ProcessCommand::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--max-time",
            "30",
            "--header",
            "Accept: application/vnd.github+json",
            "--header",
            &format!("User-Agent: portview/{version}"),
            url,
        ])
        .output()
        .context(
            "failed to run curl. Is curl installed?\n  \
             On Windows 10+ curl ships with the OS.\n  \
             On Linux install it via your package manager (e.g. apt install curl).",
        )?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code().unwrap_or(-1);

        // curl exit code 22 = HTTP error (--fail flag)
        if code == 22 {
            if stderr.contains("403") || stderr.contains("429") {
                bail!("GitHub API rate limit reached. Try again later.\n  URL: {url}");
            }
            if stderr.contains("404") {
                bail!("No releases found for {REPO_OWNER}/{REPO_NAME}.\n  URL: {url}");
            }
            bail!("GitHub API returned an HTTP error.\n  URL: {url}\n  Detail: {stderr}");
        }

        bail!("curl failed (exit code {code}).\n  URL: {url}\n  Detail: {stderr}");
    }

    String::from_utf8(output.stdout).context("GitHub API response is not valid UTF-8")
}

/// Download a file to a local path using curl.
fn curl_download_file(url: &str, dest: &Path) -> Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    let output = ProcessCommand::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--max-time",
            "120",
            "--header",
            &format!("User-Agent: portview/{version}"),
            "--output",
        ])
        .arg(dest)
        .arg(url)
        .output()
        .context("failed to run curl for download")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code().unwrap_or(-1);
        bail!("Download failed (curl exit code {code}).\n  URL: {url}\n  Detail: {stderr}");
    }

    Ok(())
}

fn fetch_latest_release() -> Result<Release> {
    let url = format!("https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/releases/latest");
    let body = curl_get_string(&url)?;
    parse_release_json(&body)
}

fn parse_release_json(body: &str) -> Result<Release> {
    let value: serde_json::Value =
        serde_json::from_str(body).context("failed to parse GitHub release JSON")?;

    let tag_name = value["tag_name"]
        .as_str()
        .context("release JSON missing 'tag_name'")?
        .to_owned();

    let html_url = value["html_url"].as_str().unwrap_or("").to_owned();

    let assets = value["assets"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    Some(Asset {
                        name: a["name"].as_str()?.to_owned(),
                        browser_download_url: a["browser_download_url"].as_str()?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(Release {
        tag_name,
        html_url,
        assets,
    })
}

// ---------------------------------------------------------------------------
// Version comparison
// ---------------------------------------------------------------------------

/// Compare two semver-like version strings numerically.
///
/// Splits on `.` and compares each segment as a number. Returns
/// `Ordering::Less` when `current` is older than `remote`.
fn compare_versions(current: &str, remote: &str) -> Ordering {
    let parse = |v: &str| -> Vec<u64> {
        v.split('.')
            .map(|seg| seg.parse::<u64>().unwrap_or(0))
            .collect()
    };

    let c = parse(current);
    let r = parse(remote);

    let max_len = c.len().max(r.len());

    for i in 0..max_len {
        let cv = c.get(i).copied().unwrap_or(0);
        let rv = r.get(i).copied().unwrap_or(0);
        match cv.cmp(&rv) {
            Ordering::Equal => {}
            other => return other,
        }
    }

    Ordering::Equal
}

// ---------------------------------------------------------------------------
// Asset lookup
// ---------------------------------------------------------------------------

fn find_asset<'a>(release: &'a Release, expected_name: &str) -> Result<&'a Asset> {
    release
        .assets
        .iter()
        .find(|a| a.name == expected_name)
        .with_context(|| {
            format!(
                "No compatible binary '{expected_name}' found in release {}.\n\
                 Download manually from: {}",
                release.tag_name, release.html_url
            )
        })
}

fn current_exe_path() -> Result<PathBuf> {
    std::env::current_exe()
        .context("cannot determine current binary path")?
        .canonicalize()
        .context("cannot resolve canonical path for current binary")
}

/// Create a temporary file path next to the target binary.
fn temp_path_beside(binary_path: &Path, suffix: &str) -> Result<PathBuf> {
    let dir = binary_path
        .parent()
        .context("cannot determine parent directory of current binary")?;
    let file_name = format!(".portview-update-{}{suffix}", std::process::id());
    Ok(dir.join(file_name))
}

// ---------------------------------------------------------------------------
// Windows update
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn download_and_replace_windows(url: &str, binary_path: &Path) -> Result<()> {
    eprintln!("Downloading update...");

    let temp = temp_path_beside(binary_path, ".exe")?;
    let old = temp_path_beside(binary_path, ".old.exe")?;

    curl_download_file(url, &temp)?;

    // Sanity check: downloaded file should be a reasonable size
    let meta = std::fs::metadata(&temp)
        .with_context(|| format!("failed to read downloaded file: {}", temp.display()))?;
    if meta.len() < 1024 {
        drop(std::fs::remove_file(&temp));
        bail!(
            "Downloaded file is too small ({} bytes) — likely not a valid binary.",
            meta.len()
        );
    }

    // Rename current -> old, temp -> current
    // On Windows the running .exe can be renamed but not deleted.
    if old.exists() {
        drop(std::fs::remove_file(&old));
    }

    std::fs::rename(binary_path, &old).with_context(|| {
        format!(
            "Failed to rename current binary to backup.\n\
             Try running as Administrator.\n  Path: {}",
            binary_path.display()
        )
    })?;

    if let Err(e) = std::fs::rename(&temp, binary_path) {
        // Attempt to restore the old binary
        drop(std::fs::rename(&old, binary_path));
        return Err(e).with_context(|| {
            format!(
                "Failed to put new binary in place.\n  Path: {}",
                binary_path.display()
            )
        });
    }

    // Best-effort cleanup of old binary
    drop(std::fs::remove_file(&old));

    Ok(())
}

#[cfg(not(windows))]
fn download_and_replace_windows(_url: &str, _binary_path: &Path) -> Result<()> {
    bail!("Windows update is not available on this platform")
}

// ---------------------------------------------------------------------------
// Linux tar.gz update
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn download_and_replace_linux_tar(url: &str, binary_path: &Path) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    eprintln!("Downloading update...");

    let temp_archive = temp_path_beside(binary_path, ".tar.gz")?;
    let temp_binary = temp_path_beside(binary_path, "")?;

    curl_download_file(url, &temp_archive)?;

    // Sanity check download size
    let meta = std::fs::metadata(&temp_archive)
        .with_context(|| format!("failed to read downloaded file: {}", temp_archive.display()))?;
    if meta.len() < 1024 {
        drop(std::fs::remove_file(&temp_archive));
        bail!(
            "Downloaded file is too small ({} bytes) — likely not a valid archive.",
            meta.len()
        );
    }

    // Extract the `portview` binary from the tar.gz archive
    let file = std::fs::File::open(&temp_archive)
        .with_context(|| format!("failed to open archive: {}", temp_archive.display()))?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    let mut found = false;

    for entry_result in archive
        .entries()
        .context("failed to read tar archive entries")?
    {
        let mut entry = entry_result.context("failed to read tar entry")?;
        let entry_path = entry.path().context("failed to read tar entry path")?;

        // Look for the `portview` binary (may be at root or in a subdirectory)
        let file_name = entry_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        if file_name == "portview" {
            let mut out_file = std::fs::File::create(&temp_binary).with_context(|| {
                format!("failed to create temp file: {}", temp_binary.display())
            })?;
            std::io::copy(&mut entry, &mut out_file)
                .context("failed to extract portview binary from archive")?;
            out_file.flush().context("failed to flush temp file")?;

            // Set executable permission
            let permissions = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(&temp_binary, permissions)
                .context("failed to set executable permission on updated binary")?;

            found = true;
            break;
        }
    }

    // Clean up archive
    drop(std::fs::remove_file(&temp_archive));

    if !found {
        drop(std::fs::remove_file(&temp_binary));
        bail!("Archive does not contain a 'portview' binary. Download manually.");
    }

    // Atomic replace: rename temp over current binary
    std::fs::rename(&temp_binary, binary_path).with_context(|| {
        format!(
            "Failed to replace binary. Try running with sudo.\n  Path: {}",
            binary_path.display()
        )
    })?;

    Ok(())
}

#[cfg(not(unix))]
fn download_and_replace_linux_tar(_url: &str, _binary_path: &Path) -> Result<()> {
    bail!("Linux tar.gz update is not available on this platform")
}

// ---------------------------------------------------------------------------
// Display helpers
// ---------------------------------------------------------------------------

fn print_manual_download_info(release: &Release) {
    if !release.html_url.is_empty() {
        eprintln!("  Release page: {}", release.html_url);
    }
    if !release.assets.is_empty() {
        eprintln!("  Available assets:");
        for asset in &release.assets {
            eprintln!("    - {}: {}", asset.name, asset.browser_download_url);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compare_equal_versions() {
        assert_eq!(compare_versions("0.1.0", "0.1.0"), Ordering::Equal);
    }

    #[test]
    fn compare_current_older() {
        assert_eq!(compare_versions("0.1.0", "0.1.1"), Ordering::Less);
        assert_eq!(compare_versions("0.1.0", "0.2.0"), Ordering::Less);
        assert_eq!(compare_versions("0.1.0", "1.0.0"), Ordering::Less);
    }

    #[test]
    fn compare_current_newer() {
        assert_eq!(compare_versions("0.2.0", "0.1.0"), Ordering::Greater);
        assert_eq!(compare_versions("1.0.0", "0.9.9"), Ordering::Greater);
    }

    #[test]
    fn compare_different_length_versions() {
        assert_eq!(compare_versions("0.1", "0.1.0"), Ordering::Equal);
        assert_eq!(compare_versions("0.1", "0.1.1"), Ordering::Less);
        assert_eq!(compare_versions("0.1.1", "0.1"), Ordering::Greater);
    }

    #[test]
    fn compare_major_version_jump() {
        assert_eq!(compare_versions("0.9.9", "1.0.0"), Ordering::Less);
        assert_eq!(compare_versions("2.0.0", "1.99.99"), Ordering::Greater);
    }

    #[test]
    fn parse_valid_release_json() {
        let json = r#"{
            "tag_name": "0.2.0",
            "html_url": "https://github.com/ehsan18t/portview/releases/tag/0.2.0",
            "assets": [
                {
                    "name": "portview-0.2.0-x86_64.exe",
                    "browser_download_url": "https://github.com/ehsan18t/portview/releases/download/0.2.0/portview-0.2.0-x86_64.exe",
                    "size": 2048000
                },
                {
                    "name": "portview-0.2.0-x86_64.tar.gz",
                    "browser_download_url": "https://github.com/ehsan18t/portview/releases/download/0.2.0/portview-0.2.0-x86_64.tar.gz",
                    "size": 1024000
                }
            ]
        }"#;

        let release = parse_release_json(json).unwrap();
        assert_eq!(release.tag_name, "0.2.0");
        assert_eq!(release.assets.len(), 2);
        assert_eq!(release.assets[0].name, "portview-0.2.0-x86_64.exe");
        assert_eq!(release.assets[1].name, "portview-0.2.0-x86_64.tar.gz");
    }

    #[test]
    fn parse_release_json_missing_tag() {
        let json = r#"{"html_url": "https://example.com"}"#;
        assert!(parse_release_json(json).is_err());
    }

    #[test]
    fn parse_release_json_empty_assets() {
        let json = r#"{"tag_name": "0.1.0", "html_url": "", "assets": []}"#;
        let release = parse_release_json(json).unwrap();
        assert!(release.assets.is_empty());
    }

    #[test]
    fn parse_release_json_missing_assets_key() {
        let json = r#"{"tag_name": "0.1.0", "html_url": ""}"#;
        let release = parse_release_json(json).unwrap();
        assert!(release.assets.is_empty());
    }

    #[test]
    fn find_asset_matches_exact_name() {
        let release = Release {
            tag_name: "0.2.0".to_owned(),
            html_url: "https://example.com".to_owned(),
            assets: vec![
                Asset {
                    name: "portview-0.2.0-x86_64.exe".to_owned(),
                    browser_download_url: "https://example.com/exe".to_owned(),
                },
                Asset {
                    name: "portview-0.2.0-x86_64.tar.gz".to_owned(),
                    browser_download_url: "https://example.com/tar".to_owned(),
                },
            ],
        };

        let asset = find_asset(&release, "portview-0.2.0-x86_64.exe").unwrap();
        assert_eq!(asset.browser_download_url, "https://example.com/exe");
    }

    #[test]
    fn find_asset_missing_returns_error() {
        let release = Release {
            tag_name: "0.2.0".to_owned(),
            html_url: "https://example.com".to_owned(),
            assets: vec![],
        };

        assert!(find_asset(&release, "portview-0.2.0-x86_64.exe").is_err());
    }
}
