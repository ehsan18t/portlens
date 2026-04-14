//! # Project detection
//!
//! Walks upward from a process working directory looking for project
//! marker files to determine the project root and name.
//!
//! The upward walk stops at the user's home directory to avoid matching
//! stray marker files (e.g. an accidental `package.json` in `~`), and
//! is capped at `MAX_WALK_DEPTH` levels as a safety net.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use log::debug;

#[cfg(unix)]
use std::ffi::CStr;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

/// Files whose presence indicates a project root directory.
///
/// Stored as a `HashSet` for O(1) lookups when scanning directory entries.
static PROJECT_MARKERS: LazyLock<HashSet<&str>> = LazyLock::new(|| {
    HashSet::from([
        "package.json",
        "Cargo.toml",
        "go.mod",
        "pyproject.toml",
        "requirements.txt",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "composer.json",
        "Gemfile",
        "mix.exs",
        "deno.json",
        "bun.lockb",
    ])
});

/// File extensions whose presence indicates a project root directory.
const PROJECT_MARKER_EXTENSIONS: &[&str] = &["csproj", "fsproj"];

/// Maximum number of parent directories to traverse before giving up.
///
/// Prevents excessive I/O for processes with deeply nested or unusual
/// working directories while still covering common monorepo nesting.
pub(crate) const MAX_WALK_DEPTH: usize = 64;

/// Iterate ancestor directories starting from `start`, walking upward.
///
/// Stops when the filesystem root is reached, `MAX_WALK_DEPTH` levels
/// have been visited, or the `home` directory ceiling is hit.
///
/// This is the single source of truth for the upward-walk strategy used
/// by both the uncached [`find_from_dir`] helper and the cached variant
/// in the collector module.
pub(crate) fn walk_ancestors<'a>(
    start: &'a Path,
    home: Option<&'a Path>,
) -> impl Iterator<Item = PathBuf> + 'a {
    let mut current = Some(start.to_path_buf());
    let mut depth = 0;

    std::iter::from_fn(move || {
        let dir = current.as_ref()?.clone();

        if depth >= MAX_WALK_DEPTH {
            current = None;
            return None;
        }

        if let Some(h) = home
            && dir == *h
        {
            current = None;
            return None;
        }

        depth += 1;

        let mut next = dir.clone();
        if next.pop() && next != dir {
            current = Some(next);
        } else {
            current = None;
        }

        Some(dir)
    })
}

/// Return the parent directories of the absolute paths found in `cmd`.
pub(crate) fn absolute_cmd_parents<'a, S: AsRef<OsStr> + 'a>(
    cmd: &'a [S],
) -> impl Iterator<Item = &'a Path> + 'a {
    cmd.iter().filter_map(|arg| {
        let path = Path::new(arg.as_ref());
        path.is_absolute().then(|| path.parent()).flatten()
    })
}

/// Walk upward from `start` looking for project marker files.
///
/// `home` is an optional ceiling directory. When `current` reaches
/// `home`, the walk stops regardless of whether a marker exists there.
/// This prevents stray markers in `~` from polluting unrelated processes.
///
/// Returns the path of the directory containing the first marker found,
/// or `None` if no marker is found before reaching any of the following
/// ceilings:
///
/// - `home` (when provided).
/// - `MAX_WALK_DEPTH` levels above `start`.
/// - The filesystem root.
///
/// Callers should resolve the home directory once and pass it in so
/// that repeated calls do not each query the OS environment.
///
/// This is the cwd-only variant exposed for caching in the collector.
/// The collector's `resolve` module combines this with
/// `absolute_cmd_parents` to replicate the full detection pipeline.
#[must_use]
pub fn find_from_dir(start: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let result = walk_ancestors(start, home).find(|dir| has_marker(dir));
    if let Some(ref root) = result {
        debug!(
            "found project marker: start={} root={}",
            start.display(),
            root.display()
        );
    }
    result
}

/// Return the current user's home directory, if it can be determined.
///
/// On Unix, this prefers passwd-database resolution for the invoking user
/// (or `SUDO_UID` during `sudo` sessions), then falls back to `SUDO_HOME`,
/// then `HOME` if lookup fails. On Windows, it uses
/// `USERPROFILE`. Returns `None` when no home directory can be determined,
/// in which case only the `MAX_WALK_DEPTH` guards against over-traversal.
///
/// Callers should call this **once** and pass the result into
/// [`find_from_dir`] to avoid repeated
/// environment-variable lookups across many process entries.
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        select_home_dir(
            preferred_home_uid().and_then(home_dir_from_uid),
            sudo_home_dir(),
            std::env::var_os("HOME").map(PathBuf::from),
        )
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
}

