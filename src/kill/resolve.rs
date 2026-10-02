//! Resolve a local port number to the set of unique PIDs using it.
//!
//! Reuses the socket collector so every platform-specific detail (IPv4/IPv6
//! duplication, `SO_REUSEPORT` workers, Docker userland-proxy collapsing) is
//! handled in one place.
//!
//! When a port is owned by a Docker/Podman container, the resolver creates
//! a [`ContainerTarget`] instead of a process target so the kill flow can
//! stop the container via the daemon API rather than killing the proxy PID.
//! A container runtime or VM/WSL port forwarder that no single container can
//! be matched to is skipped with a `forwarder` report row instead.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use log::debug;

use super::platform::{ProcessIdentity, snapshot_identities};
use super::report::KillReportEntry;
use crate::collector::{self, CollectOptions};
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

/// Targets of a kill request, plus the matching listeners that were skipped
/// because they cannot be acted on safely.
#[derive(Debug, Default)]
pub struct ResolvedTargets {
    /// Processes to signal and containers to stop.
    pub targets: Vec<ResolvedTarget>,
    /// Port forwarders left alone because no single container could be
    /// matched to them, as `forwarder` report rows.
    pub skipped: Vec<KillReportEntry>,
}

/// Enumerate targets owning sockets on `port`.
///
/// Runs Docker/Podman detection in parallel with port enumeration. When
/// the matching entry is a known container runtime proxy (by process or
/// executable name) and the daemon reports a container for that port, the
/// resolver yields a [`ContainerTarget`]. A proxy no single container can be
/// matched to is skipped (see [`ResolvedTargets::skipped`]): it may be a
/// Lima, Podman machine or WSL forwarder for a port no container publishes,
/// and signaling it could cut off the runtime or VM. Any other entry produces
/// a regular process [`Target`].
pub fn targets_for_port(filter: PortFilter) -> Result<ResolvedTargets> {
    // Start Docker detection early so it overlaps with socket enumeration.
    let home = what_stack::home_dir();
    let docker_handle = docker::Client::new().home(home.clone()).start_detection();

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

    // A failure is logged under `--trace`; an empty map then makes every
    // proxy target skip instead of guessing. A failure the user can fix (a
    // permission problem) is named in each skip reason, because it, not the
    // forwarder, is then the likely cause.
    let (container_map, detection_error) = collector::wait_for_containers(docker_handle);
    let detection_hint = detection_error
        .as_ref()
        .and_then(collector::container_detection_hint);

    let mut resolved = resolve_targets_from_entries(
        entries,
        filter,
        &container_map,
        detection_hint.as_deref(),
        &identities,
        &mut docker::RootlessPodmanResolver::new().home(home),
    );
    attach_identities(&mut resolved.targets, &identities);
    Ok(resolved)
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
    /// `(pid, port)` of forwarders already skipped, so the IPv4 and IPv6
    /// sockets of one forwarded port give a single report row.
    seen_forwarders: HashSet<(u32, u16)>,
    resolved: ResolvedTargets,
}

fn resolve_targets_from_entries(
    entries: Vec<PortEntry>,
    filter: PortFilter,
    container_map: &ContainerPortMap,
    detection_hint: Option<&str>,
    identities: &HashMap<u32, ProcessIdentity>,
    podman_rootless_resolver: &mut docker::RootlessPodmanResolver,
) -> ResolvedTargets {
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
            detection_hint,
            &mut set,
            podman_rootless_resolver,
        );
    }

    set.resolved
}

