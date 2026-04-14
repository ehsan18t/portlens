//! Resolve a local port number to the set of unique PIDs using it.
//!
//! Reuses the socket collector so every platform-specific detail (IPv4/IPv6
//! duplication, `SO_REUSEPORT` workers, Docker userland-proxy collapsing) is
//! handled in one place.
//!
//! When a port is owned by a Docker/Podman container, the resolver creates
//! a [`ContainerTarget`] instead of a process target so the kill flow can
//! stop the container via the daemon API rather than killing the proxy PID.

use std::net::SocketAddr;

use anyhow::{Result, bail};
use log::debug;

use crate::collector::{self, CollectOptions};
use crate::docker::{self, ContainerPortMap, PublishedContainerMatch};
use crate::filter::PortFilter;
use crate::types::{PortEntry, Protocol, State};

/// A PID/process-name pair for one target of a kill request.
#[derive(Debug, Clone)]
pub struct Target {
    /// OS process identifier.
    pub pid: u32,
    /// Best-effort process name, "-" if unknown.
    pub process: String,
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
/// the matching entry is a known Docker proxy/helper and the daemon reports a
/// container for that port, the resolver yields a [`ContainerTarget`].
/// Otherwise it produces a regular process [`Target`].
pub fn targets_for_port(filter: PortFilter) -> Result<Vec<ResolvedTarget>> {
    // Start Docker detection early so it overlaps with socket enumeration.
    let docker_handle = docker::start_detection();

    let entries = collector::collect_with_options(&CollectOptions {
        deep_enrichment: false,
    })?;

    let container_map = docker::await_detection(docker_handle);

    resolve_targets_from_entries(
        entries,
        filter,
        &container_map,
        #[cfg(target_os = "linux")]
        &mut docker::RootlessPodmanResolver::default(),
        #[cfg(target_os = "linux")]
        crate::project::home_dir().as_deref(),
    )
}

fn resolve_targets_from_entries(
    entries: Vec<PortEntry>,
    filter: PortFilter,
    container_map: &ContainerPortMap,
    #[cfg(target_os = "linux")] podman_rootless_resolver: &mut docker::RootlessPodmanResolver,
    #[cfg(target_os = "linux")] home: Option<&std::path::Path>,
) -> Result<Vec<ResolvedTarget>> {
    let mut seen_pids: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut seen_containers: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut targets = Vec::new();

    for entry in entries {
        if !matches_port_target(&entry, filter) {
            continue;
        }

        append_target_from_entry(
            &entry,
            container_map,
            &mut seen_pids,
            &mut seen_containers,
            &mut targets,
            #[cfg(target_os = "linux")]
            podman_rootless_resolver,
            #[cfg(target_os = "linux")]
            home,
        )?;
    }

    Ok(targets)
}

fn append_target_from_entry(
    entry: &PortEntry,
    container_map: &ContainerPortMap,
    seen_pids: &mut std::collections::HashSet<u32>,
    seen_containers: &mut std::collections::HashSet<String>,
    targets: &mut Vec<ResolvedTarget>,
    #[cfg(target_os = "linux")] podman_rootless_resolver: &mut docker::RootlessPodmanResolver,
    #[cfg(target_os = "linux")] home: Option<&std::path::Path>,
) -> Result<()> {
    let process_name = entry.process.as_ref();

    // Known proxy/helper processes can multiplex multiple published ports on a
    // single PID, so container dedup must happen after proxy resolution.
    if collector::is_docker_proxy_process(process_name) {
        let ct = container_target_for_entry(
            container_map,
            entry,
            #[cfg(target_os = "linux")]
            podman_rootless_resolver,
            #[cfg(target_os = "linux")]
            home,
        )?;

        if seen_containers.insert(ct.container_id.clone()) {
            debug!(
                "resolved port {} to container '{}' (proxy pid {})",
                entry.port, ct.container_name, ct.proxy_pid
            );
            targets.push(ResolvedTarget::Container(ct));
        }

        return Ok(());
    }

    // Non-proxy processes can own multiple matching sockets, but signaling the
    // same PID more than once is redundant.
    if seen_pids.insert(entry.pid) {
        targets.push(ResolvedTarget::Process(Target {
            pid: entry.pid,
            process: process_name.to_owned(),
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
    #[cfg(target_os = "linux")] podman_rootless_resolver: &mut docker::RootlessPodmanResolver,
    #[cfg(target_os = "linux")] home: Option<&std::path::Path>,
) -> Result<ContainerTarget> {
    let socket = SocketAddr::new(entry.local_addr, entry.port);
    let api_match = docker::lookup_published_container(map, socket, entry.proto, true);

    let info = match api_match {
        PublishedContainerMatch::Match(info) => Some(info.clone()),
        PublishedContainerMatch::Ambiguous => {
            bail!(
                "refusing to stop proxy pid {} ({}) on port {} because multiple containers publish the same port/protocol; use 'kill --pid' to target the proxy explicitly",
                entry.pid,
                entry.process,
                entry.port
            );
        }
        PublishedContainerMatch::NotFound => None,
    };

    #[cfg(target_os = "linux")]
    let info = info.or_else(|| {
        docker::lookup_rootless_podman_container(
            entry.pid,
            entry.process.as_ref(),
            podman_rootless_resolver,
            home,
        )
    });

    let Some(info) = info else {
        bail!(
            "refusing to kill proxy pid {} ({}) on port {} because the container could not be resolved; ensure the container runtime daemon is reachable or use 'kill --pid' to target the proxy explicitly",
            entry.pid,
            entry.process,
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
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

    let mut sys = System::new();
    let sys_pid = Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[sys_pid]),
        false,
        ProcessRefreshKind::nothing(),
    );

    let process = sys.process(sys_pid)?;
    let process_name = process.name().to_string_lossy();
    let process = if process_name.is_empty() {
        "-".to_owned()
    } else {
        process_name.into_owned()
    };

    Some(Target { pid, process })
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
            #[cfg(target_os = "linux")]
            &mut docker::RootlessPodmanResolver::default(),
            #[cfg(target_os = "linux")]
            None,
        )
        .expect_err("unresolved proxy ports must not fall back to killing the proxy pid");

        assert!(
            format!("{error:#}").contains("refusing to kill proxy pid"),
            "port-based kill should refuse unresolved container proxies"
        );
    }

    #[test]
    fn container_target_for_entry_refuses_ambiguous_proxy_mappings() {
        let mut entry = make_entry(8080, Protocol::Tcp, State::Listen, "docker-proxy");
        entry.local_addr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
        let mut map = ContainerPortMap::new();
        map.insert(
            (Some(IpAddr::V4(Ipv4Addr::LOCALHOST)), 8080, Protocol::Tcp),
            docker::ContainerInfo {
                id: "api-a".to_string(),
                name: "api-a".to_string(),
                image: "node:22".to_string(),
            },
        );
        map.insert(
            (
                Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10))),
                8080,
                Protocol::Tcp,
            ),
            docker::ContainerInfo {
                id: "api-b".to_string(),
                name: "api-b".to_string(),
                image: "node:22".to_string(),
            },
        );

        let error = container_target_for_entry(
            &map,
            &entry,
            #[cfg(target_os = "linux")]
            &mut docker::RootlessPodmanResolver::default(),
            #[cfg(target_os = "linux")]
            None,
        )
        .expect_err("ambiguous proxy mappings must not pick an arbitrary container");

        assert!(
            format!("{error:#}").contains("multiple containers publish the same port/protocol"),
            "ambiguous proxy matches should be rejected explicitly"
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
        map.insert(
            (None, 3000, Protocol::Tcp),
            docker::ContainerInfo {
                id: "container-a".to_string(),
                name: "api-a".to_string(),
                image: "node:22".to_string(),
            },
        );
        map.insert(
            (None, 4000, Protocol::Tcp),
            docker::ContainerInfo {
                id: "container-b".to_string(),
                name: "api-b".to_string(),
                image: "node:22".to_string(),
            },
        );

        let targets = resolve_targets_from_entries(
            vec![first, second],
            PortFilter::Range {
                start: 3000,
                end: 4000,
            },
            &map,
            #[cfg(target_os = "linux")]
            &mut docker::RootlessPodmanResolver::default(),
            #[cfg(target_os = "linux")]
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
            #[cfg(target_os = "linux")]
            &mut docker::RootlessPodmanResolver::default(),
            #[cfg(target_os = "linux")]
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
            ResolvedTarget::Process(Target { pid, process }) if *pid == 4242 && process == "node"
        ));
    }
}
