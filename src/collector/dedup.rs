//! Pure deduplication and proxy-collapsing engine.
//!
//! Takes a `Vec<PortEntry>` and returns a refined, deduplicated list.
//! This module is entirely cross-platform with zero `#[cfg]` gates
//! and zero `unsafe` blocks.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::net::IpAddr;

use crate::types::{PortEntry, Protocol, State};

/// Key for clustering Docker proxy entries.
///
/// Within a given (port, protocol, state) group, all proxy entries
/// originate from the same container mapping, so project and app
/// labels are always identical and do not need to be part of the key.
type ProxyClusterKey = (u16, Protocol, State);

/// Deduplicate entries that share the same user-visible logical socket.
///
/// On Windows with Docker Desktop (WSL2), the OS reports multiple sockets
/// for the same Docker-published port (for example `wslrelay.exe` on IPv4
/// and `com.docker.backend.exe` on IPv4 and IPv6). This collapses repeated
/// rows from the same PID and then removes known Docker proxy duplicates
/// while preserving distinct non-proxy worker processes.
pub(super) fn deduplicate(entries: Vec<PortEntry>) -> Vec<PortEntry> {
    let mut grouped: HashMap<(u16, IpAddr, Protocol, State), Vec<PortEntry>> =
        HashMap::with_capacity(entries.len());

    for entry in entries {
        let key = (entry.port, entry.local_addr, entry.proto, entry.state);
        grouped.entry(key).or_default().push(entry);
    }

    let mut deduplicated = Vec::new();

    for group in grouped.into_values() {
        deduplicated.extend(deduplicate_group(group));
    }

    collapse_docker_proxy_clusters(deduplicated)
}

fn collapse_docker_proxy_clusters(entries: Vec<PortEntry>) -> Vec<PortEntry> {
    let mut proxy_clusters: HashMap<ProxyClusterKey, Vec<PortEntry>> =
        HashMap::with_capacity(entries.len());
    let mut result = Vec::with_capacity(entries.len());

    for entry in entries {
        if let Some(key) = docker_proxy_cluster_key(&entry) {
            proxy_clusters.entry(key).or_default().push(entry);
        } else {
            result.push(entry);
        }
    }

    for cluster in proxy_clusters.into_values() {
        if let Some(best) = cluster.into_iter().max_by(compare_proxy_cluster_preference) {
            result.push(best);
        }
    }

    result
}

fn docker_proxy_cluster_key(entry: &PortEntry) -> Option<ProxyClusterKey> {
    (is_docker_proxy_process(&entry.process) && has_docker_enrichment(entry)).then_some((
        entry.port,
        entry.proto,
        entry.state,
    ))
}

fn deduplicate_group(entries: Vec<PortEntry>) -> Vec<PortEntry> {
    let deduplicated = deduplicate_by_pid(entries);
    if deduplicated.len() <= 1 {
        return deduplicated;
    }

    let (proxy_entries, real_entries): (Vec<_>, Vec<_>) = deduplicated
        .into_iter()
        .partition(|entry| is_docker_proxy_process(&entry.process));

    if !proxy_entries.iter().any(has_docker_enrichment) {
        return proxy_entries.into_iter().chain(real_entries).collect();
    }

    if real_entries.is_empty() {
        return best_entry(proxy_entries).into_iter().collect();
    }

    real_entries
}

const fn has_docker_enrichment(entry: &PortEntry) -> bool {
    entry.project.is_some() || entry.app.is_some()
}

/// Deduplicate repeated rows from the same process while preserving distinct PIDs.
fn deduplicate_by_pid(entries: Vec<PortEntry>) -> Vec<PortEntry> {
    use std::collections::hash_map::Entry;

    let mut best_by_pid: HashMap<u32, PortEntry> = HashMap::with_capacity(entries.len());

    for entry in entries {
        match best_by_pid.entry(entry.pid) {
            Entry::Occupied(mut slot) => {
                if compare_entry_enrichment(&entry, slot.get()).is_gt() {
                    slot.insert(entry);
                }
            }
            Entry::Vacant(slot) => {
                slot.insert(entry);
            }
        }
    }

    best_by_pid.into_values().collect()
}

