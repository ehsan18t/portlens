//! # Self-update module
//!
//! Checks GitHub Releases for a newer version of `PortLens` and, on supported
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
//!
//! Archive extraction on Linux is delegated to the system `tar` command
//! (part of coreutils on every Linux distribution), which avoids pulling
//! in the `tar` + `flate2` Rust crates and their transitive dependencies.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output};

use anyhow::{Context, Result, bail};
use log::debug;

/// GitHub repository owner.
const REPO_OWNER: &str = "ehsan18t";
/// GitHub repository name.
const REPO_NAME: &str = "portlens";

/// Run the update command.
///
/// When `check_only` is true, only checks for a newer version and prints
/// the result without downloading or replacing anything.
pub fn run(check_only: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    debug!("update check: current_version={current} check_only={check_only}");
    let Some(release) = check_for_update(current)? else {
        return Ok(());
    };

    let remote = &release.tag_name;

    if !is_update_available(current, remote) {
        print_up_to_date(current);
        return Ok(());
    }

    print_available_update(current, remote);

    if check_only {
        print_manual_download_info(&release);
        return Ok(());
    }

    install_update(&release, current, remote)
}

fn check_for_update(current: &str) -> Result<Option<Release>> {
    eprintln!("Current version: {current}");
    eprint!("Checking for updates... ");
    let release = fetch_latest_release().context("failed to check for updates")?;
    if release.is_none() {
        eprintln!("no published releases found.");
    }
    Ok(release)
}

fn is_update_available(current: &str, remote: &str) -> bool {
    let available = compare_versions(current, remote) == Ordering::Less;
    debug!("version comparison: current={current} remote={remote} update_available={available}");
    available
}

fn print_up_to_date(current: &str) {
    eprintln!("up to date.");
    eprintln!("PortLens is already up to date ({current}).");
}

fn print_available_update(current: &str, remote: &str) {
    eprintln!("new version available!");
    eprintln!("New version: {remote} (current: {current})");
}

fn install_update(release: &Release, current: &str, remote: &str) -> Result<()> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        install_release_asset(
            release,
            current,
            remote,
            "exe",
            download_and_replace_windows,
        )
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        match detect_linux_install_method(&current_exe_path()?) {
            LinuxInstallMethod::TarGz => install_release_asset(
                release,
                current,
                remote,
                "tar.gz",
                download_and_replace_linux_tar,
            ),
            LinuxInstallMethod::Deb => {
                notify_package_managed(release, "dpkg (Debian/Ubuntu)");
                Ok(())
            }
            LinuxInstallMethod::Rpm => {
                notify_package_managed(release, "rpm (Fedora/RHEL)");
                Ok(())
            }
        }
    }

    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )))]
    {
        notify_unsupported_platform(release);
        Ok(())
    }
}

fn install_release_asset(
    release: &Release,
    current: &str,
    remote: &str,
    ext: &str,
    install: fn(&Asset, &Path) -> Result<()>,
) -> Result<()> {
    apply_asset_update(release, remote, ext, install)?;
    eprintln!("Updated PortLens: {current} -> {remote}");
    Ok(())
}

#[cfg(not(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
)))]
fn notify_unsupported_platform(release: &Release) {
    eprintln!();
    eprintln!("WARNING: Auto-update is not available on this platform.");
    eprintln!("Please download the new version manually:");
    print_manual_download_info(release);
}

fn apply_asset_update(
    release: &Release,
    remote: &str,
    ext: &str,
    install: fn(&Asset, &Path) -> Result<()>,
) -> Result<()> {
    let asset = find_release_asset(release, remote, ext)?;
    let binary_path = current_exe_path()?;
    install(asset, &binary_path)
}

fn release_asset_name(tag_name: &str, ext: &str) -> String {
    let version = normalized_version_tag(tag_name);
    format!("portlens-{version}-x86_64.{ext}")
}

