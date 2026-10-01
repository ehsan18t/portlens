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
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use log::debug;
use sha2::{Digest, Sha256};

/// GitHub repository owner.
const REPO_OWNER: &str = "ehsan18t";
/// GitHub repository name.
const REPO_NAME: &str = "portlens";
/// Name of the checksum manifest published with every release.
const CHECKSUMS_ASSET_NAME: &str = "SHA256SUMS";
/// Upper bound (bytes) accepted for the checksum manifest download.
const CHECKSUMS_MAX_BYTES: &str = "65536";
/// File-name prefix for every temporary file the updater creates.
const UPDATE_TEMP_PREFIX: &str = ".portlens-update-";
/// Suffixes appended after `{UPDATE_TEMP_PREFIX}{pid}` by the updater.
const UPDATE_TEMP_SUFFIXES: &[&str] = &[".exe", ".old.exe", ".tar.gz", ".extract"];
/// Maximum time the downloaded binary may take to answer `--version`.
const SMOKE_TEST_TIMEOUT: Duration = Duration::from_secs(10);

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

/// A release asset selected for installation, plus everything needed to
/// verify it before it replaces the running binary.
struct UpdateTarget<'a> {
    asset: &'a Asset,
    /// Lowercase hex SHA-256 digest published in the release's `SHA256SUMS`.
    sha256: String,
    /// Version (without a leading `v`) the new binary must report.
    version: &'a str,
}

/// Signature shared by the per-platform download-and-replace routines.
type InstallFn = fn(&UpdateTarget<'_>, &Path) -> Result<()>;

fn install_release_asset(
    release: &Release,
    current: &str,
    remote: &str,
    ext: &str,
    install: InstallFn,
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
    install: InstallFn,
) -> Result<()> {
    let binary_path = current_exe_path()?;
    if let Some(dir) = binary_path.parent() {
        cleanup_stale_update_artifacts(dir);
    }

    let asset = find_release_asset(release, remote, ext)?;
    ensure_release_asset_url(release, asset)?;
    let sha256 = fetch_expected_checksum(release, &asset.name)?;
    let target = UpdateTarget {
        asset,
        sha256,
        version: normalized_version_tag(remote),
    };
    install(&target, &binary_path)
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
    ensure_allowed_url(url, UrlKind::Api)?;
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
    ensure_allowed_url(url, UrlKind::Download)?;
    let output = curl_download_command(url, dest)
        .output()
        .context("failed to run curl for download")?;

    if !output.status.success() {
        let (code, stderr) = curl_failure_parts(&output);
        bail!("Download failed (curl exit code {code}).\n  URL: {url}\n  Detail: {stderr}");
    }

    Ok(())
}

/// Download a small text release asset (the checksum manifest) into memory.
fn curl_download_text(url: &str) -> Result<String> {
    ensure_allowed_url(url, UrlKind::Download)?;
    let mut command = base_curl_command("30");
    command
        .arg("--max-filesize")
        .arg(CHECKSUMS_MAX_BYTES)
        .arg(url);
    let output = command
        .output()
        .context("failed to run curl for download")?;

    if !output.status.success() {
        let (code, stderr) = curl_failure_parts(&output);
        bail!("Download failed (curl exit code {code}).\n  URL: {url}\n  Detail: {stderr}");
    }

    String::from_utf8(output.stdout).with_context(|| format!("{url} is not valid UTF-8 text"))
}

/// Build the shared curl invocation.
///
/// Every request and every redirect hop is restricted to HTTPS with TLS 1.2
/// or newer, so a compromised redirect cannot downgrade the transfer to
/// plain HTTP. URL globbing is disabled because URLs come from API data.
fn base_curl_command(timeout_seconds: &str) -> ProcessCommand {
    let version = env!("CARGO_PKG_VERSION");
    let mut command = ProcessCommand::new(curl_program());
    command
        .arg("--silent")
        .arg("--show-error")
        .arg("--fail")
        .arg("--location")
        .arg("--max-redirs")
        .arg("5")
        .arg("--proto")
        .arg("=https")
        .arg("--proto-redir")
        .arg("=https")
        .arg("--tlsv1.2")
        .arg("--globoff")
        .arg("--max-time")
        .arg(timeout_seconds)
        .arg("--header")
        .arg(format!("User-Agent: PortLens/{version}"));
    command
}

/// Resolve the curl executable to run.
///
/// On Windows a bare `curl` is resolved by searching the application
/// directory first, so a `curl.exe` planted next to `portlens.exe` would be
/// picked up. Prefer the copy that ships in `%SystemRoot%\System32` and fall
/// back to a `PATH` lookup only when it is absent.
fn curl_program() -> PathBuf {
    system_curl_path().unwrap_or_else(|| PathBuf::from("curl"))
}

#[cfg(windows)]
fn system_curl_path() -> Option<PathBuf> {
    let root = std::env::var_os("SystemRoot")?;
    let candidate = PathBuf::from(root).join("System32").join("curl.exe");
    candidate.is_file().then_some(candidate)
}

#[cfg(not(windows))]
const fn system_curl_path() -> Option<PathBuf> {
    None
}

/// Which kind of GitHub endpoint a URL is expected to target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UrlKind {
    /// GitHub REST API calls for this repository.
    Api,
    /// Release asset downloads for this repository.
    Download,
}

