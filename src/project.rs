//! # Project detection
//!
//! Walks upward from a process working directory looking for project
//! marker files to determine the project root and name.
//!
//! The upward walk stops at the user's home directory to avoid matching
//! stray marker files (e.g. an accidental `package.json` in `~`), and
//! is capped at `MAX_WALK_DEPTH` levels as a safety net.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Files whose presence indicates a project root directory.
const PROJECT_MARKERS: &[&str] = &[
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
];

/// File extensions whose presence indicates a project root directory.
const PROJECT_MARKER_EXTENSIONS: &[&str] = &["csproj", "fsproj"];

/// Maximum number of parent directories to traverse before giving up.
///
/// Prevents excessive I/O for processes with deeply nested or unusual
/// working directories (e.g. `/var/run/service/nested/deep/path`).
pub(crate) const MAX_WALK_DEPTH: usize = 16;

/// Detect the project root path for a process.
///
/// Tries the working directory first, then falls back to parsing
/// command-line arguments for absolute file paths.
///
/// `home` is the user's home directory used as an upward-walk ceiling.
/// Pass the value resolved once by the caller to avoid repeated env
/// lookups across many processes.
///
/// Returns the project root directory path, or `None` if no project
/// root can be determined.
pub fn detect_project_root(
    cwd: Option<&Path>,
    cmd: &[impl AsRef<OsStr>],
    home: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(cwd) = cwd
        && let Some(root) = find_from_dir(cwd, home)
    {
        return Some(root);
    }

    for parent in absolute_cmd_parents(cmd) {
        if let Some(root) = find_from_dir(parent, home) {
            return Some(root);
        }
    }

    None
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
/// Use [`detect_project_root`] for the full detection pipeline that also
/// checks command-line arguments.
#[must_use]
pub fn find_from_dir(start: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let mut current = start.to_path_buf();

    for _ in 0..MAX_WALK_DEPTH {
        // Stop at the user's home directory to avoid matching stray
        // marker files that were accidentally placed in ~ or above.
        if let Some(h) = home
            && current == h
        {
            return None;
        }

        if has_marker(&current) {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }

    None
}

/// Return the current user's home directory, if it can be determined.
///
/// Uses `HOME` on Unix and `USERPROFILE` on Windows. Returns `None`
/// when the environment variable is unset, in which case only the
/// `MAX_WALK_DEPTH` guards against over-traversal.
///
/// Callers should call this **once** and pass the result into
/// [`find_from_dir`] / [`detect_project_root`] to avoid repeated
/// environment-variable lookups across many process entries.
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
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

        PROJECT_MARKERS.contains(&name)
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
            let result = detect_project_root(Some(dir.path()), &[] as &[&str], None);
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
        let result = detect_project_root(Some(&sub), &Vec::<String>::new(), None);
        assert_eq!(result.as_deref(), Some(dir.path()));
    }

    #[test]
    fn detect_no_marker_returns_none() {
        let dir = TempDir::new().unwrap();
        let result = detect_project_root(Some(dir.path()), &Vec::<String>::new(), None);
        assert!(result.is_none());
    }

    #[test]
    fn detect_none_cwd_returns_none() {
        let result = detect_project_root(None, &Vec::<String>::new(), None);
        assert!(result.is_none());
    }

    #[test]
    fn detect_csproj_extension_marker() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("MyApp.csproj"), "").unwrap();
        let result = detect_project_root(Some(dir.path()), &Vec::<String>::new(), None);
        assert_eq!(result.as_deref(), Some(dir.path()));
    }

    #[test]
    fn detect_fallback_to_cmd_args() {
        let dir = setup_project("Cargo.toml");
        let fake_path = dir.path().join("src").join("main.rs");
        fs::create_dir_all(fake_path.parent().unwrap()).unwrap();
        fs::write(&fake_path, "").unwrap();
        let cmd = vec![fake_path.to_string_lossy().into_owned()];
        let result = detect_project_root(None, &cmd, None);
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

        let result = detect_project_root(None, &cmd, None);
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
}