fn release_asset_candidates(tag_name: &str, ext: &str) -> Vec<String> {
    let normalized = release_asset_name(tag_name, ext);
    let raw = format!("portlens-{tag_name}-x86_64.{ext}");

    if raw == normalized {
        vec![normalized]
    } else {
        vec![normalized, raw]
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn notify_package_managed(release: &Release, manager: &str) {
    eprintln!();
    eprintln!("WARNING: Auto-update is not available for your installation method.");
    eprintln!("Your binary appears to be managed by {manager}.");
    eprintln!("Please update using your package manager, or download manually:");
    print_manual_download_info(release);
}

// ---------------------------------------------------------------------------
// Platform detection
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
enum LinuxInstallMethod {
    TarGz,
    Deb,
    Rpm,
}

#[cfg(target_os = "linux")]
fn detect_linux_install_method(binary_path: &Path) -> LinuxInstallMethod {
    let path_str = binary_path.to_string_lossy();

    if path_owned_by("dpkg", "-S", &path_str) {
        return LinuxInstallMethod::Deb;
    }
    if path_owned_by("rpm", "-qf", &path_str) {
        return LinuxInstallMethod::Rpm;
    }
    LinuxInstallMethod::TarGz
}

/// Return true if `tool` reports that it owns `path` (exit status 0).
#[cfg(target_os = "linux")]
fn path_owned_by(tool: &str, flag: &str, path: &str) -> bool {
    ProcessCommand::new(tool)
        .args([flag, path])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
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
    size_bytes: Option<u64>,
}

/// Execute curl and return stdout as a string.
///
/// Fails with a descriptive message if curl is not installed or exits
/// with a non-zero status.
fn curl_get_string(url: &str) -> Result<Option<String>> {
    let output = curl_api_command(url).output().context(
        "failed to run curl. Is curl installed?\n  \
             On Windows 10+ curl ships with the OS.\n  \
             On Linux install it via your package manager (e.g. apt install curl).",
    )?;

    if !output.status.success() {
        let (code, stderr) = curl_failure_parts(&output);
        return match classify_api_curl_failure(code, stderr.as_ref()) {
            ApiCurlFailure::NotFound => Ok(None),
            ApiCurlFailure::RateLimited => Err(anyhow::anyhow!(
                "GitHub API rate limit reached. Try again later.\n  URL: {url}"
            )),
            ApiCurlFailure::HttpError => Err(anyhow::anyhow!(
                "GitHub API returned an HTTP error.\n  URL: {url}\n  Detail: {stderr}"
            )),
            ApiCurlFailure::Transport => Err(anyhow::anyhow!(
                "curl failed (exit code {code}).\n  URL: {url}\n  Detail: {stderr}"
            )),
        };
    }

    String::from_utf8(output.stdout)
        .context("GitHub API response is not valid UTF-8")
        .map(Some)
}

/// Download a file to a local path using curl.
fn curl_download_file(url: &str, dest: &Path) -> Result<()> {
    let output = curl_download_command(url, dest)
        .output()
        .context("failed to run curl for download")?;

    if !output.status.success() {
        let (code, stderr) = curl_failure_parts(&output);
        bail!("Download failed (curl exit code {code}).\n  URL: {url}\n  Detail: {stderr}");
    }

    Ok(())
}

fn base_curl_command(timeout_seconds: &str) -> ProcessCommand {
    let version = env!("CARGO_PKG_VERSION");
    let mut command = ProcessCommand::new("curl");
    command
        .arg("--silent")
        .arg("--show-error")
        .arg("--fail")
        .arg("--location")
        .arg("--max-time")
        .arg(timeout_seconds)
        .arg("--header")
        .arg(format!("User-Agent: PortLens/{version}"));
    command
}

fn curl_api_command(url: &str) -> ProcessCommand {
    let mut command = base_curl_command("30");
    command
        .arg("--header")
        .arg("Accept: application/vnd.github+json")
        .arg(url);
    command
}

fn curl_download_command(url: &str, dest: &Path) -> ProcessCommand {
    let mut command = base_curl_command("120");
    command.arg("--output").arg(dest).arg(url);
    command
}

/// Extract the exit code and stderr text from a failed curl invocation.
fn curl_failure_parts(output: &Output) -> (i32, std::borrow::Cow<'_, str>) {
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr),
    )
}

/// Build a descriptive error from a failed GitHub API curl call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApiCurlFailure {
    Transport,
    RateLimited,
    NotFound,
    HttpError,
}

fn classify_api_curl_failure(code: i32, stderr: &str) -> ApiCurlFailure {
    if code != 22 {
        return ApiCurlFailure::Transport;
    }
    if stderr.contains("403") || stderr.contains("429") {
        return ApiCurlFailure::RateLimited;
    }
    if stderr.contains("404") {
        return ApiCurlFailure::NotFound;
    }
    ApiCurlFailure::HttpError
}