fn best_entry(entries: Vec<PortEntry>) -> Option<PortEntry> {
    entries.into_iter().max_by(compare_entry_preference)
}

fn compare_entry_enrichment(left: &PortEntry, right: &PortEntry) -> Ordering {
    enrichment_score(left).cmp(&enrichment_score(right))
}

fn compare_entry_preference(left: &PortEntry, right: &PortEntry) -> Ordering {
    compare_entry_enrichment(left, right)
        .then_with(|| right.pid.cmp(&left.pid))
        .then_with(|| right.process.as_str().cmp(left.process.as_str()))
}

fn compare_proxy_cluster_preference(left: &PortEntry, right: &PortEntry) -> Ordering {
    compare_entry_enrichment(left, right)
        .then_with(|| {
            address_preference(left.local_addr).cmp(&address_preference(right.local_addr))
        })
        .then_with(|| compare_entry_preference(left, right))
}

const fn address_preference(address: IpAddr) -> u8 {
    match address {
        IpAddr::V4(ipv4) if ipv4.is_unspecified() => 4,
        IpAddr::V6(ipv6) if ipv6.is_unspecified() => 3,
        IpAddr::V4(ipv4) if ipv4.is_loopback() => 2,
        IpAddr::V6(ipv6) if ipv6.is_loopback() => 1,
        IpAddr::V4(_) | IpAddr::V6(_) => 5,
    }
}

