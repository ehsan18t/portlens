//! Resolve a local port number to the set of unique PIDs using it.
//!
//! Reuses the socket collector so every platform-specific detail (IPv4/IPv6
//! duplication, `SO_REUSEPORT` workers, Docker userland-proxy collapsing) is
//! handled in one place.
//!
//! When a port is owned by a Docker/Podman container, the resolver creates
//! a [`ContainerTarget`] instead of a process target so the kill flow can
//! stop the container via the daemon API rather than killing the proxy PID.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use log::debug;

use super::platform::{ProcessIdentity, snapshot_identities};
use crate::collector::{self, CollectOptions};
use crate::display::sanitize_for_terminal;
use crate::docker::{self, ContainerPortMap, ProxyFallback, PublishedContainerMatch};
use crate::filter::PortFilter;
use crate::types::{PortEntry, Protocol, State};

/// A PID/process-name pair for one target of a kill request.
#[derive(Debug, Clone)]
pub struct Target {
    /// OS process identifier.
    pub pid: u32,
    /// Best-effort process name, "-" if unknown.
    pub process: String,
    /// Name and start time captured at resolve time, re-checked right before
    /// signaling to detect PID reuse. `None` when the process was not visible.
    pub identity: Option<ProcessIdentity>,
}

/// A Docker/Podman container to stop via the daemon API.
#[derive(Debug, Clone)]
pub struct ContainerTarget {
    /// Container identifier for API calls (full hex ID or name).
    pub container_id: String,
    /// Human-readable container name.
    pub container_name: String,
    /// The host port being freed.
    pub port: u16,
    /// PID of the Docker/Podman proxy process on the host.
    pub proxy_pid: u32,
    /// Name of the proxy process (e.g. "docker-proxy").
    pub proxy_process: String,
}

/// A resolved target that is either a process or a container.
#[derive(Debug, Clone)]
pub enum ResolvedTarget {
    /// A regular OS process to be signaled.
    Process(Target),
    /// A container to be stopped via the Docker/Podman daemon API.
    Container(ContainerTarget),
}

/// Enumerate targets owning sockets on `port`.
///
/// Runs Docker/Podman detection in parallel with port enumeration. When
/// the matching entry is a known container runtime proxy (by process or
/// executable name) and the daemon reports a container for that port, the
/// resolver yields a [`ContainerTarget`]. Otherwise it produces a regular
/// process [`Target`].
pub fn targets_for_port(filter: PortFilter) -> Result<Vec<ResolvedTarget>> {
    // Start Docker detection early so it overlaps with socket enumeration.
    let docker_handle = docker::start_detection(what_stack::home_dir());

    let entries = collector::collect_with_options(&CollectOptions {
        deep_enrichment: false,
    })?;

    // One snapshot serves both the proxy check (executable names) and the
    // PID-reuse check (identities) for every process on a matching port.
    let mut pids: Vec<u32> = entries
        .iter()
        .filter(|entry| matches_port_target(entry, filter))
        .map(|entry| entry.pid)
        .collect();
    pids.sort_unstable();
    pids.dedup();
    let identities = snapshot_identities(&pids);

    let container_map = docker_handle.wait();

    let mut targets = resolve_targets_from_entries(
        entries,
        filter,
        &container_map,
        &identities,
        &mut docker::RootlessPodmanResolver::default(),
        what_stack::home_dir().as_deref(),
    )?;
    attach_identities(&mut targets, &identities);
    Ok(targets)
}

/// Attach the resolve-time identity of every process target so the kill
/// step can detect a PID that was reused after resolution.
fn attach_identities(targets: &mut [ResolvedTarget], identities: &HashMap<u32, ProcessIdentity>) {
    for t in targets {
        if let ResolvedTarget::Process(p) = t {
            p.identity = identities.get(&p.pid).cloned();
        }
    }
}

/// Targets resolved so far, with the keys used to skip duplicates.
#[derive(Default)]
struct TargetSet {
    seen_pids: HashSet<u32>,
    seen_containers: HashSet<String>,
    targets: Vec<ResolvedTarget>,
}

