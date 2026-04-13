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
//! - `resolve` — Container and project-root resolution with caching.
//! - `tcp_state` — OS-specific TCP connection state polling.
//! - `user` — User identity resolution and privilege detection.
//!
//! ## Future parallelization (crate extraction)
//!
//! The per-listener enrichment loop in `collect_with_options` is currently
//! sequential. When this module is extracted into a standalone crate, the
//! `build_entry` fan-out is a natural parallelization point: each listener
//! is enriched independently except for three shared caches
//! (`project_cache`, `process_names`, `UserResolver`).
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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use log::debug;
use sysinfo::{ProcessesToUpdate, System};

use crate::docker::{self, ContainerPortMap};
use crate::project;
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
    pub(in crate::collector) project_cache: &'a mut HashMap<PathBuf, Option<PathBuf>>,
    pub(in crate::collector) process_names: &'a mut HashSet<Arc<str>>,
    pub(in crate::collector) home: Option<&'a Path>,
    #[cfg(target_os = "linux")]
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

/// Collect all open TCP and UDP sockets using the provided enrichment options.
///
/// When `deep_enrichment` is disabled, the collector skips Docker/Podman
/// probing, project-root walking, config-file scanning, and command-line path
/// fallback. Core socket, PID, user, uptime, and process-name detection remain.
pub fn collect_with_options(options: &CollectOptions) -> Result<Vec<PortEntry>> {
    // Start Docker/Podman detection early so it runs concurrently with
    // the OS-level socket enumeration and process metadata refresh.
    let docker_handle = if options.deep_enrichment {
        Some(docker::start_detection())
    } else {
        None
    };

    let raw_listeners = listeners::get_all()
        .map_err(|e| anyhow::anyhow!("failed to enumerate open sockets from the OS: {e}"))?;

    let mut sys = System::new();

    let mut tracked_pids: Vec<_> = raw_listeners
        .iter()
        .map(|listener| sysinfo::Pid::from_u32(listener.process.pid))
        .collect();
    tracked_pids.sort_unstable();
    tracked_pids.dedup();
    debug!(
        "enumerated raw listeners: deep_enrichment={} listeners={} tracked_pids={}",
        options.deep_enrichment,
        raw_listeners.len(),
        tracked_pids.len()
    );

    // `false` = do not remove previously-tracked dead processes. On a
    // freshly created System the internal map is empty, so this flag
    // has no effect either way. Passing `false` avoids the slightly
    // more expensive "clean up stale entries" pass.
    if !tracked_pids.is_empty() {
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&tracked_pids),
            false,
            entry::process_refresh_kind(options.deep_enrichment),
        );
    }

    let mut user_resolver = UserResolver::default();

    // Block on Docker results only after all other I/O is done.
    let container_map =
        docker_handle.map_or_else(ContainerPortMap::default, docker::await_detection);
    let tcp_states = tcp_state::load_tcp_state_index();

    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut project_cache: HashMap<PathBuf, Option<PathBuf>> =
        HashMap::with_capacity(raw_listeners.len());
    let mut process_names: HashSet<Arc<str>> = HashSet::new();
    #[cfg(target_os = "linux")]
    let mut podman_rootless_resolver = docker::RootlessPodmanResolver::default();

    // Resolve the home directory once so that every per-process
    // invocation of find_from_dir does not each query the OS environment.
    let home = if options.deep_enrichment {
        project::home_dir()
    } else {
        None
    };
    let mut context = CollectContext {
        sys: &sys,
        user_resolver: &mut user_resolver,
        container_map: &container_map,
        tcp_states: &tcp_states,
        now_epoch,
        deep_enrichment: options.deep_enrichment,
        project_cache: &mut project_cache,
        process_names: &mut process_names,
        home: home.as_deref(),
        #[cfg(target_os = "linux")]
        podman_rootless_resolver: &mut podman_rootless_resolver,
    };

    let all_entries: Vec<PortEntry> = raw_listeners
        .into_iter()
        .map(|l| entry::build_entry(&l, &mut context))
        .collect();

    let mut entries = dedup::deduplicate(all_entries);
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
    debug!("finished socket collection: entries={}", entries.len());
    Ok(entries)
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
