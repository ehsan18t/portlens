//! # Socket collector
//!
//! Calls the `listeners` crate to enumerate open sockets and `sysinfo` to
//! resolve process metadata (name, owning user). Enriches each entry with
//! Docker container info, project root detection, and app/framework labels.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind, Users};

use crate::docker::{self, ContainerPortMap};
use crate::types::{PortEntry, Protocol, State};
use crate::{framework, project};

/// Collect all open TCP and UDP sockets on the system.
///
/// Returns a `Vec<PortEntry>` sorted by port number in ascending order.
/// Entries where the PID or username cannot be resolved are still included
/// with placeholder values.
///
/// When multiple OS-level sockets share the same port and protocol (e.g.
/// Docker Desktop on Windows binding to both IPv4 and IPv6), only the
/// most enriched entry is kept.
pub fn collect() -> Result<Vec<PortEntry>> {
    // Start Docker/Podman detection early so it runs concurrently with
    // the OS-level socket enumeration and process metadata refresh.
    let docker_handle = docker::start_detection();

    let raw_listeners = listeners::get_all()
        .map_err(|e| anyhow::anyhow!("failed to enumerate open sockets from the OS: {e}"))?;

    let mut sys = System::new();
    // `false` = do not remove previously-tracked dead processes. On a
    // freshly created System the internal map is empty, so this flag
    // has no effect either way. Passing `false` avoids the slightly
    // more expensive "clean up stale entries" pass.
    sys.refresh_processes_specifics(ProcessesToUpdate::All, false, process_refresh_kind());

    let users = Users::new_with_refreshed_list();

    // Block on Docker results only after all other I/O is done.
    let container_map = docker::await_detection(docker_handle);

    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut project_cache: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();

    // Resolve the home directory once so that every per-process
    // invocation of find_from_dir does not each query the OS environment.
    let home = project::home_dir();

    let all_entries: Vec<PortEntry> = raw_listeners
        .into_iter()
        .map(|l| {
            build_entry(
                &l,
                &sys,
                &users,
                &container_map,
                now_epoch,
                &mut project_cache,
                home.as_deref(),
            )
        })
        .collect();

    let mut entries = deduplicate(all_entries);
    entries.sort_by_key(|e| (e.port, e.proto));
    Ok(entries)
}

/// Build a single [`PortEntry`] from a [`listeners::Listener`], enriching it
/// with Docker, project, framework, and uptime information.
fn build_entry(
    l: &listeners::Listener,
    sys: &System,
    users: &Users,
    container_map: &ContainerPortMap,
    now_epoch: u64,
    project_cache: &mut HashMap<PathBuf, Option<PathBuf>>,
    home: Option<&Path>,
) -> PortEntry {
    let proto = match l.protocol {
        listeners::Protocol::TCP => Protocol::Tcp,
        listeners::Protocol::UDP => Protocol::Udp,
    };

    let state = match proto {
        Protocol::Tcp => State::Listen,
        Protocol::Udp => State::NotApplicable,
    };

    let sysinfo_pid = sysinfo::Pid::from_u32(l.process.pid);
    let sysinfo_process = sys.process(sysinfo_pid);
    let user = resolve_user(sysinfo_process, users);

    // Docker container lookup
    let container = container_map.get(&(l.socket.port(), proto));

    // Project detection: use container name for Docker ports, otherwise walk cwd.
    // The cache avoids redundant directory walks for processes sharing a cwd.
    let (project_name, project_root) = container.map_or_else(
        || {
            let cwd = sysinfo_process.and_then(sysinfo::Process::cwd);
            let cmd = extract_cmd(sysinfo_process);
            let root = lookup_project_root(cwd, &cmd, project_cache, home);
            let name = root
                .as_ref()
                .and_then(|r| r.file_name())
                .map(|n| n.to_string_lossy().into_owned());
            (name, root)
        },
        |c| (Some(c.name.clone()), None),
    );

    // App/framework detection
    let app = framework::detect(container, project_root.as_deref(), &l.process.name);

    // Uptime from process start time
    let uptime_secs = sysinfo_process.and_then(|p| {
        let start = p.start_time();
        if start > 0 && now_epoch > start {
            Some(now_epoch - start)
        } else {
            None
        }
    });

    PortEntry {
        port: l.socket.port(),
        proto,
        state,
        pid: l.process.pid,
        process: l.process.name.clone(),
        user,
        project: project_name,
        app,
        uptime_secs,
    }
}

/// Look up the project root for a process, using a cache to skip repeated
/// directory walks for processes that share the same working directory.
///
/// Falls back to [`project::detect_project_root`] for command-line
/// argument parsing when cwd-based detection fails.
///
/// Accepts `Option<&Path>` to avoid allocating a `PathBuf` for every
/// process on the cache-hit path. A `PathBuf` is only allocated on a
/// cache miss when inserting the result.
///
/// `home` is the user's home directory ceiling resolved once by
/// [`collect`] and passed down to avoid repeated env-var reads.
fn lookup_project_root(
    cwd: Option<&Path>,
    cmd: &[String],
    cache: &mut HashMap<PathBuf, Option<PathBuf>>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(cwd_path) = cwd {
        if let Some(cached) = cache.get(cwd_path) {
            if cached.is_some() {
                return cached.clone();
            }
            // Cached None: cwd walk found nothing; fall through to cmd-args.
        } else {
            let result = project::find_from_dir(cwd_path, home);
            cache.insert(cwd_path.to_path_buf(), result.clone());
            if result.is_some() {
                return result;
            }
        }
    }

    // Delegate the cmd-args fallback to the project module so the
    // logic is not duplicated in two places.
    project::detect_project_root(None, cmd, home)
}