fn fetch_latest_release() -> Result<Option<Release>> {
    let url = format!("https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/releases/latest");
    let Some(body) = curl_get_string(&url)? else {
        return Ok(None);
    };
    parse_release_json(&body).map(Some)
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
                        size_bytes: a["size"].as_u64(),
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

/// Compare two semver-like version strings.
///
/// Strips a leading `v` / `V` prefix (common in GitHub release tags)
/// before parsing. Compares numeric `MAJOR.MINOR.PATCH` segments first,
/// then applies `SemVer` pre-release precedence (SS11): a version without
/// a pre-release tag has higher precedence than the same numeric triple
/// with one. Build metadata (after `+`) is ignored per SS10. Non-numeric
/// core segments fall back to `0` so malformed upstream tags sort
/// defensively.
fn compare_versions(current: &str, remote: &str) -> Ordering {
    fn split(v: &str) -> (Vec<u64>, Option<&str>) {
        let v = normalized_version_tag(v);
        // Strip build metadata (`+...`) first, then split core from pre-release.
        let v = v.split('+').next().unwrap_or(v);
        let (core, pre) = match v.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (v, None),
        };
        let nums = core
            .split('.')
            .map(|seg| seg.parse::<u64>().unwrap_or(0))
            .collect();
        (nums, pre)
    }

    let (c_nums, c_pre) = split(current);
    let (r_nums, r_pre) = split(remote);
    let len = c_nums.len().max(r_nums.len());

    let core_ordering = (0..len)
        .map(|i| {
            let cv = c_nums.get(i).copied().unwrap_or(0);
            let rv = r_nums.get(i).copied().unwrap_or(0);
            cv.cmp(&rv)
        })
        .find(|o| *o != Ordering::Equal)
        .unwrap_or(Ordering::Equal);

    if core_ordering != Ordering::Equal {
        return core_ordering;
    }

    // Same numeric core: a version WITH a pre-release is LESS than one without.
    match (c_pre, r_pre) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => compare_prerelease(a, b),
    }
}

fn normalized_version_tag(version: &str) -> &str {
    version
        .strip_prefix('v')
        .or_else(|| version.strip_prefix('V'))
        .unwrap_or(version)
}

/// Compare two `SemVer` pre-release strings (dot-separated identifiers).
///
/// Per `SemVer` §11.4: numeric identifiers compare numerically; alphanumeric
/// identifiers compare lexically in ASCII; numeric < alphanumeric; a shorter
/// list of identifiers is less than a longer one when all prior identifiers
/// are equal.
fn compare_prerelease(a: &str, b: &str) -> Ordering {
    let mut ai = a.split('.');
    let mut bi = b.split('.');
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(xn), Ok(yn)) => xn.cmp(&yn),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Asset lookup
// ---------------------------------------------------------------------------

fn find_release_asset<'a>(release: &'a Release, tag_name: &str, ext: &str) -> Result<&'a Asset> {
    let expected_names = release_asset_candidates(tag_name, ext);

    release
        .assets
        .iter()
        .find(|asset| expected_names.contains(&asset.name))
        .with_context(|| {
            format!(
                "No compatible binary ({}) found in release {}.\n\
                 Download manually from: {}",
                expected_names.join(", "),
                release.tag_name,
                release.html_url
            )
        })
}

/// Resolve the absolute, canonical path of the currently running binary.
///
/// `std::env::current_exe()` is the correct API here: it wraps
/// `GetModuleFileNameW` on Windows and reads `/proc/self/exe` on Linux,
/// which is exactly what any hand-rolled alternative would do. The path
/// is used only to locate the file we need to overwrite as part of the
/// self-update flow — it is never used as input to a security decision
/// (no authentication, authorization, trust check, or code/config load
/// keys off this value). The user explicitly invoked `portlens update`,
/// so replacing their own binary in place is the intended behavior.
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
    let file_name = format!(".portlens-update-{}{suffix}", std::process::id());
    Ok(dir.join(file_name))
}