pub(super) fn is_docker_proxy_process(process_name: &str) -> bool {
    const DOCKER_PROXY_PROCESSES: &[&str] = &[
        "wslrelay",
        "com.docker.backend",
        "vpnkit",
        "docker-proxy",
        "rootlessport",
    ];

    let name = crate::types::strip_windows_exe_suffix(process_name);
    DOCKER_PROXY_PROCESSES
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
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

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    fn make_entry(port: u16, proto: Protocol) -> PortEntry {
        PortEntry {
            port,
            local_addr: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
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
        assert_eq!(result.len(), 1, "same-process duplicates should merge");
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
    fn dedup_preserves_same_port_different_processes_without_enrichment() {
        let mut first = make_entry(8080, Protocol::Tcp);
        first.pid = 1001;
        first.process = "worker-a".to_string();

        let mut second = make_entry(8080, Protocol::Tcp);
        second.pid = 1002;
        second.process = "worker-b".to_string();

        let result = deduplicate(vec![first, second]);
        assert_eq!(
            result.len(),
            2,
            "distinct processes on the same port should both remain"
        );
    }

    #[test]
    fn dedup_preserves_same_port_on_different_addresses() {
        let mut first = make_entry(8080, Protocol::Tcp);
        first.local_addr = IpAddr::V4(Ipv4Addr::LOCALHOST);

        let mut second = make_entry(8080, Protocol::Tcp);
        second.local_addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10));

        let result = deduplicate(vec![first, second]);
        assert_eq!(
            result.len(),
            2,
            "same-port listeners on different local addresses must remain distinct"
        );
    }

    #[test]
    fn dedup_collapses_docker_proxy_cluster_across_addresses() {
        let mut ipv4 = make_entry(5432, Protocol::Tcp);
        ipv4.local_addr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
        ipv4.pid = 2001;
        ipv4.process = "com.docker.backend.exe".to_string();
        ipv4.project = Some("ecom-postgres".to_string());
        ipv4.app = Some("PostgreSQL".into());

        let mut ipv6 = make_entry(5432, Protocol::Tcp);
        ipv6.local_addr = IpAddr::V6(Ipv6Addr::UNSPECIFIED);
        ipv6.pid = 2001;
        ipv6.process = "com.docker.backend.exe".to_string();
        ipv6.project = Some("ecom-postgres".to_string());
        ipv6.app = Some("PostgreSQL".into());

        let mut relay = make_entry(5432, Protocol::Tcp);
        relay.local_addr = IpAddr::V6(Ipv6Addr::LOCALHOST);
        relay.pid = 2002;
        relay.process = "wslrelay.exe".to_string();
        relay.project = Some("ecom-postgres".to_string());
        relay.app = Some("PostgreSQL".into());

        let result = deduplicate(vec![ipv4, ipv6, relay]);
        assert_eq!(
            result.len(),
            1,
            "Docker Desktop proxy fan-out should collapse to a single row"
        );
        assert_eq!(
            result[0].local_addr,
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            "IPv4 wildcard should be the preferred representative when present"
        );
    }

    #[test]
    fn dedup_prefers_enriched_entry() {
        let mut bare = make_entry(5432, Protocol::Tcp);
        bare.pid = 1001;
        bare.process = "wslrelay.exe".to_string();

        let mut enriched = make_entry(5432, Protocol::Tcp);
        enriched.pid = 1002;
        enriched.process = "com.docker.backend.exe".to_string();
        enriched.project = Some("my-postgres".to_string());
        enriched.app = Some("PostgreSQL".into());
        enriched.uptime_secs = Some(3600);

        let result = deduplicate(vec![bare, enriched]);
        assert_eq!(result.len(), 1);
        let entry = &result[0];
        assert_eq!(
            entry.project.as_deref(),
            Some("my-postgres"),
            "should keep the richest proxy entry"
        );
    }

    #[test]
    fn dedup_keeps_distinct_enriched_workers() {
        let mut first = make_entry(8080, Protocol::Tcp);
        first.pid = 1001;
        first.process = "nginx".to_string();
        first.app = Some("Nginx".into());

        let mut second = make_entry(8080, Protocol::Tcp);
        second.pid = 1002;
        second.process = "nginx".to_string();
        second.app = Some("Nginx".into());

        let result = deduplicate(vec![first, second]);
        assert_eq!(
            result.len(),
            2,
            "distinct worker PIDs should remain visible"
        );
    }

    #[test]
    fn dedup_drops_proxy_when_real_process_exists() {
        let mut proxy = make_entry(5432, Protocol::Tcp);
        proxy.pid = 1001;
        proxy.process = "wslrelay.exe".to_string();
        proxy.project = Some("my-postgres".to_string());
        proxy.app = Some("PostgreSQL".into());

        let mut real = make_entry(5432, Protocol::Tcp);
        real.pid = 1002;
        real.process = "postgres".to_string();
        real.app = Some("PostgreSQL".into());

        let result = deduplicate(vec![proxy, real]);
        assert_eq!(
            result.len(),
            1,
            "docker proxy rows should yield to real processes"
        );
        assert_eq!(result[0].process, "postgres");
    }

    #[test]
    fn dedup_keeps_proxy_named_process_without_docker_enrichment() {
        let mut proxy_named = make_entry(8080, Protocol::Tcp);
        proxy_named.pid = 1001;
        proxy_named.process = "docker-proxy.exe".to_string();

        let mut real = make_entry(8080, Protocol::Tcp);
        real.pid = 1002;
        real.process = "my-app".to_string();

        let result = deduplicate(vec![proxy_named, real]);
        assert_eq!(
            result.len(),
            2,
            "proxy-name pruning should only happen for Docker-enriched groups"
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
        entry.app = Some("App".into());
        entry.uptime_secs = Some(100);
        entry.user = "admin".to_string();
        assert_eq!(enrichment_score(&entry), 6, "fully enriched should score 6");
    }

    #[test]
    fn docker_proxy_process_names_are_detected_case_insensitively() {
        assert!(is_docker_proxy_process("wslrelay.exe"));
        assert!(is_docker_proxy_process("COM.DOCKER.BACKEND.EXE"));
        assert!(is_docker_proxy_process("vpnkit"));
        assert!(is_docker_proxy_process("ROOTLESSPORT"));
        assert!(!is_docker_proxy_process("nginx"));
    }

    #[test]
    fn docker_proxy_process_name_stripping_handles_non_ascii_input() {
        assert!(!is_docker_proxy_process("éabc"));
    }
}