/// Return the only URL prefix accepted for `kind`.
///
/// Release asset `browser_download_url` values always point at
/// `github.com/{owner}/{repo}/releases/download/`; GitHub then redirects to
/// its asset CDN, and those hops are constrained to HTTPS by curl's
/// `--proto-redir` rather than by this allow-list.
fn allowed_url_prefix(kind: UrlKind) -> String {
    match kind {
        UrlKind::Api => format!("https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/"),
        UrlKind::Download => {
            format!("https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/download/")
        }
    }
}

/// Return true when `url` is an HTTPS URL under the expected GitHub prefix.
///
/// Rejects whitespace, control and non-ASCII characters, backslashes,
/// query strings, fragments, and dot segments (literal or percent-encoded)
/// that curl would normalize into a different path.
fn is_allowed_url(url: &str, kind: UrlKind) -> bool {
    let prefix = allowed_url_prefix(kind);
    if !url.bytes().all(|b| b.is_ascii_graphic()) || url.contains(['\\', '?', '#']) {
        return false;
    }
    let Some(head) = url.get(..prefix.len()) else {
        return false;
    };
    if !head.eq_ignore_ascii_case(&prefix) {
        return false;
    }
    let rest = &url[prefix.len()..];
    !rest.is_empty()
        && !rest.to_ascii_lowercase().contains("%2e")
        && rest
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

fn ensure_allowed_url(url: &str, kind: UrlKind) -> Result<()> {
    if is_allowed_url(url, kind) {
        return Ok(());
    }
    bail!(
        "Refusing to contact an unexpected URL: {url}\n  Expected a URL starting with {}",
        allowed_url_prefix(kind)
    )
}

/// The only download URL accepted for `asset_name` in the release `tag_name`.
fn expected_asset_url(tag_name: &str, asset_name: &str) -> String {
    format!(
        "{}{tag_name}/{asset_name}",
        allowed_url_prefix(UrlKind::Download)
    )
}

/// Require `asset` to download from this repository's release URL for the
/// exact release being installed, so API data cannot point the updater at a
/// file attached to a different (older or unrelated) release.
fn ensure_release_asset_url(release: &Release, asset: &Asset) -> Result<()> {
    let expected = expected_asset_url(&release.tag_name, &asset.name);
    if asset.browser_download_url == expected && is_allowed_url(&expected, UrlKind::Download) {
        return Ok(());
    }
    bail!(
        "Refusing to download {} from an unexpected URL: {}\n  Expected exactly {expected}",
        asset.name,
        asset.browser_download_url
    )
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
/// self-update flow; it is never used as input to a security decision
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
    let file_name = format!("{UPDATE_TEMP_PREFIX}{}{suffix}", std::process::id());
    Ok(dir.join(file_name))
}

/// Return true when `file_name` is an updater temporary file left behind by
/// a different process (a previous run, crash, or locked Windows backup).
///
/// Only exact `{UPDATE_TEMP_PREFIX}{digits}{known suffix}` names match, so
/// unrelated files in the binary directory are never touched.
fn is_stale_update_artifact(file_name: &str, current_pid: u32) -> bool {
    let Some(rest) = file_name.strip_prefix(UPDATE_TEMP_PREFIX) else {
        return false;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return false;
    }
    let (pid, suffix) = rest.split_at(digits);
    UPDATE_TEMP_SUFFIXES.contains(&suffix) && pid.parse::<u32>().ok() != Some(current_pid)
}

/// Best-effort removal of stale updater files in `dir`.
///
/// On Windows the previous binary is renamed to a `.old.exe` backup that
/// cannot be deleted while it is still running, so it is cleaned up by the
/// next update instead. Errors are ignored: a file that is still locked is
/// simply retried on a later run.
fn cleanup_stale_update_artifacts(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let pid = std::process::id();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_stale_update_artifact(name, pid) {
            continue;
        }
        let path = entry.path();
        debug!("removing stale update artifact: {}", path.display());
        // DirEntry::file_type does not follow symlinks, so a symlink to a
        // directory is removed as a link and its target is left alone.
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            drop(std::fs::remove_dir_all(&path));
        } else if std::fs::remove_file(&path).is_err() {
            drop(std::fs::remove_dir(&path));
        }
    }
}