#[cfg(unix)]
fn select_home_dir(
    passwd_home: Option<PathBuf>,
    sudo_home: Option<PathBuf>,
    env_home: Option<PathBuf>,
) -> Option<PathBuf> {
    passwd_home.or(sudo_home).or(env_home)
}

#[cfg(unix)]
fn sudo_home_dir() -> Option<PathBuf> {
    (current_effective_uid() == 0)
        .then(|| std::env::var_os("SUDO_HOME"))
        .flatten()
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

#[cfg(unix)]
fn preferred_home_uid() -> Option<libc::uid_t> {
    preferred_home_uid_from_env(
        std::env::var_os("SUDO_UID").as_deref(),
        current_effective_uid(),
    )
}

#[cfg(unix)]
fn preferred_home_uid_from_env(
    sudo_uid: Option<&OsStr>,
    current_euid: libc::uid_t,
) -> Option<libc::uid_t> {
    if current_euid == 0 {
        sudo_uid
            .and_then(OsStr::to_str)
            .and_then(|value| value.parse::<libc::uid_t>().ok())
            .or(Some(current_euid))
    } else {
        Some(current_euid)
    }
}

#[cfg(unix)]
fn current_effective_uid() -> libc::uid_t {
    unsafe { libc::geteuid() }
}

#[cfg(unix)]
fn home_dir_from_uid(uid: libc::uid_t) -> Option<PathBuf> {
    let mut buffer = vec![0_u8; passwd_buffer_len()];
    let mut passwd = std::mem::MaybeUninit::<libc::passwd>::zeroed();
    let mut result = std::ptr::null_mut();
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            passwd.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &raw mut result,
        )
    };

    if status != 0 || result.is_null() {
        return None;
    }

    let passwd = unsafe { passwd.assume_init() };
    if passwd.pw_dir.is_null() {
        return None;
    }

    let home = unsafe { CStr::from_ptr(passwd.pw_dir) };
    Some(Path::new(OsStr::from_bytes(home.to_bytes())).to_path_buf())
}

#[cfg(unix)]
fn passwd_buffer_len() -> usize {
    const DEFAULT_PASSWD_BUFFER_LEN: usize = 1024;

    match unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) } {
        size if size > 0 => usize::try_from(size).unwrap_or(DEFAULT_PASSWD_BUFFER_LEN),
        _ => DEFAULT_PASSWD_BUFFER_LEN,
    }
}