// ---------------------------------------------------------------------------
// Windows update
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn download_and_replace_windows(asset: &Asset, binary_path: &Path) -> Result<()> {
    eprintln!("Downloading update...");

    let temp = temp_path_beside(binary_path, ".exe")?;
    let old = temp_path_beside(binary_path, ".old.exe")?;

    curl_download_file(&asset.browser_download_url, &temp)?;
    verify_download_size(&temp, asset.size_bytes, 1024, "binary")?;

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

// ---------------------------------------------------------------------------
// Linux tar.gz update
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn download_and_replace_linux_tar(asset: &Asset, binary_path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    eprintln!("Downloading update...");

    let temp_archive = temp_path_beside(binary_path, ".tar.gz")?;
    let extract_dir = temp_path_beside(binary_path, ".extract")?;

    download_archive(asset, &temp_archive)?;
    let temp_binary = extract_portlens_binary(&temp_archive, &extract_dir)?;

    // Set executable permission
    let permissions = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(&temp_binary, permissions)
        .context("failed to set executable permission on updated binary")?;

    let rename_result = replace_linux_binary(&temp_binary, binary_path);

    // Best-effort cleanup of extraction directory
    drop(std::fs::remove_dir_all(&extract_dir));

    rename_result
}

#[cfg(target_os = "linux")]
fn download_archive(asset: &Asset, archive_path: &Path) -> Result<()> {
    curl_download_file(&asset.browser_download_url, archive_path)?;
    verify_download_size(archive_path, asset.size_bytes, 1024, "archive")
}

#[cfg(target_os = "linux")]
fn extract_portlens_binary(archive_path: &Path, extract_dir: &Path) -> Result<PathBuf> {
    recreate_directory(extract_dir)?;

    let extraction_result = extract_archive_with_tar(archive_path, extract_dir).and_then(|()| {
        find_portlens_in_dir(extract_dir).with_context(|| {
            format!(
                "Archive does not contain a 'portlens' binary: {}",
                extract_dir.display()
            )
        })
    });

    drop(std::fs::remove_file(archive_path));
    if extraction_result.is_err() {
        drop(std::fs::remove_dir_all(extract_dir));
    }

    extraction_result
}

#[cfg(target_os = "linux")]
fn recreate_directory(path: &Path) -> Result<()> {
    if path.exists() {
        drop(std::fs::remove_dir_all(path));
    }

    std::fs::create_dir_all(path)
        .with_context(|| format!("failed to create extraction directory: {}", path.display()))
}

#[cfg(target_os = "linux")]
fn extract_archive_with_tar(archive_path: &Path, extract_dir: &Path) -> Result<()> {
    let status = ProcessCommand::new("tar")
        .arg("-xzf")
        .arg(archive_path)
        .arg("-C")
        .arg(extract_dir)
        .status()
        .context(
            "failed to run tar. Is tar installed?\n  \
             On Linux install it via your package manager (e.g. apt install tar).",
        )?;

    if !status.success() {
        bail!(
            "tar extraction failed (exit code {}).",
            status.code().unwrap_or(-1)
        );
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn replace_linux_binary(temp_binary: &Path, binary_path: &Path) -> Result<()> {
    std::fs::rename(temp_binary, binary_path).with_context(|| {
        format!(
            "Failed to replace binary. Try running with sudo.\n  Path: {}",
            binary_path.display()
        )
    })
}

/// Recursively search `dir` for a file named `portlens` and return its path.
#[cfg(target_os = "linux")]
fn find_portlens_in_dir(dir: &Path) -> Result<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = std::fs::read_dir(&current)
            .with_context(|| format!("failed to read directory: {}", current.display()))?;
        for entry in entries {
            let entry = entry.context("failed to read directory entry")?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("failed to stat: {}", path.display()))?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file()
                && path.file_name().and_then(|n| n.to_str()) == Some("portlens")
            {
                return Ok(path);
            }
        }
    }
    bail!("no 'portlens' binary found under {}", dir.display())
}