// ---------------------------------------------------------------------------
// Integrity verification
// ---------------------------------------------------------------------------

/// Download the release's `SHA256SUMS` manifest and return the digest listed
/// for `asset_name`.
///
/// Fails closed: a release without a manifest, or a manifest that does not
/// list the asset, is treated as unverifiable and nothing is installed.
fn fetch_expected_checksum(release: &Release, asset_name: &str) -> Result<String> {
    let manifest = release
        .assets
        .iter()
        .find(|asset| asset.name == CHECKSUMS_ASSET_NAME)
        .with_context(|| {
            format!(
                "Release {} does not publish a {CHECKSUMS_ASSET_NAME} checksum file, so the \
                 download cannot be verified. Refusing to install.\n\
                 Download manually from: {}",
                release.tag_name, release.html_url
            )
        })?;

    ensure_release_asset_url(release, manifest)?;
    let body = curl_download_text(&manifest.browser_download_url)
        .with_context(|| format!("failed to download {CHECKSUMS_ASSET_NAME}"))?;
    checksum_for_asset(&body, asset_name).with_context(|| {
        format!(
            "Cannot verify {asset_name} for release {}. Refusing to install.\n\
             Download manually from: {}",
            release.tag_name, release.html_url
        )
    })
}

/// Parse one `sha256sum`-style line: `<64 hex>  <name>` (text mode) or
/// `<64 hex> *<name>` (binary mode).
fn parse_checksum_line(line: &str) -> Option<(&str, &str)> {
    let (hash, rest) = line.split_at_checked(64)?;
    if !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let name = rest
        .strip_prefix("  ")
        .or_else(|| rest.strip_prefix(" *"))?;
    if name.is_empty() || name.trim() != name {
        return None;
    }
    Some((hash, name))
}