fn resolve_targets_from_entries(
    entries: Vec<PortEntry>,
    filter: PortFilter,
    container_map: &ContainerPortMap,
    identities: &HashMap<u32, ProcessIdentity>,
    podman_rootless_resolver: &mut docker::RootlessPodmanResolver,
    home: Option<&std::path::Path>,
) -> Result<Vec<ResolvedTarget>> {
    let mut set = TargetSet::default();

    for entry in entries {
        if !matches_port_target(&entry, filter) {
            continue;
        }

        let exe_name = identities
            .get(&entry.pid)
            .and_then(|identity| identity.exe_name.as_deref());

        append_target_from_entry(
            &entry,
            exe_name,
            container_map,
            &mut set,
            podman_rootless_resolver,
            home,
        )?;
    }

    Ok(set.targets)
}

fn append_target_from_entry(
    entry: &PortEntry,
    exe_name: Option<&str>,
    container_map: &ContainerPortMap,
    set: &mut TargetSet,
    podman_rootless_resolver: &mut docker::RootlessPodmanResolver,
    home: Option<&std::path::Path>,
) -> Result<()> {
    // Known proxy/helper processes can multiplex multiple published ports on a
    // single PID, so container dedup must happen after proxy resolution.
    if collector::is_container_proxy(&entry.process, exe_name) {
        let ct = container_target_for_entry(
            container_map,
            entry,
            exe_name,
            podman_rootless_resolver,
            home,
        )?;

        if set.seen_containers.insert(ct.container_id.clone()) {
            debug!(
                "resolved port {} to container '{}' (proxy pid {})",
                entry.port, ct.container_name, ct.proxy_pid
            );
            set.targets.push(ResolvedTarget::Container(ct));
        }

        return Ok(());
    }

    // Non-proxy processes can own multiple matching sockets, but signaling the
    // same PID more than once is redundant.
    if set.seen_pids.insert(entry.pid) {
        set.targets.push(ResolvedTarget::Process(Target {
            pid: entry.pid,
            process: entry.process.as_ref().to_owned(),
            identity: None,
        }));
    }

    Ok(())
}

fn matches_port_target(entry: &PortEntry, filter: PortFilter) -> bool {
    filter.matches(entry.port) && (entry.proto == Protocol::Udp || entry.state == State::Listen)
}

/// Resolve a proxy/helper entry to a unique container target.
fn container_target_for_entry(
    map: &ContainerPortMap,
    entry: &crate::types::PortEntry,
    exe_name: Option<&str>,
    podman_rootless_resolver: &mut docker::RootlessPodmanResolver,
    home: Option<&std::path::Path>,
) -> Result<ContainerTarget> {
    let api_match = map.lookup(
        entry.local_addr,
        entry.port,
        entry.proto,
        ProxyFallback::Allow,
    );

    let info = match api_match {
        PublishedContainerMatch::Match(info) => Some(info.clone()),
        PublishedContainerMatch::Ambiguous => {
            bail!(
                "refusing to stop proxy pid {} ({}) on port {} because multiple containers publish the same port/protocol; use 'kill --pid' to target the proxy explicitly",
                entry.pid,
                sanitize_for_terminal(entry.process.as_ref()),
                entry.port
            );
        }
        _ => None,
    };

    // Like the collector, accept `rootlessport` as either name.
    let rootless_name = exe_name
        .filter(|name| docker::is_podman_rootlessport_process(name))
        .unwrap_or(&entry.process);
    let info = info.or_else(|| {
        docker::lookup_rootless_podman_container(
            entry.pid,
            rootless_name,
            podman_rootless_resolver,
            home,
        )
    });

    let Some(info) = info else {
        bail!(
            "refusing to kill proxy pid {} ({}) on port {} because the container could not be resolved; ensure the container runtime daemon is reachable or use 'kill --pid' to target the proxy explicitly",
            entry.pid,
            sanitize_for_terminal(entry.process.as_ref()),
            entry.port
        );
    };

    // Use the container ID if available, otherwise fall back to the name.
    let api_id = if info.id.is_empty() {
        info.name.clone()
    } else {
        info.id
    };
    let container_name = info.name;

    Ok(ContainerTarget {
        container_id: api_id,
        container_name,
        port: entry.port,
        proxy_pid: entry.pid,
        proxy_process: entry.process.as_ref().to_owned(),
    })
}

