//! # Project detection
//!
//! Walks upward from a process working directory looking for project
//! marker files to determine the project root and name.

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

/// Detect the project root path for a process.
///
/// Tries the working directory first, then falls back to parsing
/// command-line arguments for absolute file paths.
///
/// Returns the project root directory path, or `None` if no project
/// root can be determined.
pub fn detect_project_root(cwd: Option<&Path>, cmd: &[impl AsRef<str>]) -> Option<PathBuf> {
    if let Some(cwd) = cwd
        && let Some(root) = find_from_dir(cwd)
    {
        return Some(root);
    }

    // Fallback: look for absolute paths in command-line arguments
    for arg in cmd {
        let path = Path::new(arg.as_ref());
        if path.is_absolute()
            && let Some(parent) = path.parent()
            && let Some(root) = find_from_dir(parent)
        {
            return Some(root);
        }
    }

    None
}

/// Walk upward from `start` looking for project marker files.
///
/// Returns the path of the directory containing the first marker found,
/// or `None` if no marker is found before reaching the filesystem root.
///
/// This is the cwd-only variant exposed for caching in the collector.
/// Use [`detect_project_root`] for the full detection pipeline that also
/// checks command-line arguments.
#[must_use]
pub fn find_from_dir(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();

    loop {
        if has_marker(&current) {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Check whether a directory contains any project marker file.
///
/// Uses a single readdir call and in-memory checks instead of individual
/// stat calls per marker, reducing I/O for directories without markers.
fn has_marker(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };

    for entry in entries.filter_map(Result::ok) {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };

        if PROJECT_MARKERS.contains(&name) {
            return true;
        }

        if let Some(ext) = std::path::Path::new(name).extension() {
            let ext_str = ext.to_string_lossy();
            if PROJECT_MARKER_EXTENSIONS
                .iter()
                .any(|m| *m == ext_str.as_ref())
            {
                return true;
            }
        }
    }

    false
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
    fn detect_node_project() {
        let dir = setup_project("package.json");
        let result = detect_project_root(Some(dir.path()), &Vec::<String>::new());
        assert_eq!(result.as_deref(), Some(dir.path()));
    }

    #[test]
    fn detect_rust_project() {
        let dir = setup_project("Cargo.toml");
        let result = detect_project_root(Some(dir.path()), &Vec::<String>::new());
        assert_eq!(result.as_deref(), Some(dir.path()));
    }

    #[test]
    fn detect_go_project() {
        let dir = setup_project("go.mod");
        let result = detect_project_root(Some(dir.path()), &Vec::<String>::new());
        assert_eq!(result.as_deref(), Some(dir.path()));
    }

    #[test]
    fn detect_python_project() {
        let dir = setup_project("pyproject.toml");
        let result = detect_project_root(Some(dir.path()), &Vec::<String>::new());
        assert_eq!(result.as_deref(), Some(dir.path()));
    }

    #[test]
    fn detect_walks_upward() {
        let dir = setup_project("package.json");
        let sub = dir.path().join("src").join("deep");
        fs::create_dir_all(&sub).unwrap();
        let result = detect_project_root(Some(&sub), &Vec::<String>::new());
        assert_eq!(result.as_deref(), Some(dir.path()));
    }

    #[test]
    fn detect_no_marker_returns_none() {
        let dir = TempDir::new().unwrap();
        let result = detect_project_root(Some(dir.path()), &Vec::<String>::new());
        assert!(result.is_none());
    }

    #[test]
    fn detect_none_cwd_returns_none() {
        let result = detect_project_root(None, &Vec::<String>::new());
        assert!(result.is_none());
    }

    #[test]
    fn detect_csproj_extension_marker() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("MyApp.csproj"), "").unwrap();
        let result = detect_project_root(Some(dir.path()), &Vec::<String>::new());
        assert_eq!(result.as_deref(), Some(dir.path()));
    }

    #[test]
    fn detect_fallback_to_cmd_args() {
        let dir = setup_project("Cargo.toml");
        let fake_path = dir.path().join("src").join("main.rs");
        fs::create_dir_all(fake_path.parent().unwrap()).unwrap();
        fs::write(&fake_path, "").unwrap();
        let cmd = vec![fake_path.to_string_lossy().into_owned()];
        let result = detect_project_root(None, &cmd);
        assert_eq!(result.as_deref(), Some(dir.path()));
    }
}