/// Look up the lowercase hex digest for `asset_name` in a `SHA256SUMS` body.
///
/// Blank lines are skipped; any other malformed line, a missing entry, or
/// two conflicting entries for the same asset is an error.
fn checksum_for_asset(manifest: &str, asset_name: &str) -> Result<String> {
    let manifest = manifest.strip_prefix('\u{feff}').unwrap_or(manifest);
    let mut found: Option<&str> = None;

    for (index, line) in manifest.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let Some((hash, name)) = parse_checksum_line(line) else {
            bail!("{CHECKSUMS_ASSET_NAME} line {} is malformed", index + 1);
        };
        if name != asset_name {
            continue;
        }
        if found.is_some_and(|existing| !existing.eq_ignore_ascii_case(hash)) {
            bail!("{CHECKSUMS_ASSET_NAME} lists conflicting digests for {asset_name}");
        }
        found = Some(hash);
    }

    found
        .map(str::to_ascii_lowercase)
        .with_context(|| format!("{CHECKSUMS_ASSET_NAME} has no entry for {asset_name}"))
}

/// Compare two hex SHA-256 digests, ignoring ASCII case.
const fn sha256_hex_matches(expected: &str, actual: &str) -> bool {
    expected.len() == 64 && expected.eq_ignore_ascii_case(actual)
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

/// Compute the lowercase hex SHA-256 digest of the file at `path`.
fn sha256_file_hex(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("failed to open downloaded file: {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read downloaded file: {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

/// Verify the file at `path` hashes to `expected`, deleting it on mismatch.
fn verify_file_sha256(path: &Path, expected: &str, kind: &str) -> Result<()> {
    let actual = sha256_file_hex(path)?;
    if sha256_hex_matches(expected, &actual) {
        debug!("sha256 verified for {}: {actual}", path.display());
        return Ok(());
    }
    drop(std::fs::remove_file(path));
    bail!(
        "Downloaded {kind} failed checksum verification. Refusing to install.\n  \
         Expected SHA-256: {expected}\n  Actual SHA-256:   {actual}"
    )
}

/// Download the target asset to `dest` and verify its size and SHA-256.
fn download_verified_asset(target: &UpdateTarget<'_>, dest: &Path, kind: &str) -> Result<()> {
    curl_download_file(&target.asset.browser_download_url, dest)?;
    verify_download_size(dest, target.asset.size_bytes, 1024, kind)?;
    verify_file_sha256(dest, &target.sha256, kind)
}

// ---------------------------------------------------------------------------
// Smoke test
// ---------------------------------------------------------------------------

/// Run the downloaded binary with `--version` and require it to report
/// `expected_version` before it is allowed to replace the current binary.
fn smoke_test_binary(path: &Path, expected_version: &str) -> Result<()> {
    let output = run_version_probe(path)?;
    if version_output_matches(&output, expected_version) {
        return Ok(());
    }
    bail!(
        "Downloaded binary did not report the expected version {expected_version}. \
         Refusing to install.\n  Output: {}",
        output.trim()
    )
}

/// Return true when any whitespace-separated token of `output` equals
/// `expected` once a leading `v` / `V` is ignored on both sides.
fn version_output_matches(output: &str, expected: &str) -> bool {
    let expected = normalized_version_tag(expected);
    !expected.is_empty()
        && output
            .split_whitespace()
            .any(|token| normalized_version_tag(token) == expected)
}

/// Spawn `path --version` and collect its stdout, killing it on timeout.
fn run_version_probe(path: &Path) -> Result<String> {
    let mut child = ProcessCommand::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to run downloaded binary: {}", path.display()))?;

    let deadline = Instant::now() + SMOKE_TEST_TIMEOUT;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .context("failed to wait for downloaded binary")?
        {
            break status;
        }
        if Instant::now() >= deadline {
            drop(child.kill());
            drop(child.wait());
            bail!(
                "Downloaded binary did not answer --version within {} seconds. \
                 Refusing to install.",
                SMOKE_TEST_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    };

    let mut stdout = Vec::new();
    if let Some(pipe) = child.stdout.take() {
        drop(pipe.take(4096).read_to_end(&mut stdout));
    }
    if !status.success() {
        bail!(
            "Downloaded binary failed to run --version (exit code {}). Refusing to install.",
            status.code().unwrap_or(-1)
        );
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

// ---------------------------------------------------------------------------
// Windows update
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn download_and_replace_windows(target: &UpdateTarget<'_>, binary_path: &Path) -> Result<()> {
    eprintln!("Downloading update...");

    let temp = temp_path_beside(binary_path, ".exe")?;
    let old = temp_path_beside(binary_path, ".old.exe")?;

    // Verify integrity and run the smoke test before touching the current
    // binary; any failure removes the partial download.
    let staged = download_verified_asset(target, &temp, "binary")
        .and_then(|()| smoke_test_binary(&temp, target.version));
    if let Err(error) = staged {
        drop(std::fs::remove_file(&temp));
        return Err(error);
    }

    swap_windows_binary(&temp, &old, binary_path)
}

#[cfg(windows)]
fn swap_windows_binary(temp: &Path, old: &Path, binary_path: &Path) -> Result<()> {
    // Rename current -> old, temp -> current
    // On Windows the running .exe can be renamed but not deleted.
    if old.exists() {
        drop(std::fs::remove_file(old));
    }

    if let Err(e) = std::fs::rename(binary_path, old) {
        drop(std::fs::remove_file(temp));
        return Err(e).with_context(|| {
            format!(
                "Failed to rename current binary to backup.
                 Try running as Administrator.
  Path: {}",
                binary_path.display()
            )
        });
    }

    if let Err(e) = std::fs::rename(temp, binary_path) {
        // Attempt to restore the old binary
        drop(std::fs::rename(old, binary_path));
        drop(std::fs::remove_file(temp));
        return Err(e).with_context(|| {
            format!(
                "Failed to put new binary in place.
  Path: {}",
                binary_path.display()
            )
        });
    }

    // Best-effort cleanup of the old binary. This fails while the old
    // executable is still running; the next update removes it instead.
    drop(std::fs::remove_file(old));

    Ok(())
}

// ---------------------------------------------------------------------------
// Linux tar.gz update
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn download_and_replace_linux_tar(target: &UpdateTarget<'_>, binary_path: &Path) -> Result<()> {
    eprintln!("Downloading update...");

    let temp_archive = temp_path_beside(binary_path, ".tar.gz")?;
    let extract_dir = temp_path_beside(binary_path, ".extract")?;

    // The archive is hashed before extraction so tar never sees unverified
    // input; a failed download or checksum removes the partial archive.
    if let Err(error) = download_verified_asset(target, &temp_archive, "archive") {
        drop(std::fs::remove_file(&temp_archive));
        return Err(error);
    }

    let result = extract_portlens_binary(&temp_archive, &extract_dir)
        .and_then(|temp_binary| install_extracted_binary(target, &temp_binary, binary_path));

    // Best-effort cleanup of extraction directory
    drop(std::fs::remove_dir_all(&extract_dir));

    result
}

#[cfg(target_os = "linux")]
fn install_extracted_binary(
    target: &UpdateTarget<'_>,
    temp_binary: &Path,
    binary_path: &Path,
) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let permissions = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(temp_binary, permissions)
        .context("failed to set executable permission on updated binary")?;
    smoke_test_binary(temp_binary, target.version)?;
    replace_linux_binary(temp_binary, binary_path)
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
            "Downloaded file is too small ({} bytes), likely not a valid {kind}.",
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

    const HASH_A: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const HASH_B: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn checksum_manifest_text_mode_lookup() {
        let manifest = format!(
            "{HASH_B}  portlens-0.2.0-x86_64.tar.gz\n{HASH_A}  portlens-0.2.0-x86_64.exe\n"
        );
        assert_eq!(
            checksum_for_asset(&manifest, "portlens-0.2.0-x86_64.exe").unwrap(),
            HASH_A,
            "two-space text-mode lines should be parsed"
        );
    }

    #[test]
    fn checksum_manifest_binary_mode_and_crlf_lookup() {
        let manifest = format!(
            "\u{feff}{} *portlens-0.2.0-x86_64.exe\r\n\r\n",
            HASH_A.to_ascii_uppercase()
        );
        assert_eq!(
            checksum_for_asset(&manifest, "portlens-0.2.0-x86_64.exe").unwrap(),
            HASH_A,
            "binary-mode lines with CRLF, BOM, and uppercase hex should normalize to lowercase"
        );
    }

    #[test]
    fn checksum_manifest_missing_asset_is_error() {
        let manifest = format!("{HASH_A}  portlens-0.2.0-x86_64.tar.gz\n");
        let error = checksum_for_asset(&manifest, "portlens-0.2.0-x86_64.exe")
            .expect_err("an asset absent from the manifest must not verify");
        assert!(
            error.to_string().contains("no entry"),
            "missing entries should be reported explicitly"
        );
        assert!(
            checksum_for_asset("", "portlens-0.2.0-x86_64.exe").is_err(),
            "an empty manifest must not verify anything"
        );
    }

    #[test]
    fn checksum_manifest_rejects_malformed_lines() {
        let name = "portlens-0.2.0-x86_64.exe";
        let short_hash = &HASH_A[..63];
        let bad_hex = format!("{}g", &HASH_A[..63]);
        let malformed = [
            format!("{short_hash}  {name}"),
            format!("{bad_hex}  {name}"),
            format!("{HASH_A} {name}"),
            format!("{HASH_A}  "),
            format!("{HASH_A}   {name}"),
            format!("{HASH_A}\t{name}"),
            "not a checksum line".to_owned(),
        ];
        for line in &malformed {
            let manifest = format!("{line}\n{HASH_B}  other-file\n");
            assert!(
                checksum_for_asset(&manifest, name).is_err(),
                "malformed line should be rejected: {line:?}"
            );
        }
    }

    #[test]
    fn checksum_manifest_rejects_conflicting_entries() {
        let name = "portlens-0.2.0-x86_64.exe";
        let manifest = format!("{HASH_A}  {name}\n{HASH_B}  {name}\n");
        assert!(
            checksum_for_asset(&manifest, name).is_err(),
            "two different digests for one asset must not verify"
        );
        let duplicate = format!(
            "{HASH_A}  {name}\n{}  {name}\n",
            HASH_A.to_ascii_uppercase()
        );
        assert_eq!(
            checksum_for_asset(&duplicate, name).unwrap(),
            HASH_A,
            "identical duplicate entries (any case) are harmless"
        );
    }

    #[test]
    fn sha256_comparison_is_case_insensitive() {
        assert!(
            sha256_hex_matches(&HASH_A.to_ascii_uppercase(), HASH_A),
            "hex digests should compare case-insensitively"
        );
        assert!(
            !sha256_hex_matches(HASH_B, HASH_A),
            "different digests must not match"
        );
        assert!(
            !sha256_hex_matches("", ""),
            "an empty expected digest must never match"
        );
    }

    #[test]
    fn sha256_file_hex_and_verification() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("asset.bin");
        fs::write(&file_path, b"abc").unwrap();

        assert_eq!(
            sha256_file_hex(&file_path).unwrap(),
            HASH_A,
            "SHA-256 of 'abc' should match the FIPS 180-2 test vector"
        );
        verify_file_sha256(&file_path, &HASH_A.to_ascii_uppercase(), "binary")
            .expect("matching digest should verify");

        let error = verify_file_sha256(&file_path, HASH_B, "binary")
            .expect_err("mismatched digest should be rejected");
        assert!(
            error.to_string().contains("checksum verification"),
            "mismatch errors should mention checksum verification"
        );
        assert!(
            !file_path.exists(),
            "failed checksum verification should remove the downloaded file"
        );
    }

    #[test]
    fn url_allow_list_accepts_expected_github_urls() {
        assert!(is_allowed_url(
            "https://api.github.com/repos/ehsan18t/portlens/releases/latest",
            UrlKind::Api
        ));
        assert!(is_allowed_url(
            "https://github.com/ehsan18t/portlens/releases/download/v0.2.0/portlens-0.2.0-x86_64.exe",
            UrlKind::Download
        ));
        assert!(is_allowed_url(
            "HTTPS://GitHub.com/ehsan18t/portlens/releases/download/v0.2.0/SHA256SUMS",
            UrlKind::Download
        ));
    }

    #[test]
    fn url_allow_list_rejects_unexpected_urls() {
        let rejected = [
            "http://github.com/ehsan18t/portlens/releases/download/v0.2.0/a.exe",
            "https://github.com.evil.example/ehsan18t/portlens/releases/download/v0.2.0/a.exe",
            "https://evil.example/ehsan18t/portlens/releases/download/v0.2.0/a.exe",
            "https://github.com/someone-else/portlens/releases/download/v0.2.0/a.exe",
            "https://github.com/ehsan18t/portlens/releases/download/../../../x/y/a.exe",
            "https://github.com/ehsan18t/portlens/releases/download/%2E%2E/a.exe",
            "https://github.com/ehsan18t/portlens/releases/download/v0.2.0/a.exe?x=1",
            "https://github.com/ehsan18t/portlens/releases/download/v0.2.0/a b.exe",
            "https://github.com/ehsan18t/portlens/releases/download/",
            "https://objects.githubusercontent.com/ehsan18t/portlens/a.exe",
            "https://api.github.com/repos/ehsan18t/portlens/releases/latest",
            "",
        ];
        for url in rejected {
            assert!(
                !is_allowed_url(url, UrlKind::Download),
                "download URL should be rejected: {url}"
            );
        }
        assert!(
            !is_allowed_url(
                "https://github.com/ehsan18t/portlens/releases/download/v0.2.0/a.exe",
                UrlKind::Api
            ),
            "download URLs are not valid API URLs"
        );
        assert!(ensure_allowed_url("http://api.github.com/repos/x", UrlKind::Api).is_err());
    }

    fn release_with_asset(tag_name: &str, name: &str, url: &str) -> Release {
        Release {
            tag_name: tag_name.to_owned(),
            html_url: String::new(),
            assets: vec![Asset {
                name: name.to_owned(),
                browser_download_url: url.to_owned(),
                size_bytes: None,
            }],
        }
    }

    #[test]
    fn release_asset_url_must_match_release_tag_and_asset_name() {
        let name = "portlens-0.3.0-x86_64.exe";
        let ok = release_with_asset(
            "v0.3.0",
            name,
            "https://github.com/ehsan18t/portlens/releases/download/v0.3.0/portlens-0.3.0-x86_64.exe",
        );
        assert!(
            ensure_release_asset_url(&ok, &ok.assets[0]).is_ok(),
            "the canonical URL for the installed release must be accepted"
        );

        let sums = release_with_asset(
            "v0.3.0",
            CHECKSUMS_ASSET_NAME,
            "https://github.com/ehsan18t/portlens/releases/download/v0.3.0/SHA256SUMS",
        );
        assert!(ensure_release_asset_url(&sums, &sums.assets[0]).is_ok());

        for url in [
            // Asset from an older release of the same repository.
            "https://github.com/ehsan18t/portlens/releases/download/v0.1.0/portlens-0.3.0-x86_64.exe",
            // Right tag, but a different file than the asset being installed.
            "https://github.com/ehsan18t/portlens/releases/download/v0.3.0/other.exe",
            // Extra path segment under the release.
            "https://github.com/ehsan18t/portlens/releases/download/v0.3.0/x/portlens-0.3.0-x86_64.exe",
            // Case changes are not the exact URL.
            "https://GitHub.com/ehsan18t/portlens/releases/download/v0.3.0/portlens-0.3.0-x86_64.exe",
            "https://github.com/someone-else/portlens/releases/download/v0.3.0/portlens-0.3.0-x86_64.exe",
        ] {
            let release = release_with_asset("v0.3.0", name, url);
            assert!(
                ensure_release_asset_url(&release, &release.assets[0]).is_err(),
                "URL outside the exact release asset path must be refused: {url}"
            );
        }
    }

    #[test]
    fn release_asset_url_rejects_unsafe_tag_or_asset_names() {
        for (tag, name) in [
            ("..", "a.exe"),
            ("v0.3.0", "../a.exe"),
            ("v0.3.0", "a.exe?x=1"),
        ] {
            let url = expected_asset_url(tag, name);
            let release = release_with_asset(tag, name, &url);
            assert!(
                ensure_release_asset_url(&release, &release.assets[0]).is_err(),
                "a self-consistent but unsafe URL must still be refused: {url}"
            );
        }
    }

    #[test]
    fn curl_command_restricts_protocols() {
        let command = base_curl_command("30");
        let args: Vec<String> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        for (flag, value) in [("--proto", "=https"), ("--proto-redir", "=https")] {
            let index = args.iter().position(|a| a == flag);
            assert_eq!(
                index.and_then(|i| args.get(i + 1)).map(String::as_str),
                Some(value),
                "{flag} should be set to {value}"
            );
        }
        assert!(
            args.iter().any(|a| a == "--tlsv1.2"),
            "TLS 1.2+ should be required"
        );
    }

    #[test]
    fn stale_update_artifact_names() {
        let pid = 4242;
        for name in [
            ".portlens-update-1234.exe",
            ".portlens-update-1234.old.exe",
            ".portlens-update-1234.tar.gz",
            ".portlens-update-1234.extract",
            ".portlens-update-99999999999999999999.old.exe",
        ] {
            assert!(
                is_stale_update_artifact(name, pid),
                "{name} should be treated as stale"
            );
        }
        for name in [
            ".portlens-update-4242.exe",
            ".portlens-update-4242.old.exe",
            ".portlens-update-.exe",
            ".portlens-update-abc.exe",
            ".portlens-update-1234.txt",
            ".portlens-update-1234.exe.bak",
            "portlens.exe",
            "portlens-update-1234.exe",
        ] {
            assert!(
                !is_stale_update_artifact(name, pid),
                "{name} must not be treated as stale"
            );
        }
    }

    #[test]
    fn cleanup_removes_only_foreign_update_artifacts() {
        let dir = TempDir::new().unwrap();
        let own = dir
            .path()
            .join(format!(".portlens-update-{}.old.exe", std::process::id()));
        let foreign_file = dir.path().join(".portlens-update-1.old.exe");
        let foreign_dir = dir.path().join(".portlens-update-1.extract");
        let unrelated = dir.path().join("portlens.exe");
        fs::write(&own, b"x").unwrap();
        fs::write(&foreign_file, b"x").unwrap();
        fs::create_dir(&foreign_dir).unwrap();
        fs::write(foreign_dir.join("portlens"), b"x").unwrap();
        fs::write(&unrelated, b"x").unwrap();

        cleanup_stale_update_artifacts(dir.path());

        assert!(own.exists(), "the current process's files must be kept");
        assert!(!foreign_file.exists(), "stale backups should be removed");
        assert!(
            !foreign_dir.exists(),
            "stale extract dirs should be removed"
        );
        assert!(unrelated.exists(), "unrelated files must be kept");
    }

    #[test]
    fn smoke_test_rejects_unrunnable_file() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("not-a-binary");
        fs::write(&file_path, b"not an executable").unwrap();
        assert!(
            smoke_test_binary(&file_path, "0.2.0").is_err(),
            "a file that cannot run must fail the smoke test"
        );
    }

    #[test]
    fn version_output_matching() {
        assert!(version_output_matches("PortLens 0.2.0\n", "0.2.0"));
        assert!(version_output_matches("PortLens 0.2.0\n", "v0.2.0"));
        assert!(!version_output_matches("PortLens 0.2.1\n", "0.2.0"));
        assert!(!version_output_matches("PortLens 0.2.0-rc1\n", "0.2.0"));
        assert!(!version_output_matches("", "0.2.0"));
        assert!(!version_output_matches("PortLens", ""));
    }
}
