//! # Socket collector
//!
//! Calls the `listeners` crate to enumerate open sockets and `sysinfo` to
//! resolve process metadata (name, owning user). Enriches each entry with
//! Docker container info, project root detection, and app/framework labels.
//!
//! ## Module structure
//!
//! - `dedup` — Pure deduplication and proxy-collapsing engine.
//! - `entry` — Per-listener enrichment pipeline (build a single `PortEntry`).
//! - `resolve` — Container resolution against published runtime ports.
//! - `tcp_state` — OS-specific TCP connection state polling.
//! - `user` — User identity resolution and privilege detection.
//!
//! ## Future parallelization (crate extraction)
//!
//! The per-listener enrichment loop in `collect_with_options` is currently
//! sequential. When this module is extracted into a standalone crate, the
//! `build_entry` fan-out is a natural parallelization point: each listener
//! is enriched independently except for shared caches (`StackDetector`,
//! `process_names`, `UserResolver`).
//!
//! A deps-free approach using only `std::sync` primitives is sufficient:
//! wrap each cache in `Arc<Mutex<_>>` (or `Arc<RwLock<_>>` for the read-heavy
//! project cache) and dispatch work across a `std::thread::scope` pool. Avoid
//! pulling in `rayon`/`dashmap` — the crate aims to stay dependency-light, and
//! the expected listener count (< a few hundred) does not justify the cost.

mod dedup;
mod entry;
mod resolve;
mod tcp_state;
mod user;

pub(crate) use resolve::is_container_proxy;

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use log::debug;
use sysinfo::{ProcessesToUpdate, System};

use crate::docker::{self, ContainerPortMap};
use crate::types::PortEntry;

use tcp_state::TcpStateIndex;
use user::UserResolver;

/// Shared caches and inputs threaded through every per-listener enrichment.
///
/// Fields are `pub(super)` so submodules (`entry`, `resolve`) can read the
/// caches directly; `mod.rs` is the sole construction site.
pub(in crate::collector) struct CollectContext<'a> {
    pub(in crate::collector) sys: &'a System,
    pub(in crate::collector) user_resolver: &'a mut UserResolver,
    pub(in crate::collector) container_map: &'a ContainerPortMap,
    pub(in crate::collector) tcp_states: &'a TcpStateIndex,
    pub(in crate::collector) now_epoch: u64,
    pub(in crate::collector) deep_enrichment: bool,
    pub(in crate::collector) stack_detector: &'a mut what_stack::StackDetector,
    pub(in crate::collector) process_names: &'a mut HashSet<Arc<str>>,
    pub(in crate::collector) podman_rootless_resolver: &'a mut docker::RootlessPodmanResolver,
}

/// Options controlling how the collector enriches socket data.
#[derive(Debug, Clone, Copy)]
pub struct CollectOptions {
    /// Enable Docker/Podman lookup plus project-root and config-file enrichment.
    pub deep_enrichment: bool,
}

impl Default for CollectOptions {
    fn default() -> Self {
        Self {
            deep_enrichment: true,
        }
    }
}

/// The result of one collection pass.
#[derive(Debug)]
pub struct Collection {
    /// Deduplicated socket entries, sorted by port, address, and protocol.
    pub entries: Vec<PortEntry>,
    /// Why Docker/Podman detection found no containers, when it ran and
    /// failed. `None` when it succeeded or was skipped (`--no-enrich`).
    pub container_error: Option<docker::Error>,
    /// Whether a daemon published more ports than nanodock maps, so some rows
    /// may be missing their container name. `false` when detection failed or
    /// was skipped.
    pub containers_truncated: bool,
}

/// Collect all open TCP and UDP sockets using the provided enrichment options.
///
/// Shorthand for [`collect`] for callers that do not report container
/// detection failures.
pub fn collect_with_options(options: &CollectOptions) -> Result<Vec<PortEntry>> {
    collect(options).map(|collection| collection.entries)
}