/// Resolve a PID by itself: look up its process name if possible.
///
/// Returns a synthetic target with "-" process name when the PID is not
/// currently enumerable (the kill path still treats that as `AlreadyGone` later).
pub fn target_for_pid(pid: u32) -> Option<Target> {
    let identity = snapshot_identities(&[pid]).remove(&pid)?;
    let process = if identity.name.is_empty() {
        "-".to_owned()
    } else {
        identity.name.clone()
    };

    Some(Target {
        pid,
        process,
        identity: Some(identity),
    })
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    fn make_entry(port: u16, proto: Protocol, state: State, process: &str) -> PortEntry {
        PortEntry {
            port,
            local_addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            proto,
            state,
            pid: 4242,
            process: process.into(),
            user: "user".into(),
            project: None,
            app: None,
            uptime_secs: None,
        }
    }

    fn insert_test_container(
        map: &mut ContainerPortMap,
        host_ip: Option<IpAddr>,
        port: u16,
        proto: Protocol,
        id: &str,
        name: &str,
        image: &str,
    ) {
        map.insert(
            host_ip,
            port,
            proto,
            docker::ContainerInfo::new(id, name, image),
        );
    }

    #[test]
    fn matches_port_target_requires_tcp_listen_state() {
        assert!(matches_port_target(
            &make_entry(8080, Protocol::Tcp, State::Listen, "node"),
            PortFilter::Single(8080),
        ));
        assert!(matches_port_target(
            &make_entry(53, Protocol::Udp, State::NotApplicable, "dnsmasq"),
            PortFilter::Single(53),
        ));
        assert!(
            !matches_port_target(
                &make_entry(8080, Protocol::Tcp, State::Established, "curl"),
                PortFilter::Single(8080),
            ),
            "port-based kill should not target non-listening TCP sockets"
        );
    }

    #[test]
    fn container_target_for_entry_refuses_unresolved_proxy() {
        let entry = make_entry(5432, Protocol::Tcp, State::Listen, "docker-proxy");
        let error = container_target_for_entry(
            &ContainerPortMap::default(),
            &entry,
            None,
            &mut docker::RootlessPodmanResolver::default(),
            None,
        )
        .expect_err("unresolved proxy ports must not fall back to killing the proxy pid");

        assert!(
            format!("{error:#}").contains("refusing to kill proxy pid"),
            "port-based kill should refuse unresolved container proxies"
        );
    }

    #[test]
    fn container_target_errors_sanitize_process_names() {
        let entry = make_entry(5432, Protocol::Tcp, State::Listen, "proxy\x1b]0;pwned\x07");
        let error = container_target_for_entry(
            &ContainerPortMap::default(),
            &entry,
            None,
            &mut docker::RootlessPodmanResolver::default(),
            None,
        )
        .expect_err("unresolved proxy ports must be refused");
        let message = format!("{error:#}");
        assert!(
            !message.contains(['\x1b', '\x07']),
            "process names in errors must be sanitized: {message:?}"
        );
    }

    #[test]
    fn container_target_for_entry_refuses_ambiguous_proxy_mappings() {
        let mut entry = make_entry(8080, Protocol::Tcp, State::Listen, "docker-proxy");
        entry.local_addr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
        let mut map = ContainerPortMap::new();
        insert_test_container(
            &mut map,
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            8080,
            Protocol::Tcp,
            "api-a",
            "api-a",
            "node:22",
        );
        insert_test_container(
            &mut map,
            Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10))),
            8080,
            Protocol::Tcp,
            "api-b",
            "api-b",
            "node:22",
        );

        let error = container_target_for_entry(
            &map,
            &entry,
            None,
            &mut docker::RootlessPodmanResolver::default(),
            None,
        )
        .expect_err("ambiguous proxy mappings must not pick an arbitrary container");

        assert!(
            format!("{error:#}").contains("multiple containers publish the same port/protocol"),
            "ambiguous proxy matches should be rejected explicitly"
        );
    }

    #[test]
    fn target_for_pid_captures_identity() {
        let target = target_for_pid(std::process::id())
            .expect("the test process should resolve to a kill target");
        let identity = target
            .identity
            .expect("a visible pid should carry a resolve-time identity");
        assert_eq!(
            target.process, identity.name,
            "display name and identity name should come from the same snapshot"
        );
    }

    #[test]
    fn target_for_pid_returns_none_when_process_is_missing() {
        assert!(
            target_for_pid(u32::MAX).is_none(),
            "an impossible pid should not resolve to a synthetic kill target"
        );
    }

    #[test]
    fn resolve_targets_from_entries_keeps_multiple_container_targets_for_shared_proxy_pid() {
        let mut first = make_entry(3000, Protocol::Tcp, State::Listen, "com.docker.backend.exe");
        first.pid = 7000;
        first.local_addr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

        let mut second = make_entry(4000, Protocol::Tcp, State::Listen, "com.docker.backend.exe");
        second.pid = 7000;
        second.local_addr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

        let mut map = ContainerPortMap::new();
        insert_test_container(
            &mut map,
            None,
            3000,
            Protocol::Tcp,
            "container-a",
            "api-a",
            "node:22",
        );
        insert_test_container(
            &mut map,
            None,
            4000,
            Protocol::Tcp,
            "container-b",
            "api-b",
            "node:22",
        );

        let targets = resolve_targets_from_entries(
            vec![first, second],
            PortFilter::Range {
                start: 3000,
                end: 4000,
            },
            &map,
            &HashMap::new(),
            &mut docker::RootlessPodmanResolver::default(),
            None,
        )
        .expect("shared proxy pids should still resolve each container target");

        assert_eq!(
            targets.len(),
            2,
            "both container targets should remain visible"
        );
        assert!(matches!(
            &targets[0],
            ResolvedTarget::Container(ContainerTarget { container_name, .. }) if container_name == "api-a"
        ));
        assert!(matches!(
            &targets[1],
            ResolvedTarget::Container(ContainerTarget { container_name, .. }) if container_name == "api-b"
        ));
    }

    #[test]
    fn resolve_targets_from_entries_keeps_pid_dedup_for_non_proxy_processes() {
        let mut first = make_entry(3000, Protocol::Tcp, State::Listen, "node");
        first.pid = 4242;

        let mut second = make_entry(4000, Protocol::Tcp, State::Listen, "node");
        second.pid = 4242;

        let targets = resolve_targets_from_entries(
            vec![first, second],
            PortFilter::Range {
                start: 3000,
                end: 4000,
            },
            &ContainerPortMap::default(),
            &HashMap::new(),
            &mut docker::RootlessPodmanResolver::default(),
            None,
        )
        .expect("non-proxy pid dedup should stay intact");

        assert_eq!(
            targets.len(),
            1,
            "the same non-proxy pid should still be targeted once"
        );
        assert!(matches!(
            &targets[0],
            ResolvedTarget::Process(Target { pid, process, .. }) if *pid == 4242 && process == "node"
        ));
    }

    #[test]
    fn resolve_targets_from_entries_recognizes_proxy_by_executable_name() {
        let mut entry = make_entry(5432, Protocol::Tcp, State::Listen, "renamed-helper");
        entry.pid = 7100;
        entry.local_addr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

        let mut map = ContainerPortMap::new();
        insert_test_container(
            &mut map,
            None,
            5432,
            Protocol::Tcp,
            "container-db",
            "db",
            "postgres:16",
        );

        let identities = HashMap::from([(
            7100,
            ProcessIdentity {
                name: "renamed-helper".to_string(),
                start_time: 1,
                origin: super::super::platform::ProcessOrigin::default(),
                exe_name: Some("docker-proxy".to_string()),
            },
        )]);

        let targets = resolve_targets_from_entries(
            vec![entry],
            PortFilter::Single(5432),
            &map,
            &identities,
            &mut docker::RootlessPodmanResolver::default(),
            None,
        )
        .expect("a proxy known by its executable name should resolve");

        assert!(
            matches!(
                targets.as_slice(),
                [ResolvedTarget::Container(ContainerTarget { container_name, .. })] if container_name == "db"
            ),
            "the listing and kill must agree that this row is a container: {targets:?}"
        );
    }
}