/// Verify a downloaded file matches the expected asset size when available.
///
/// Falls back to a minimum-size guard only when the upstream release metadata
/// omitted the asset size.
fn verify_download_size(
    path: &Path,
    expected_bytes: Option<u64>,
    min_bytes: u64,
    kind: &str,
) -> Result<()> {
    let meta = std::fs::metadata(path)
        .with_context(|| format!("failed to read downloaded file: {}", path.display()))?;

    if let Some(expected_bytes) = expected_bytes.filter(|size| *size > 0) {
        if meta.len() != expected_bytes {
            drop(std::fs::remove_file(path));
            bail!(
                "Downloaded {kind} size mismatch (expected {expected_bytes} bytes, got {} bytes).",
                meta.len()
            );
        }
        return Ok(());
    }

    if meta.len() < min_bytes {
        drop(std::fs::remove_file(path));
        bail!(
            "Downloaded file is too small ({} bytes) — likely not a valid {kind}.",
            meta.len()
        );
    }
    Ok(())
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
    use std::fs;

    use tempfile::TempDir;

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
    fn prerelease_is_less_than_release() {
        assert_eq!(compare_versions("1.0.0-rc1", "1.0.0"), Ordering::Less);
        assert_eq!(compare_versions("1.0.0", "1.0.0-rc1"), Ordering::Greater);
        assert_eq!(
            compare_versions("1.0.0-alpha", "1.0.0-beta"),
            Ordering::Less
        );
        assert_eq!(
            compare_versions("1.0.0-rc.2", "1.0.0-rc.10"),
            Ordering::Less
        );
        assert_eq!(
            compare_versions("1.0.0-alpha", "1.0.0-alpha.1"),
            Ordering::Less
        );
    }

    #[test]
    fn build_metadata_ignored() {
        assert_eq!(compare_versions("1.0.0+abc", "1.0.0+xyz"), Ordering::Equal);
        assert_eq!(
            compare_versions("1.0.0-rc1+abc", "1.0.0-rc1+xyz"),
            Ordering::Equal
        );
    }

    #[test]
    fn v_prefixed_tag_is_compared_correctly() {
        assert_eq!(
            compare_versions("0.1.0", "v0.2.0"),
            Ordering::Less,
            "v-prefixed remote should be parsed as 0.2.0"
        );
        assert_eq!(
            compare_versions("0.1.0", "v1.0.0"),
            Ordering::Less,
            "v-prefixed major bump should be detected"
        );
        assert_eq!(
            compare_versions("v1.0.0", "v1.0.0"),
            Ordering::Equal,
            "identical v-prefixed versions should be equal"
        );
        assert_eq!(
            compare_versions("1.0.0", "V1.0.0"),
            Ordering::Equal,
            "uppercase V prefix should be stripped"
        );
        assert_eq!(
            compare_versions("v1.0.0-rc1", "v1.0.0"),
            Ordering::Less,
            "v-prefixed pre-release should be less than release"
        );
    }

    #[test]
    fn api_curl_failure_detects_missing_releases() {
        assert_eq!(
            classify_api_curl_failure(22, "curl: (22) The requested URL returned error: 404"),
            ApiCurlFailure::NotFound,
            "404 release lookups should be treated as an empty releases state"
        );
    }

    #[test]
    fn api_curl_failure_detects_rate_limits() {
        assert_eq!(
            classify_api_curl_failure(22, "curl: (22) The requested URL returned error: 403"),
            ApiCurlFailure::RateLimited,
            "403 API failures should be reported as rate limiting"
        );
        assert_eq!(
            classify_api_curl_failure(22, "curl: (22) The requested URL returned error: 429"),
            ApiCurlFailure::RateLimited,
            "429 API failures should be reported as rate limiting"
        );
    }

    #[test]
    fn release_asset_name_strips_v_prefix() {
        assert_eq!(
            release_asset_name("v0.2.0", "exe"),
            "portlens-0.2.0-x86_64.exe",
            "asset lookup should normalize leading v prefixes in release tags"
        );
        assert_eq!(
            release_asset_name("V0.2.0", "tar.gz"),
            "portlens-0.2.0-x86_64.tar.gz",
            "asset lookup should normalize uppercase V prefixes in release tags"
        );
    }

    #[test]
    fn release_asset_candidates_accept_normalized_and_tagged_names() {
        assert_eq!(
            release_asset_candidates("v0.2.0", "exe"),
            vec![
                "portlens-0.2.0-x86_64.exe".to_string(),
                "portlens-v0.2.0-x86_64.exe".to_string(),
            ],
            "updater should support both normalized and legacy v-prefixed asset names"
        );
        assert_eq!(
            release_asset_candidates("0.2.0", "tar.gz"),
            vec!["portlens-0.2.0-x86_64.tar.gz".to_string()],
            "non-prefixed tags should only produce one candidate name"
        );
    }

    #[test]
    fn core_takes_precedence_over_prerelease() {
        assert_eq!(compare_versions("1.0.0-rc1", "0.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("0.9.9", "1.0.0-rc1"), Ordering::Less);
    }

    #[test]
    fn parse_valid_release_json() {
        let json = r#"{
            "tag_name": "0.2.0",
            "html_url": "https://github.com/ehsan18t/portlens/releases/tag/0.2.0",
            "assets": [
                {
                    "name": "portlens-0.2.0-x86_64.exe",
                    "browser_download_url": "https://github.com/ehsan18t/portlens/releases/download/0.2.0/portlens-0.2.0-x86_64.exe",
                    "size": 2048000
                },
                {
                    "name": "portlens-0.2.0-x86_64.tar.gz",
                    "browser_download_url": "https://github.com/ehsan18t/portlens/releases/download/0.2.0/portlens-0.2.0-x86_64.tar.gz",
                    "size": 1024000
                }
            ]
        }"#;

        let release = parse_release_json(json).unwrap();
        assert_eq!(release.tag_name, "0.2.0");
        assert_eq!(release.assets.len(), 2);
        assert_eq!(release.assets[0].name, "portlens-0.2.0-x86_64.exe");
        assert_eq!(release.assets[1].name, "portlens-0.2.0-x86_64.tar.gz");
        assert_eq!(release.assets[0].size_bytes, Some(2_048_000));
        assert_eq!(release.assets[1].size_bytes, Some(1_024_000));
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
    fn find_release_asset_matches_exact_name() {
        let release = Release {
            tag_name: "0.2.0".to_owned(),
            html_url: "https://example.com".to_owned(),
            assets: vec![
                Asset {
                    name: "portlens-0.2.0-x86_64.exe".to_owned(),
                    browser_download_url: "https://example.com/exe".to_owned(),
                    size_bytes: Some(1234),
                },
                Asset {
                    name: "portlens-0.2.0-x86_64.tar.gz".to_owned(),
                    browser_download_url: "https://example.com/tar".to_owned(),
                    size_bytes: Some(5678),
                },
            ],
        };

        let asset = find_release_asset(&release, "0.2.0", "exe").unwrap();
        assert_eq!(asset.browser_download_url, "https://example.com/exe");
        assert_eq!(asset.size_bytes, Some(1234));
    }

    #[test]
    fn find_release_asset_matches_legacy_tagged_name() {
        let release = Release {
            tag_name: "v0.2.0".to_owned(),
            html_url: "https://example.com".to_owned(),
            assets: vec![Asset {
                name: "portlens-v0.2.0-x86_64.exe".to_owned(),
                browser_download_url: "https://example.com/exe".to_owned(),
                size_bytes: Some(1234),
            }],
        };

        let asset = find_release_asset(&release, "v0.2.0", "exe").unwrap();
        assert_eq!(asset.browser_download_url, "https://example.com/exe");
    }

    #[test]
    fn find_release_asset_missing_returns_error() {
        let release = Release {
            tag_name: "0.2.0".to_owned(),
            html_url: "https://example.com".to_owned(),
            assets: vec![],
        };

        assert!(find_release_asset(&release, "0.2.0", "exe").is_err());
    }

    #[test]
    fn verify_download_size_accepts_exact_asset_size() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("portlens.exe");
        fs::write(&file_path, [0_u8; 8]).unwrap();

        verify_download_size(&file_path, Some(8), 1024, "binary")
            .expect("exact asset size should pass verification");
    }

    #[test]
    fn verify_download_size_rejects_mismatched_asset_size() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("portlens.exe");
        fs::write(&file_path, [0_u8; 8]).unwrap();

        let error = verify_download_size(&file_path, Some(9), 1024, "binary")
            .expect_err("mismatched asset sizes should be rejected");

        assert!(
            error.to_string().contains("size mismatch"),
            "verification errors should explain the size mismatch"
        );
        assert!(
            !file_path.exists(),
            "failed size verification should remove the downloaded file"
        );
    }

    #[test]
    fn verify_download_size_falls_back_to_minimum_size_without_metadata() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("portlens.exe");
        fs::write(&file_path, [0_u8; 16]).unwrap();

        let error = verify_download_size(&file_path, None, 1024, "binary")
            .expect_err("missing metadata should still honor the minimum-size guard");

        assert!(
            error.to_string().contains("too small"),
            "fallback verification should keep the existing minimum-size message"
        );
        assert!(
            !file_path.exists(),
            "failed minimum-size verification should remove the downloaded file"
        );
    }
}