/// Collect all open TCP and UDP sockets, and keep why container detection
/// failed.
///
/// When `deep_enrichment` is disabled, the collector skips Docker/Podman
/// probing, project-root walking, config-file scanning, and command-line path
/// fallback. Core socket, PID, user, uptime, and process-name detection remain.
pub fn collect(options: &CollectOptions) -> Result<Collection> {
    // Resolve the home directory once so Docker/Podman probing, the rootless
    // Podman resolver, and project-root detection share the same ceiling.
    let home = if options.deep_enrichment {
        what_stack::home_dir()
    } else {
        None
    };

    // Start Docker/Podman detection early so it runs concurrently with
    // the OS-level socket enumeration and process metadata refresh.
    let docker_handle = if options.deep_enrichment {
        Some(docker::Client::new().home(home.clone()).start_detection())
    } else {
        None
    };

    let raw_listeners = collect_raw_listeners()?;

    let mut sys = System::new();
    let tracked_pids = tracked_process_ids(&raw_listeners);
    let refresh_exe_paths = raw_listeners
        .iter()
        .any(|listener| listener.process.path.is_empty());
    debug!(
        "enumerated raw listeners: deep_enrichment={} listeners={} tracked_pids={} refresh_exe_paths={}",
        options.deep_enrichment,
        raw_listeners.len(),
        tracked_pids.len(),
        refresh_exe_paths
    );
    refresh_tracked_processes(
        &mut sys,
        &tracked_pids,
        options.deep_enrichment,
        refresh_exe_paths,
    );

    let mut user_resolver = UserResolver::default();

    // Block on Docker results only after all other I/O is done.
    let (container_map, container_error) =
        docker_handle.map_or_else(|| (ContainerPortMap::default(), None), wait_for_containers);
    let containers_truncated = container_map.truncated();
    let tcp_states = tcp_state::load_tcp_state_index();
    let now_epoch = current_epoch_secs();

    let mut process_names: HashSet<Arc<str>> = HashSet::new();
    // One resolver per scan: it caches Podman storage and per-PID answers
    // and never refreshes them on its own.
    let mut podman_rootless_resolver = docker::RootlessPodmanResolver::new().home(home.clone());
    let mut stack_detector = what_stack::StackDetector::with_home(home);
    let mut context = CollectContext {
        sys: &sys,
        user_resolver: &mut user_resolver,
        container_map: &container_map,
        tcp_states: &tcp_states,
        now_epoch,
        deep_enrichment: options.deep_enrichment,
        stack_detector: &mut stack_detector,
        process_names: &mut process_names,
        podman_rootless_resolver: &mut podman_rootless_resolver,
    };

    let all_entries: Vec<PortEntry> = raw_listeners
        .into_iter()
        .map(|l| entry::build_entry(&l, &mut context))
        .collect();

    let entries = deduplicate_and_sort_entries(all_entries);
    debug!("finished socket collection: entries={}", entries.len());
    Ok(Collection {
        entries,
        container_error,
        containers_truncated,
    })
}

/// Wait for background container detection, logging under `--trace` why it
/// failed or that it dropped published ports.
#[must_use]
pub fn wait_for_containers(
    handle: docker::DetectionHandle,
) -> (ContainerPortMap, Option<docker::Error>) {
    match handle.wait_result() {
        Ok(container_map) => {
            if container_map.truncated() {
                debug!(
                    "container detection dropped published ports past nanodock's binding cap: bindings_kept={}",
                    container_map.len()
                );
            }
            (container_map, None)
        }
        Err(error) => {
            log_detection_error(&error);
            (ContainerPortMap::default(), Some(error))
        }
    }
}

/// Log a container detection failure, with its underlying cause, under
/// `--trace`.
fn log_detection_error(error: &docker::Error) {
    match std::error::Error::source(error) {
        Some(source) => debug!("container detection failed: {error}: {source}"),
        None => debug!("container detection failed: {error}"),
    }
}

/// A one-line hint for a container detection failure the user can fix.
///
/// Only a permission problem gets a hint: the runtime is there but this user
/// may not talk to it, which silently drops every container name from the
/// listing. No runtime at all is normal, and the remaining failures have no
/// clear remedy, so they are only logged under `--trace`. The endpoint comes
/// from the environment (`DOCKER_HOST`) and is sanitized for the terminal.
#[must_use]
pub fn container_detection_hint(error: &docker::Error) -> Option<String> {
    let docker::Error::PermissionDenied { endpoint, .. } = error else {
        return None;
    };
    Some(permission_denied_hint(endpoint))
}

/// The one-line hint shown when container detection dropped published
/// ports (see [`Collection::containers_truncated`]). The text is fixed, so
/// nothing from a daemon reaches the terminal.
pub const CONTAINER_TRUNCATION_HINT: &str = "container detection skipped some published ports (a port range too large to map); some rows may be missing their container name";