/// Extract command-line arguments from a sysinfo process handle.
///
/// Returns an empty `Vec` when the process is `None` or has no args.
fn extract_cmd(process: Option<&sysinfo::Process>) -> Vec<String> {
    process
        .map(|p| {
            p.cmd()
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// Deduplicate entries that share the same `(port, protocol)`.
///
/// On Windows with Docker Desktop (WSL2), the OS reports multiple sockets
/// for the same Docker-published port (e.g. `wslrelay.exe` on IPv4,
/// `com.docker.backend.exe` on both IPv4 and IPv6). This keeps only the
/// entry with the richest enrichment data per port+protocol pair.
fn deduplicate(entries: Vec<PortEntry>) -> Vec<PortEntry> {
    use std::collections::hash_map::Entry;

    let mut best: HashMap<(u16, Protocol), PortEntry> = HashMap::new();

    for entry in entries {
        let key = (entry.port, entry.proto);
        match best.entry(key) {
            Entry::Occupied(mut slot) => {
                if enrichment_score(&entry) > enrichment_score(slot.get()) {
                    slot.insert(entry);
                }
            }
            Entry::Vacant(slot) => {
                slot.insert(entry);
            }
        }
    }

    best.into_values().collect()
}

/// Score an entry by how much enrichment data it carries.
///
/// Higher score means the entry has more useful metadata.
fn enrichment_score(e: &PortEntry) -> u8 {
    let mut score = 0;
    if e.project.is_some() {
        score += 2;
    }
    if e.app.is_some() {
        score += 2;
    }
    if e.uptime_secs.is_some() {
        score += 1;
    }
    if e.user != "-" {
        score += 1;
    }
    score
}

/// Resolve the owning username for an already-looked-up process.
///
/// Returns `"-"` if the process or user cannot be determined.
fn resolve_user(process: Option<&sysinfo::Process>, users: &Users) -> String {
    let Some(proc_ref) = process else {
        return "-".to_string();
    };

    let Some(uid) = proc_ref.user_id() else {
        return "-".to_string();
    };

    users
        .get_user_by_id(uid)
        .map_or_else(|| "-".to_string(), |u| u.name().to_string())
}

/// Refresh kind for process metadata needed by enrichment.
///
/// Collects: user, working directory, command-line args.
fn process_refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing()
        .with_user(UpdateKind::OnlyIfNotSet)
        .with_cwd(UpdateKind::OnlyIfNotSet)
        .with_cmd(UpdateKind::OnlyIfNotSet)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(port: u16, proto: Protocol) -> PortEntry {
        PortEntry {
            port,
            proto,
            state: State::Listen,
            pid: 1000,
            process: "test".to_string(),
            user: "-".to_string(),
            project: None,
            app: None,
            uptime_secs: None,
        }
    }

    #[test]
    fn dedup_keeps_single_entry_per_port() {
        let entries = vec![
            make_entry(5432, Protocol::Tcp),
            make_entry(5432, Protocol::Tcp),
            make_entry(5432, Protocol::Tcp),
        ];
        let result = deduplicate(entries);
        assert_eq!(result.len(), 1, "three entries on same port should merge");
    }

    #[test]
    fn dedup_preserves_different_ports() {
        let entries = vec![
            make_entry(5432, Protocol::Tcp),
            make_entry(6379, Protocol::Tcp),
            make_entry(53, Protocol::Udp),
        ];
        let result = deduplicate(entries);
        assert_eq!(
            result.len(),
            3,
            "distinct port+proto pairs should all remain"
        );
    }

    #[test]
    fn dedup_preserves_same_port_different_protocol() {
        let entries = vec![make_entry(53, Protocol::Tcp), make_entry(53, Protocol::Udp)];
        let result = deduplicate(entries);
        assert_eq!(
            result.len(),
            2,
            "same port with different protocols should both remain"
        );
    }

    #[test]
    fn dedup_prefers_enriched_entry() {
        let mut bare = make_entry(5432, Protocol::Tcp);
        bare.process = "wslrelay.exe".to_string();

        let mut enriched = make_entry(5432, Protocol::Tcp);
        enriched.process = "com.docker.backend.exe".to_string();
        enriched.project = Some("my-postgres".to_string());
        enriched.app = Some("PostgreSQL".to_string());
        enriched.uptime_secs = Some(3600);

        // Insert bare first, enriched second
        let result = deduplicate(vec![bare, enriched]);
        assert_eq!(result.len(), 1);
        let entry = &result[0];
        assert_eq!(
            entry.project.as_deref(),
            Some("my-postgres"),
            "should keep the enriched entry"
        );
    }

    #[test]
    fn enrichment_score_empty() {
        let entry = make_entry(80, Protocol::Tcp);
        assert_eq!(enrichment_score(&entry), 0, "bare entry should score 0");
    }

    #[test]
    fn enrichment_score_fully_enriched() {
        let mut entry = make_entry(80, Protocol::Tcp);
        entry.project = Some("proj".to_string());
        entry.app = Some("App".to_string());
        entry.uptime_secs = Some(100);
        entry.user = "admin".to_string();
        assert_eq!(enrichment_score(&entry), 6, "fully enriched should score 6");
    }
}
