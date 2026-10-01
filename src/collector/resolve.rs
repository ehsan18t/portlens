//! Container resolution for socket listeners.
//!
//! These helpers turn a `(socket, pid, process_name)` triple into an
//! optional Docker/Podman `ContainerInfo` using the container map carried by
//! [`CollectContext`].

use std::net::SocketAddr;

use crate::docker::{self, ContainerPortMap, ProxyFallback};
use crate::types::Protocol;

use super::CollectContext;

// ── Container resolution ─────────────────────────────────────────────

/// Whether a listener is a container runtime port proxy, judged by its
/// process name or by its executable file name.
///
/// Both names are checked because they can differ: a process can rename
/// itself, and the reported name may be shortened. The collector and the
/// kill resolver share this check, so a row the listing attributes to a
/// container through the proxy fallback is also stopped as a container
/// rather than killed as a process.
pub fn is_container_proxy(process_name: &str, exe_name: Option<&str>) -> bool {
    docker::is_container_proxy_process(process_name)
        || exe_name.is_some_and(docker::is_container_proxy_process)
}

#[cfg(target_os = "linux")]
pub(super) fn resolve_container(
    context: &mut CollectContext<'_>,
    socket: SocketAddr,
    proto: Protocol,
    pid: u32,
    process_name: &str,
    exe_name: Option<&str>,
) -> Option<docker::ContainerInfo> {
    if let Some(container) =
        lookup_container(context.container_map, socket, proto, process_name, exe_name)
    {
        return Some(container.clone());
    }

    let rootless_name =
        rootless_podman_process_name(process_name, exe_name).unwrap_or(process_name);
    docker::lookup_rootless_podman_container(
        pid,
        rootless_name,
        context.podman_rootless_resolver,
        context.home,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::needless_pass_by_ref_mut)]
pub(super) fn resolve_container(
    context: &mut CollectContext<'_>,
    socket: SocketAddr,
    proto: Protocol,
    _pid: u32,
    process_name: &str,
    exe_name: Option<&str>,
) -> Option<docker::ContainerInfo> {
    lookup_container(context.container_map, socket, proto, process_name, exe_name).cloned()
}

fn lookup_container<'a>(
    container_map: &'a ContainerPortMap,
    socket: SocketAddr,
    proto: Protocol,
    process_name: &str,
    exe_name: Option<&str>,
) -> Option<&'a docker::ContainerInfo> {
    let fallback = if is_container_proxy(process_name, exe_name) {
        ProxyFallback::Allow
    } else {
        ProxyFallback::Deny
    };

    container_map
        .lookup(socket.ip(), socket.port(), proto, fallback)
        .container()
}

#[cfg(target_os = "linux")]
fn rootless_podman_process_name<'a>(
    process_name: &'a str,
    exe_name: Option<&'a str>,
) -> Option<&'a str> {
    if docker::is_podman_rootlessport_process(process_name) {
        return Some(process_name);
    }
    exe_name.filter(|name| docker::is_podman_rootlessport_process(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    fn make_container(name: &str, image: &str) -> docker::ContainerInfo {
        docker::ContainerInfo::new("", name, image)
    }

    fn insert_container(
        map: &mut ContainerPortMap,
        address: IpAddr,
        port: u16,
        name: &str,
        image: &str,
    ) {
        map.insert(
            Some(address),
            port,
            Protocol::Tcp,
            make_container(name, image),
        );
    }

    fn assert_container_name(container: Option<&docker::ContainerInfo>, expected_name: &str) {
        assert_eq!(
            container.map(|info| info.name.as_str()),
            Some(expected_name)
        );
    }

    #[test]
    fn container_lookup_prefers_exact_address_matches() {
        let mut map = ContainerPortMap::new();
        insert_container(
            &mut map,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            8080,
            "loopback-app",
            "node:22",
        );

        let exact = lookup_container(
            &map,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080),
            Protocol::Tcp,
            "node",
            None,
        );
        assert_container_name(exact, "loopback-app");

        let mismatch = lookup_container(
            &map,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10)), 8080),
            Protocol::Tcp,
            "node",
            None,
        );
        assert!(
            mismatch.is_none(),
            "non-matching local addresses must not inherit container enrichment"
        );
    }

    #[test]
    fn container_lookup_uses_proxy_fallback_for_unique_port_mapping() {
        let mut map = ContainerPortMap::new();
        insert_container(
            &mut map,
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            5432,
            "postgres",
            "postgres:16",
        );

        let container = lookup_container(
            &map,
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 5432),
            Protocol::Tcp,
            "wslrelay.exe",
            None,
        );
        assert_container_name(container, "postgres");
    }

    #[test]
    fn container_lookup_uses_proxy_fallback_for_rootlessport() {
        let mut map = ContainerPortMap::new();
        insert_container(
            &mut map,
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            6379,
            "redis",
            "redis:7-alpine",
        );

        let container = lookup_container(
            &map,
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 6379),
            Protocol::Tcp,
            "rootlessport",
            None,
        );
        assert_container_name(container, "redis");
    }

    #[test]
    fn container_lookup_uses_exe_name_for_proxy_fallback() {
        let mut map = ContainerPortMap::new();
        insert_container(
            &mut map,
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            3000,
            "web",
            "node:22",
        );

        let container = lookup_container(
            &map,
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 3000),
            Protocol::Tcp,
            "truncated-helper",
            Some("wslrelay.exe"),
        );
        assert_container_name(container, "web");
    }

    #[test]
    fn container_lookup_refuses_ambiguous_proxy_matches() {
        let mut map = ContainerPortMap::new();
        map.insert(
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            8080,
            Protocol::Tcp,
            make_container("api-a", "node:22"),
        );
        map.insert(
            Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10))),
            8080,
            Protocol::Tcp,
            make_container("api-b", "node:22"),
        );

        let container = lookup_container(
            &map,
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 8080),
            Protocol::Tcp,
            "wslrelay.exe",
            None,
        );
        assert!(
            container.is_none(),
            "proxy fallback should not guess when multiple distinct containers share the same port"
        );
    }

    #[test]
    fn container_lookup_keeps_proxy_fallback_when_all_matches_agree() {
        let mut map = ContainerPortMap::new();
        let container_info = make_container("shared-api", "node:22");

        map.insert(
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            8080,
            Protocol::Tcp,
            container_info.clone(),
        );
        map.insert(
            Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10))),
            8080,
            Protocol::Tcp,
            container_info,
        );

        let container = lookup_container(
            &map,
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 8080),
            Protocol::Tcp,
            "wslrelay.exe",
            None,
        );
        assert_container_name(container, "shared-api");
    }

    #[test]
    fn container_proxy_check_accepts_either_name() {
        assert!(is_container_proxy("docker-proxy", None));
        assert!(is_container_proxy("renamed-helper", Some("gvproxy.exe")));
        assert!(is_container_proxy(
            "rootlessport-ch",
            Some("rootlessport-child")
        ));
        assert!(!is_container_proxy("node", Some("node.exe")));
        assert!(!is_container_proxy("node", None));
    }
}