/// Check whether a directory contains any project marker file.
///
/// Scans the directory once and checks both exact marker names and extension
/// markers from the collected entry names.
pub(crate) fn has_marker(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };

    entries.filter_map(Result::ok).any(|entry| {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            return false;
        };

        PROJECT_MARKERS.contains(name)
            || Path::new(name)
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|ext| PROJECT_MARKER_EXTENSIONS.contains(&ext))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_project(marker: &str) -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(marker), "").unwrap();
        dir
    }

    #[test]
    fn detect_known_project_markers() {
        for marker in ["package.json", "Cargo.toml", "go.mod", "pyproject.toml"] {
            let dir = setup_project(marker);
            let result = find_from_dir(dir.path(), None);
            assert_eq!(
                result.as_deref(),
                Some(dir.path()),
                "marker {marker} should be detected"
            );
        }
    }

    #[test]
    fn detect_walks_upward() {
        let dir = setup_project("package.json");
        let sub = dir.path().join("src").join("deep");
        fs::create_dir_all(&sub).unwrap();
        let result = find_from_dir(&sub, None);
        assert_eq!(result.as_deref(), Some(dir.path()));
    }

    #[test]
    fn detect_no_marker_returns_none() {
        let dir = TempDir::new().unwrap();
        let result = find_from_dir(dir.path(), None);
        assert!(result.is_none());
    }

    #[test]
    fn detect_csproj_extension_marker() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("MyApp.csproj"), "").unwrap();
        let result = find_from_dir(dir.path(), None);
        assert_eq!(result.as_deref(), Some(dir.path()));
    }

    #[test]
    fn detect_fallback_to_cmd_args() {
        let dir = setup_project("Cargo.toml");
        let fake_path = dir.path().join("src").join("main.rs");
        fs::create_dir_all(fake_path.parent().unwrap()).unwrap();
        fs::write(&fake_path, "").unwrap();
        let cmd = vec![fake_path.to_string_lossy().into_owned()];
        let result = absolute_cmd_parents(&cmd).find_map(|parent| find_from_dir(parent, None));
        assert_eq!(result.as_deref(), Some(dir.path()));
    }

    #[test]
    fn detect_fallback_scans_all_absolute_cmd_args() {
        let project = setup_project("Cargo.toml");
        let script_path = project.path().join("src").join("main.rs");
        fs::create_dir_all(script_path.parent().unwrap()).unwrap();
        fs::write(&script_path, "").unwrap();

        let interpreter_root = TempDir::new().unwrap();
        let interpreter_path = interpreter_root.path().join("bin").join("python.exe");
        fs::create_dir_all(interpreter_path.parent().unwrap()).unwrap();
        fs::write(&interpreter_path, "").unwrap();

        let cmd = vec![
            interpreter_path.to_string_lossy().into_owned(),
            script_path.to_string_lossy().into_owned(),
        ];

        let result = absolute_cmd_parents(&cmd).find_map(|parent| find_from_dir(parent, None));
        assert_eq!(result.as_deref(), Some(project.path()));
    }

    #[test]
    fn walk_stops_at_home_ceiling() {
        // Inject the fake home directory directly as a parameter — no
        // environment mutation needed, so this test is safe to run in
        // parallel with other tests.
        let fake_home = TempDir::new().unwrap();
        fs::write(fake_home.path().join("package.json"), "").unwrap();
        let sub = fake_home.path().join("unrelated");
        fs::create_dir_all(&sub).unwrap();

        let result = find_from_dir(&sub, Some(fake_home.path()));
        assert!(
            result.is_none(),
            "should NOT match stray marker in the home directory"
        );
    }

    #[test]
    fn walk_finds_marker_at_max_depth_boundary() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("package.json"), "").unwrap();

        let mut deep = dir.path().to_path_buf();
        for i in 0..MAX_WALK_DEPTH - 1 {
            deep = deep.join(format!("d{i}"));
        }
        fs::create_dir_all(&deep).unwrap();

        let result = find_from_dir(&deep, None);
        assert_eq!(
            result.as_deref(),
            Some(dir.path()),
            "walk should still find a marker at the depth boundary"
        );
    }

    #[test]
    fn walk_respects_max_depth() {
        let dir = TempDir::new().unwrap();
        // Create a marker at the temp root
        fs::write(dir.path().join("package.json"), "").unwrap();
        // Create a subdirectory deeper than MAX_WALK_DEPTH
        let mut deep = dir.path().to_path_buf();
        for i in 0..=MAX_WALK_DEPTH {
            deep = deep.join(format!("d{i}"));
        }
        fs::create_dir_all(&deep).unwrap();

        let result = find_from_dir(&deep, None);
        assert!(
            result.is_none(),
            "walk should stop after MAX_WALK_DEPTH levels"
        );
    }

    #[cfg(unix)]
    #[test]
    fn preferred_home_uid_from_env_prefers_sudo_uid_for_root_sessions() {
        assert_eq!(
            preferred_home_uid_from_env(Some(OsStr::new("1000")), 0),
            Some(1000),
            "sudo sessions should prefer the invoking user's uid"
        );
    }

    #[cfg(unix)]
    #[test]
    fn preferred_home_uid_from_env_ignores_sudo_uid_for_non_root_sessions() {
        assert_eq!(
            preferred_home_uid_from_env(Some(OsStr::new("1000")), 2000),
            Some(2000),
            "non-root sessions should keep the current effective uid"
        );
    }

    #[cfg(unix)]
    #[test]
    fn preferred_home_uid_from_env_falls_back_when_sudo_uid_is_invalid() {
        assert_eq!(
            preferred_home_uid_from_env(Some(OsStr::new("not-a-uid")), 0),
            Some(0),
            "invalid sudo metadata should not break home-directory lookup"
        );
    }

    #[cfg(unix)]
    #[test]
    fn select_home_dir_prefers_passwd_lookup_over_sudo_home() {
        let passwd_home = Some(PathBuf::from("/home/invoking-user"));
        let sudo_home = Some(PathBuf::from("/root"));
        let env_home = Some(PathBuf::from("/tmp/fallback"));

        assert_eq!(
            select_home_dir(passwd_home.clone(), sudo_home, env_home),
            passwd_home,
            "passwd-database resolution should win over environment-derived sudo home"
        );
    }

    #[cfg(unix)]
    #[test]
    fn select_home_dir_falls_back_to_sudo_home_before_home_env() {
        let sudo_home = Some(PathBuf::from("/home/invoking-user"));
        let env_home = Some(PathBuf::from("/root"));

        assert_eq!(
            select_home_dir(None, sudo_home.clone(), env_home),
            sudo_home,
            "sudo home should remain the fallback when passwd lookup is unavailable"
        );
    }
}