/// The hint text for a permission problem on `endpoint`, sanitized for the
/// terminal. Split out so it can be tested without building a
/// `nanodock::Error`, whose data variants are non-exhaustive.
fn permission_denied_hint(endpoint: &str) -> String {
    format!(
        "container detection skipped: permission denied on {} ({})",
        crate::display::sanitize_for_terminal(endpoint),
        permission_remedy(endpoint)
    )
}

#[cfg(windows)]
const fn permission_remedy(_endpoint: &str) -> &'static str {
    "add your user to the docker-users group or run in an elevated terminal"
}

#[cfg(not(windows))]
fn permission_remedy(endpoint: &str) -> &'static str {
    if endpoint.contains("podman") {
        "run with sudo, or use your user's rootless Podman socket"
    } else {
        "add your user to the docker group or run with sudo"
    }
}

fn collect_raw_listeners() -> Result<Vec<listeners::Listener>> {
    listeners::get_all()
        .map(|listeners| listeners.into_iter().collect())
        .map_err(|error| anyhow::anyhow!("failed to enumerate open sockets from the OS: {error}"))
}

fn tracked_process_ids(raw_listeners: &[listeners::Listener]) -> Vec<sysinfo::Pid> {
    let mut tracked_pids: Vec<_> = raw_listeners
        .iter()
        .map(|listener| sysinfo::Pid::from_u32(listener.process.pid))
        .collect();
    tracked_pids.sort_unstable();
    tracked_pids.dedup();
    tracked_pids
}

fn refresh_tracked_processes(
    sys: &mut System,
    tracked_pids: &[sysinfo::Pid],
    deep_enrichment: bool,
    refresh_exe_paths: bool,
) {
    // `false` = do not remove previously-tracked dead processes. On a
    // freshly created System the internal map is empty, so this flag
    // has no effect either way. Passing `false` avoids the slightly
    // more expensive "clean up stale entries" pass.
    if tracked_pids.is_empty() {
        return;
    }

    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(tracked_pids),
        false,
        entry::process_refresh_kind(deep_enrichment, refresh_exe_paths),
    );
}

fn current_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn deduplicate_and_sort_entries(entries: Vec<PortEntry>) -> Vec<PortEntry> {
    let mut entries = dedup::deduplicate(entries);
    entries.sort_by(|left, right| {
        (
            left.port,
            left.local_addr,
            left.proto,
            left.pid,
            &*left.process,
        )
            .cmp(&(
                right.port,
                right.local_addr,
                right.proto,
                right.pid,
                &*right.process,
            ))
    });
    entries
}

/// Return a best-effort warning when the current process lacks full visibility.
///
/// On Linux this checks for effective root privileges. On Windows it checks
/// whether the current token is elevated. Other targets return `None`.
#[must_use]
pub fn visibility_warning() -> Option<&'static str> {
    if user::has_full_visibility_privileges() {
        None
    } else {
        Some(visibility_warning_message())
    }
}

#[cfg(target_os = "linux")]
const fn visibility_warning_message() -> &'static str {
    "running without root privileges can hide sockets and container metadata; rerun with sudo for full visibility"
}

#[cfg(windows)]
const fn visibility_warning_message() -> &'static str {
    "running without Administrator privileges can hide sockets and process metadata; rerun in an elevated terminal for full visibility"
}

#[cfg(not(any(target_os = "linux", windows)))]
const fn visibility_warning_message() -> &'static str {
    ""
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_denied_gets_a_sanitized_one_line_hint() {
        let hint = permission_denied_hint("/var/run/docker.sock\x1b]0;pwned\x07\nnext");
        assert!(
            hint.starts_with(
                "container detection skipped: permission denied on /var/run/docker.sock"
            ),
            "hint should name the endpoint: {hint}"
        );
        assert!(
            !hint.contains(['\x1b', '\x07', '\n']),
            "the endpoint must be sanitized for the terminal: {hint:?}"
        );
    }

    #[test]
    fn truncation_hint_is_one_plain_line() {
        assert!(
            !CONTAINER_TRUNCATION_HINT.chars().any(char::is_control)
                && CONTAINER_TRUNCATION_HINT.is_ascii(),
            "the truncation hint must print as one plain line: {CONTAINER_TRUNCATION_HINT:?}"
        );
    }

    #[test]
    fn missing_runtime_gets_no_hint() {
        assert!(
            container_detection_hint(&docker::Error::DaemonNotFound).is_none(),
            "no runtime installed is normal and should only be logged under --trace"
        );
    }
}