fn append_target_from_entry(
    entry: &PortEntry,
    exe_name: Option<&str>,
    container_map: &ContainerPortMap,
    detection_hint: Option<&str>,
    set: &mut TargetSet,
    podman_rootless_resolver: &mut docker::RootlessPodmanResolver,
) {
    // Known proxy/helper processes can multiplex multiple published ports on a
    // single PID, so container dedup must happen after proxy resolution.
    if collector::is_container_proxy(&entry.process, exe_name) {
        match container_target_for_entry(
            container_map,
            entry,
            exe_name,
            detection_hint,
            podman_rootless_resolver,
        ) {
            Ok(ct) => {
                if set.seen_containers.insert(ct.container_id.clone()) {
                    debug!(
                        "resolved port {} to container '{}' (proxy pid {})",
                        entry.port, ct.container_name, ct.proxy_pid
                    );
                    set.resolved.targets.push(ResolvedTarget::Container(ct));
                }
            }
            // Skipped like a protected process, so one forwarder does not
            // abort the rest of a `--port` range.
            Err(reason) => {
                if set.seen_forwarders.insert((entry.pid, entry.port)) {
                    debug!(
                        "skipping port forwarder pid {} on port {}: {reason}",
                        entry.pid, entry.port
                    );
                    set.resolved.skipped.push(KillReportEntry::from_forwarder(
                        entry.pid,
                        entry.process.as_ref().to_owned(),
                        entry.port,
                        reason,
                    ));
                }
            }
        }
        return;
    }

    // Non-proxy processes can own multiple matching sockets, but signaling the
    // same PID more than once is redundant.
    if set.seen_pids.insert(entry.pid) {
        set.resolved.targets.push(ResolvedTarget::Process(Target {
            pid: entry.pid,
            process: entry.process.as_ref().to_owned(),
            identity: None,
        }));
    }
}

fn matches_port_target(entry: &PortEntry, filter: PortFilter) -> bool {
    filter.matches(entry.port) && (entry.proto == Protocol::Udp || entry.state == State::Listen)
}

/// Resolve a proxy/helper entry to a unique container target.
///
/// Returns why the entry must be left alone when no single container can be
/// matched to it. The reason follows the `skipped pid N (name) on port P:`
/// prefix of the report line, so it does not repeat the process or port.
/// `detection_hint` is the sanitized hint for a container detection failure
/// the user can fix, appended when no container could be matched.
fn container_target_for_entry(
    map: &ContainerPortMap,
    entry: &PortEntry,
    exe_name: Option<&str>,
    detection_hint: Option<&str>,
    podman_rootless_resolver: &mut docker::RootlessPodmanResolver,
) -> Result<ContainerTarget, String> {
    let api_match = map.lookup(
        entry.local_addr,
        entry.port,
        entry.proto,
        ProxyFallback::Allow,
    );

    let info = match api_match {
        PublishedContainerMatch::Match(info) => Some(Arc::clone(info)),
        PublishedContainerMatch::Ambiguous => {
            return Err(format!(
                "container runtime port forwarder for several containers that publish this port and protocol, so the container to stop is unclear; use 'kill --pid {}' to signal the forwarder itself",
                entry.pid
            ));
        }
        _ => None,
    };

    // Like the collector, accept `rootlessport` as either name.
    let rootless_name = exe_name
        .filter(|name| docker::is_podman_rootlessport_process(name))
        .unwrap_or(&entry.process);
    let info = info.or_else(|| {
        podman_rootless_resolver
            .lookup(entry.pid, rootless_name)
            .map(Arc::new)
    });

    let Some(info) = info else {
        let mut reason = format!(
            "container runtime or VM/WSL port forwarder with no matching container; it may be forwarding a port that no container publishes, and signaling it could cut off the runtime or VM; use 'kill --pid {}' if you really mean to signal it",
            entry.pid
        );
        if let Some(hint) = detection_hint {
            reason.push_str("; ");
            reason.push_str(hint);
        }
        return Err(reason);
    };

    // Use the container ID if available, otherwise fall back to the name.
    let api_id = if info.id.is_empty() {
        &info.name
    } else {
        &info.id
    };

    Ok(ContainerTarget {
        container_id: api_id.clone(),
        container_name: info.name.clone(),
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

    /// A resolver that never reads the test machine's rootless Podman
    /// storage below its home directory.
    fn no_home_resolver() -> docker::RootlessPodmanResolver {
        docker::RootlessPodmanResolver::new().home(None)
    }

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
            container_matched: false,
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
            None,
            &mut no_home_resolver(),
        )
        .expect_err("unresolved proxy ports must not fall back to killing the proxy pid");

        assert!(
            error.contains("port forwarder with no matching container")
                && error.contains("'kill --pid 4242'"),
            "the skip reason should name the forwarder case and the --pid escape hatch: {error}"
        );
        assert!(
            !error.contains("daemon is reachable"),
            "a forwarder for a port no container publishes is not a daemon problem: {error}"
        );
    }

    #[test]
    fn unresolved_proxy_reason_names_the_detection_failure() {
        let entry = make_entry(5432, Protocol::Tcp, State::Listen, "docker-proxy");
        let hint = "container detection skipped: permission denied on /var/run/docker.sock (add your user to the docker group or run with sudo)";
        let reason = container_target_for_entry(
            &ContainerPortMap::default(),
            &entry,
            None,
            Some(hint),
            &mut no_home_resolver(),
        )
        .expect_err("an empty map must not resolve a container");

        assert!(
            reason.ends_with(&format!("; {hint}")),
            "a permission problem should be named in the skip reason: {reason}"
        );
    }

    #[test]
    fn container_target_errors_sanitize_process_names() {
        let entry = make_entry(5432, Protocol::Tcp, State::Listen, "proxy\x1b]0;pwned\x07");
        let error = container_target_for_entry(
            &ContainerPortMap::default(),
            &entry,
            None,
            None,
            &mut no_home_resolver(),
        )
        .expect_err("unresolved proxy ports must be refused");
        assert!(
            !error.contains(['\x1b', '\x07']),
            "skip reasons must not carry raw process names: {error:?}"
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

        let error = container_target_for_entry(&map, &entry, None, None, &mut no_home_resolver())
            .expect_err("ambiguous proxy mappings must not pick an arbitrary container");

        assert!(
            error.contains("several containers that publish this port and protocol"),
            "ambiguous proxy matches should be rejected explicitly: {error}"
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
            None,
            &HashMap::new(),
            &mut no_home_resolver(),
        )
        .targets;

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
            None,
            &HashMap::new(),
            &mut no_home_resolver(),
        )
        .targets;

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
            None,
            &identities,
            &mut no_home_resolver(),
        )
        .targets;

        assert!(
            matches!(
                targets.as_slice(),
                [ResolvedTarget::Container(ContainerTarget { container_name, .. })] if container_name == "db"
            ),
            "the listing and kill must agree that this row is a container: {targets:?}"
        );
    }

    #[test]
    fn resolve_targets_from_entries_skips_unresolved_forwarder_in_range() {
        let mut forwarder_v4 = make_entry(8080, Protocol::Tcp, State::Listen, "limactl");
        forwarder_v4.pid = 7200;
        forwarder_v4.local_addr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
        let mut forwarder_v6 = forwarder_v4.clone();
        forwarder_v6.local_addr = IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED);
        let node = make_entry(3000, Protocol::Tcp, State::Listen, "node");

        let resolved = resolve_targets_from_entries(
            vec![forwarder_v4, forwarder_v6, node],
            PortFilter::Range {
                start: 3000,
                end: 9000,
            },
            &ContainerPortMap::default(),
            None,
            &HashMap::new(),
            &mut no_home_resolver(),
        );

        assert!(
            matches!(
                resolved.targets.as_slice(),
                [ResolvedTarget::Process(Target { pid: 4242, .. })]
            ),
            "the rest of the range must still resolve: {:?}",
            resolved.targets
        );
        assert_eq!(
            resolved.skipped.len(),
            1,
            "both sockets of one forwarded port should give one skipped row"
        );
        let skipped = &resolved.skipped[0];
        assert_eq!(skipped.status, super::super::report::KillStatus::Forwarder);
        assert_eq!((skipped.pid, skipped.port), (7200, Some(8080)));
    }
}
