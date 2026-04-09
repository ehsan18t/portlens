//! # Socket collector
//!
//! Calls the `listeners` crate to enumerate open sockets and `sysinfo` to
//! resolve process metadata (name, owning user). Enriches each entry with
//! Docker container info, project root detection, and app/framework labels.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
#[cfg(unix)]
use std::ffi::CStr;
use std::ffi::OsString;
#[cfg(unix)]
use std::mem::MaybeUninit;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
#[cfg(unix)]
use std::ops::Deref;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::{ffi::c_void, ptr};

#[cfg(target_os = "linux")]
use std::io::{BufRead, BufReader};

use anyhow::Result;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::docker::{self, ContainerPortMap};
use crate::types::{PortEntry, Protocol, State};
use crate::{framework, project};

type TcpStateIndex = HashMap<SocketAddr, State>;

#[derive(Default)]
struct UserResolver {
    #[cfg(unix)]
    names_by_uid: HashMap<libc::uid_t, String>,
    #[cfg(windows)]
    names_by_pid: HashMap<u32, String>,
}

#[cfg(windows)]
type RawHandle = *mut c_void;

#[cfg(windows)]
type RawSid = *mut c_void;

#[cfg(windows)]
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

#[cfg(windows)]
const TOKEN_QUERY: u32 = 0x0008;

#[cfg(windows)]
const TOKEN_USER_CLASS: u32 = 1;

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct SidAndAttributes {
    sid: RawSid,
    attributes: u32,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct TokenUser {
    user: SidAndAttributes,
}

#[cfg(windows)]
struct OwnedHandle(RawHandle);

#[cfg(windows)]
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> RawHandle;
    fn CloseHandle(object: RawHandle) -> i32;
}

#[cfg(windows)]
#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(
        process_handle: RawHandle,
        desired_access: u32,
        token_handle: *mut RawHandle,
    ) -> i32;
    fn GetTokenInformation(
        token_handle: RawHandle,
        token_information_class: u32,
        token_information: *mut c_void,
        token_information_length: u32,
        return_length: *mut u32,
    ) -> i32;
    fn LookupAccountSidW(
        system_name: *const u16,
        sid: RawSid,
        name: *mut u16,
        name_len: *mut u32,
        domain_name: *mut u16,
        domain_len: *mut u32,
        sid_name_use: *mut u32,
    ) -> i32;
}

struct CollectContext<'a> {
    sys: &'a System,
    user_resolver: &'a mut UserResolver,
    container_map: &'a ContainerPortMap,
    tcp_states: &'a TcpStateIndex,
    now_epoch: u64,
    project_cache: &'a mut HashMap<PathBuf, Option<PathBuf>>,
    home: Option<&'a Path>,
}

/// Collect all open TCP and UDP sockets on the system.
///
/// Returns a `Vec<PortEntry>` sorted by port number in ascending order.
/// Entries where the PID or username cannot be resolved are still included
/// with placeholder values.
///
/// Repeated rows from the same PID are collapsed. Known Docker proxy
/// duplicates (for example Docker Desktop binding both IPv4 and IPv6)
/// are collapsed as well, but distinct non-proxy PIDs remain visible.
pub fn collect() -> Result<Vec<PortEntry>> {
    // Start Docker/Podman detection early so it runs concurrently with
    // the OS-level socket enumeration and process metadata refresh.
    let docker_handle = docker::start_detection();

    let raw_listeners = listeners::get_all()
        .map_err(|e| anyhow::anyhow!("failed to enumerate open sockets from the OS: {e}"))?;

    let mut sys = System::new();
    let tracked_pids: Vec<_> = raw_listeners
        .iter()
        .map(|listener| sysinfo::Pid::from_u32(listener.process.pid))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    // `false` = do not remove previously-tracked dead processes. On a
    // freshly created System the internal map is empty, so this flag
    // has no effect either way. Passing `false` avoids the slightly
    // more expensive "clean up stale entries" pass.
    if !tracked_pids.is_empty() {
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&tracked_pids),
            false,
            process_refresh_kind(),
        );
    }

    let mut user_resolver = UserResolver::default();

    // Block on Docker results only after all other I/O is done.
    let container_map = docker::await_detection(docker_handle);
    let tcp_states = load_tcp_state_index();

    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut project_cache: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();

    // Resolve the home directory once so that every per-process
    // invocation of find_from_dir does not each query the OS environment.
    let home = project::home_dir();
    let mut context = CollectContext {
        sys: &sys,
        user_resolver: &mut user_resolver,
        container_map: &container_map,
        tcp_states: &tcp_states,
        now_epoch,
        project_cache: &mut project_cache,
        home: home.as_deref(),
    };

    let all_entries: Vec<PortEntry> = raw_listeners
        .into_iter()
        .map(|l| build_entry(&l, &mut context))
        .collect();

    let mut entries = deduplicate(all_entries);
    entries.sort_by(|left, right| {
        (
            left.port,
            left.local_addr,
            left.proto,
            left.pid,
            left.process.as_str(),
        )
            .cmp(&(
                right.port,
                right.local_addr,
                right.proto,
                right.pid,
                right.process.as_str(),
            ))
    });
    Ok(entries)
}

/// Build a single [`PortEntry`] from a [`listeners::Listener`], enriching it
/// with Docker, project, framework, and uptime information.
fn build_entry(l: &listeners::Listener, context: &mut CollectContext<'_>) -> PortEntry {
    let proto = match l.protocol {
        listeners::Protocol::TCP => Protocol::Tcp,
        listeners::Protocol::UDP => Protocol::Udp,
    };

    let state = resolve_state(l, context.tcp_states);

    let sysinfo_pid = sysinfo::Pid::from_u32(l.process.pid);
    let sysinfo_process = context.sys.process(sysinfo_pid);
    let user = resolve_user(sysinfo_process, l.process.pid, context.user_resolver);

    // Docker container lookup
    let container = lookup_container(context.container_map, l.socket, proto, &l.process.name);

    // Project detection: use container name for Docker ports, otherwise walk cwd.
    // The cache avoids redundant directory walks for processes sharing a cwd.
    let (project_name, project_root) = container.map_or_else(
        || {
            let cwd = sysinfo_process.and_then(sysinfo::Process::cwd);
            let cmd = sysinfo_process.map_or(&[][..], sysinfo::Process::cmd);
            let root = lookup_project_root(cwd, cmd, context.project_cache, context.home);
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
        if start > 0 && context.now_epoch > start {
            Some(context.now_epoch - start)
        } else {
            None
        }
    });

    PortEntry {
        port: l.socket.port(),
        local_addr: l.socket.ip(),
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

fn lookup_container<'a>(
    container_map: &'a ContainerPortMap,
    socket: SocketAddr,
    proto: Protocol,
    process_name: &str,
) -> Option<&'a docker::ContainerInfo> {
    if let Some(container) = container_map
        .get(&(Some(socket.ip()), socket.port(), proto))
        .or_else(|| container_map.get(&(None, socket.port(), proto)))
    {
        return Some(container);
    }

    if !is_docker_proxy_process(process_name) {
        return None;
    }

    let mut candidate = None;

    for ((_, port, key_proto), info) in container_map {
        if *port != socket.port() || *key_proto != proto {
            continue;
        }

        match candidate {
            None => candidate = Some(info),
            Some(existing) if existing == info => {}
            Some(_) => return None,
        }
    }

    candidate
}

/// Resolve the best-known TCP state for a listener entry.
fn resolve_state(l: &listeners::Listener, tcp_states: &TcpStateIndex) -> State {
    match l.protocol {
        listeners::Protocol::TCP => tcp_states.get(&l.socket).copied().unwrap_or(State::Unknown),
        listeners::Protocol::UDP => State::NotApplicable,
    }
}

/// Load a best-effort index of TCP socket states keyed by local socket.
///
/// Because the `listeners` crate exposes only the local socket, state
/// aggregation is necessarily port-centric: exact matches are preserved,
/// mixed non-listener states become `UNKNOWN`, and `LISTEN` wins when a
/// listening socket shares the same local address and port.
#[cfg(target_os = "linux")]
fn load_tcp_state_index() -> TcpStateIndex {
    let mut index = HashMap::new();
    extend_linux_tcp_state_index("/proc/net/tcp", false, &mut index);
    extend_linux_tcp_state_index("/proc/net/tcp6", true, &mut index);
    index
}

/// Load a best-effort index of TCP socket states keyed by local socket.
///
/// Because the `listeners` crate exposes only the local socket, state
/// aggregation is necessarily port-centric: exact matches are preserved,
/// mixed non-listener states become `UNKNOWN`, and `LISTEN` wins when a
/// listening socket shares the same local address and port.
#[cfg(windows)]
fn load_tcp_state_index() -> TcpStateIndex {
    let mut index = HashMap::new();
    if let Some(table) = read_windows_tcp_table(AF_INET) {
        extend_windows_tcpv4_state_index(&table, &mut index);
    }
    if let Some(table) = read_windows_tcp_table(AF_INET6) {
        extend_windows_tcpv6_state_index(&table, &mut index);
    }
    index
}

/// Load a best-effort index of TCP socket states keyed by local socket.
#[cfg(not(any(target_os = "linux", windows)))]
fn load_tcp_state_index() -> TcpStateIndex {
    HashMap::new()
}

#[cfg(target_os = "linux")]
fn extend_linux_tcp_state_index(path: &str, ipv6: bool, index: &mut TcpStateIndex) {
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };

    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let parsed = if ipv6 {
            parse_linux_tcp6_table_entry(&line)
        } else {
            parse_linux_tcp_table_entry(&line)
        };

        if let Some((socket, state)) = parsed {
            merge_tcp_state(index, socket, state);
        }
    }
}

#[cfg(target_os = "linux")]
fn parse_linux_tcp_table_entry(line: &str) -> Option<(SocketAddr, State)> {
    let mut parts = line.split_whitespace();
    let _slot = parts.next()?;
    let local_addr_hex = parts.next()?;
    let _remote_addr_hex = parts.next()?;
    let state_hex = parts.next()?;

    let (ip_hex, port_hex) = local_addr_hex.split_once(':')?;
    let ip = Ipv4Addr::from(u32::from_be(u32::from_str_radix(ip_hex, 16).ok()?));
    let port = u16::from_str_radix(port_hex, 16).ok()?;

    Some((
        SocketAddr::new(IpAddr::V4(ip), port),
        state_from_linux_code(state_hex),
    ))
}

#[cfg(target_os = "linux")]
fn parse_linux_tcp6_table_entry(line: &str) -> Option<(SocketAddr, State)> {
    #[cfg(target_endian = "little")]
    let read_endian = u32::from_le_bytes;
    #[cfg(target_endian = "big")]
    let read_endian = u32::from_be_bytes;

    let mut parts = line.split_whitespace();
    let _slot = parts.next()?;
    let local_addr_hex = parts.next()?;
    let _remote_addr_hex = parts.next()?;
    let state_hex = parts.next()?;

    let (ip_hex, port_hex) = local_addr_hex.split_once(':')?;
    if ip_hex.len() != 32 {
        return None;
    }

    let mut bytes = [0_u8; 16];
    for (index, slot) in bytes.iter_mut().enumerate() {
        let offset = index * 2;
        *slot = u8::from_str_radix(&ip_hex[offset..offset + 2], 16).ok()?;
    }

    let ip_a = read_endian(bytes[0..4].try_into().ok()?);
    let ip_b = read_endian(bytes[4..8].try_into().ok()?);
    let ip_c = read_endian(bytes[8..12].try_into().ok()?);
    let ip_d = read_endian(bytes[12..16].try_into().ok()?);
    let ip = Ipv6Addr::new(
        ((ip_a >> 16) & 0xffff) as u16,
        (ip_a & 0xffff) as u16,
        ((ip_b >> 16) & 0xffff) as u16,
        (ip_b & 0xffff) as u16,
        ((ip_c >> 16) & 0xffff) as u16,
        (ip_c & 0xffff) as u16,
        ((ip_d >> 16) & 0xffff) as u16,
        (ip_d & 0xffff) as u16,
    );
    let port = u16::from_str_radix(port_hex, 16).ok()?;

    Some((
        SocketAddr::new(IpAddr::V6(ip), port),
        state_from_linux_code(state_hex),
    ))
}

#[cfg(windows)]
const AF_INET: u32 = 2;
#[cfg(windows)]
const AF_INET6: u32 = 23;
#[cfg(windows)]
const TCP_TABLE_OWNER_PID_ALL: u32 = 5;
#[cfg(windows)]
const ERROR_INSUFFICIENT_BUFFER: u32 = 0x7A;
#[cfg(windows)]
const NO_ERROR: u32 = 0;
#[cfg(windows)]
const WINDOWS_TCP4_ROW_SIZE: usize = 24;
#[cfg(windows)]
const WINDOWS_TCP6_ROW_SIZE: usize = 56;

#[cfg(windows)]
#[link(name = "iphlpapi")]
unsafe extern "system" {
    #[link_name = "GetExtendedTcpTable"]
    fn get_extended_tcp_table(
        tcp_table: *mut std::ffi::c_void,
        size: *mut u32,
        order: i32,
        address_family: u32,
        table_class: u32,
        reserved: u32,
    ) -> u32;
}

#[cfg(windows)]
fn read_windows_tcp_table(address_family: u32) -> Option<Vec<u8>> {
    let mut attempts = 0;

    loop {
        let mut size = 0_u32;
        let initial = unsafe {
            get_extended_tcp_table(
                std::ptr::null_mut(),
                &raw mut size,
                0,
                address_family,
                TCP_TABLE_OWNER_PID_ALL,
                0,
            )
        };

        if initial != ERROR_INSUFFICIENT_BUFFER {
            return None;
        }

        let Ok(buffer_len) = usize::try_from(size) else {
            return None;
        };
        let mut buffer = vec![0_u8; buffer_len];
        let result = unsafe {
            get_extended_tcp_table(
                buffer.as_mut_ptr().cast(),
                &raw mut size,
                0,
                address_family,
                TCP_TABLE_OWNER_PID_ALL,
                0,
            )
        };

        if result == NO_ERROR {
            return Some(buffer);
        }

        attempts += 1;
        if result != ERROR_INSUFFICIENT_BUFFER || attempts >= 3 {
            return None;
        }
    }
}

#[cfg(windows)]
fn extend_windows_tcpv4_state_index(table: &[u8], index: &mut TcpStateIndex) {
    let Some(rows_count) = windows_rows_count(table) else {
        return;
    };

    for row in table[4..]
        .chunks_exact(WINDOWS_TCP4_ROW_SIZE)
        .take(rows_count)
    {
        let Some(state_code) = read_u32_ne(row, 0) else {
            continue;
        };
        let Some(local_addr) = read_u32_ne(row, 4) else {
            continue;
        };
        let Some(port) = read_windows_port(row, 8) else {
            continue;
        };

        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::from(u32::from_be(local_addr))), port);
        merge_tcp_state(index, socket, state_from_windows_code(state_code));
    }
}

#[cfg(windows)]
fn extend_windows_tcpv6_state_index(table: &[u8], index: &mut TcpStateIndex) {
    let Some(rows_count) = windows_rows_count(table) else {
        return;
    };

    for row in table[4..]
        .chunks_exact(WINDOWS_TCP6_ROW_SIZE)
        .take(rows_count)
    {
        let Some(state_code) = read_u32_ne(row, 48) else {
            continue;
        };
        let Some(local_addr_bytes) = row.get(0..16) else {
            continue;
        };
        let Some(port) = read_windows_port(row, 20) else {
            continue;
        };
        let Ok(local_addr) = <[u8; 16]>::try_from(local_addr_bytes) else {
            continue;
        };

        let socket = SocketAddr::new(IpAddr::V6(Ipv6Addr::from(local_addr)), port);
        merge_tcp_state(index, socket, state_from_windows_code(state_code));
    }
}

#[cfg(windows)]
fn windows_rows_count(table: &[u8]) -> Option<usize> {
    usize::try_from(read_u32_ne(table, 0)?).ok()
}

#[cfg(windows)]
fn read_u32_ne(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let raw = bytes.get(offset..end)?;
    let array: [u8; 4] = raw.try_into().ok()?;
    Some(u32::from_ne_bytes(array))
}

#[cfg(windows)]
fn read_windows_port(bytes: &[u8], offset: usize) -> Option<u16> {
    let end = offset.checked_add(2)?;
    let raw = bytes.get(offset..end)?;
    let array: [u8; 2] = raw.try_into().ok()?;
    Some(u16::from_be_bytes(array))
}

fn merge_tcp_state(index: &mut TcpStateIndex, socket: SocketAddr, state: State) {
    use std::collections::hash_map::Entry;

    match index.entry(socket) {
        Entry::Occupied(mut slot) => {
            slot.insert(merge_state(*slot.get(), state));
        }
        Entry::Vacant(slot) => {
            slot.insert(state);
        }
    }
}

fn merge_state(current: State, next: State) -> State {
    if current == next {
        return current;
    }

    if current == State::Unknown {
        return next;
    }
    if next == State::Unknown {
        return current;
    }

    if current == State::Listen || next == State::Listen {
        return State::Listen;
    }

    State::Unknown
}

#[cfg(any(test, target_os = "linux"))]
fn state_from_linux_code(code: &str) -> State {
    match code.to_ascii_uppercase().as_str() {
        "01" => State::Established,
        "02" => State::SynSent,
        "03" => State::SynReceived,
        "04" => State::FinWait1,
        "05" => State::FinWait2,
        "06" => State::TimeWait,
        "07" => State::Close,
        "08" => State::CloseWait,
        "09" => State::LastAck,
        "0A" => State::Listen,
        "0B" => State::Closing,
        "0C" => State::NewSynReceived,
        _ => State::Unknown,
    }
}

#[cfg(any(test, windows))]
const fn state_from_windows_code(code: u32) -> State {
    match code {
        2 => State::Listen,
        3 => State::SynSent,
        4 => State::SynReceived,
        5 => State::Established,
        6 => State::FinWait1,
        7 => State::FinWait2,
        8 => State::CloseWait,
        9 => State::Closing,
        10 => State::LastAck,
        11 => State::TimeWait,
        12 => State::DeleteTcb,
        _ => State::Unknown,
    }
}

/// Look up the project root for a process, using a cache to skip repeated
/// directory walks for processes that share the same working directory.
///
/// Falls back to the first absolute path found in the command-line
/// arguments when cwd-based detection fails.
///
/// Accepts `Option<&Path>` to avoid allocating a `PathBuf` for every
/// process on the cache-hit path. A `PathBuf` is only allocated on a
/// cache miss when inserting the result.
///
/// `home` is the user's home directory ceiling resolved once by
/// [`collect`] and passed down to avoid repeated env-var reads.
fn lookup_project_root(
    cwd: Option<&Path>,
    cmd: &[OsString],
    cache: &mut HashMap<PathBuf, Option<PathBuf>>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(cwd_path) = cwd
        && let Some(root) = lookup_cached_project_root(cwd_path, cache, home)
    {
        return Some(root);
    }

    for cmd_path in project::absolute_cmd_parents(cmd) {
        if let Some(root) = lookup_cached_project_root(cmd_path, cache, home) {
            return Some(root);
        }
    }

    None
}

fn lookup_cached_project_root(
    start: &Path,
    cache: &mut HashMap<PathBuf, Option<PathBuf>>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    let mut visited = Vec::new();

    for _ in 0..project::MAX_WALK_DEPTH {
        if let Some(h) = home
            && current == h
        {
            break;
        }

        if let Some(cached) = cache.get(&current).cloned() {
            for path in visited {
                cache.insert(path, cached.clone());
            }
            return cached;
        }

        visited.push(current.clone());

        if project::has_marker(&current) {
            let result = Some(current.clone());
            for path in visited {
                cache.insert(path, result.clone());
            }
            return result;
        }

        if !current.pop() {
            break;
        }
    }

    for path in visited {
        cache.insert(path, None);
    }

    None
}

/// Deduplicate entries that share the same user-visible logical socket.
///
/// On Windows with Docker Desktop (WSL2), the OS reports multiple sockets
/// for the same Docker-published port (for example `wslrelay.exe` on IPv4
/// and `com.docker.backend.exe` on IPv4 and IPv6). This collapses repeated
/// rows from the same PID and then removes known Docker proxy duplicates
/// while preserving distinct non-proxy worker processes.
fn deduplicate(entries: Vec<PortEntry>) -> Vec<PortEntry> {
    let mut grouped: HashMap<(u16, IpAddr, Protocol, State), Vec<PortEntry>> = HashMap::new();

    for entry in entries {
        let key = (entry.port, entry.local_addr, entry.proto, entry.state);
        grouped.entry(key).or_default().push(entry);
    }

    let mut deduplicated = Vec::new();

    for group in grouped.into_values() {
        deduplicated.extend(deduplicate_group(group));
    }

    deduplicated
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

    let mut best_by_pid: HashMap<u32, PortEntry> = HashMap::new();

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

fn is_docker_proxy_process(process_name: &str) -> bool {
    const DOCKER_PROXY_PROCESSES: &[&str] =
        &["wslrelay", "com.docker.backend", "vpnkit", "docker-proxy"];

    let name = strip_windows_exe_suffix(process_name);
    DOCKER_PROXY_PROCESSES
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

fn strip_windows_exe_suffix(process_name: &str) -> &str {
    let Some(prefix_len) = process_name.len().checked_sub(4) else {
        return process_name;
    };

    match process_name.get(prefix_len..) {
        Some(suffix) if suffix.eq_ignore_ascii_case(".exe") => {
            process_name.get(..prefix_len).unwrap_or(process_name)
        }
        _ => process_name,
    }
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
#[cfg(unix)]
fn resolve_user(
    process: Option<&sysinfo::Process>,
    _pid: u32,
    resolver: &mut UserResolver,
) -> String {
    let Some(proc_ref) = process else {
        return "-".to_string();
    };

    let Some(uid) = proc_ref.user_id() else {
        return "-".to_string();
    };

    let uid = *uid.deref();
    if let Some(cached) = resolver.names_by_uid.get(&uid) {
        return cached.clone();
    }

    let name = lookup_unix_username(uid).unwrap_or_else(|| "-".to_string());
    resolver.names_by_uid.insert(uid, name.clone());
    name
}

#[cfg(unix)]
fn lookup_unix_username(uid: libc::uid_t) -> Option<String> {
    let mut buffer_len = match unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) } {
        suggested if suggested > 0 => usize::try_from(suggested).ok()?,
        _ => 1024,
    };

    loop {
        let mut password = MaybeUninit::<libc::passwd>::uninit();
        let mut buffer = vec![0_u8; buffer_len];
        let mut result = std::ptr::null_mut();

        let status = unsafe {
            libc::getpwuid_r(
                uid,
                password.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };

        if status == 0 {
            if result.is_null() {
                return None;
            }

            let password = unsafe { password.assume_init() };
            if password.pw_name.is_null() {
                return None;
            }

            let name = unsafe { CStr::from_ptr(password.pw_name) }
                .to_str()
                .ok()?
                .to_string();
            return Some(name);
        }

        if status != libc::ERANGE {
            return None;
        }

        buffer_len = buffer_len.saturating_mul(2);
        if buffer_len > 1024 * 1024 {
            return None;
        }
    }
}

#[cfg(windows)]
fn resolve_user(
    _process: Option<&sysinfo::Process>,
    pid: u32,
    resolver: &mut UserResolver,
) -> String {
    if let Some(cached) = resolver.names_by_pid.get(&pid) {
        return cached.clone();
    }

    let name = lookup_windows_username(pid).unwrap_or_else(|| "-".to_string());
    resolver.names_by_pid.insert(pid, name.clone());
    name
}

#[cfg(windows)]
fn lookup_windows_username(pid: u32) -> Option<String> {
    let process = open_process_for_query(pid)?;
    let token = open_process_token(process.0)?;
    let token_user = read_token_user(token.0)?;
    let token_user = unsafe { std::ptr::read_unaligned(token_user.as_ptr().cast::<TokenUser>()) };
    lookup_account_name(token_user.user.sid)
}

#[cfg(windows)]
fn open_process_for_query(pid: u32) -> Option<OwnedHandle> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    (!handle.is_null()).then_some(OwnedHandle(handle))
}

#[cfg(windows)]
fn open_process_token(process: RawHandle) -> Option<OwnedHandle> {
    let mut token = ptr::null_mut();
    let success = unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) };
    (success != 0 && !token.is_null()).then_some(OwnedHandle(token))
}

#[cfg(windows)]
fn read_token_user(token: RawHandle) -> Option<Vec<u8>> {
    let mut required_len = 0;
    unsafe {
        let _ = GetTokenInformation(
            token,
            TOKEN_USER_CLASS,
            ptr::null_mut(),
            0,
            &raw mut required_len,
        );
    }
    if required_len == 0 {
        return None;
    }

    let mut buffer = vec![0_u8; required_len as usize];
    let success = unsafe {
        GetTokenInformation(
            token,
            TOKEN_USER_CLASS,
            buffer.as_mut_ptr().cast(),
            required_len,
            &raw mut required_len,
        )
    };
    (success != 0).then_some(buffer)
}

#[cfg(windows)]
fn lookup_account_name(sid: RawSid) -> Option<String> {
    let mut name_len = 0;
    let mut domain_len = 0;
    let mut sid_name_use = 0;

    unsafe {
        let _ = LookupAccountSidW(
            ptr::null(),
            sid,
            ptr::null_mut(),
            &raw mut name_len,
            ptr::null_mut(),
            &raw mut domain_len,
            &raw mut sid_name_use,
        );
    }
    if name_len == 0 {
        return None;
    }

    let mut name = vec![0_u16; name_len as usize];
    let success = unsafe {
        LookupAccountSidW(
            ptr::null(),
            sid,
            name.as_mut_ptr(),
            &raw mut name_len,
            ptr::null_mut(),
            &raw mut domain_len,
            &raw mut sid_name_use,
        )
    };
    if success == 0 {
        return None;
    }

    Some(wide_buffer_to_string(&name))
}

#[cfg(windows)]
fn wide_buffer_to_string(buffer: &[u16]) -> String {
    let end = buffer
        .iter()
        .position(|&value| value == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

#[cfg(not(any(unix, windows)))]
fn resolve_user(
    _process: Option<&sysinfo::Process>,
    _pid: u32,
    _resolver: &mut UserResolver,
) -> String {
    "-".to_string()
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
    use std::fs;
    use tempfile::TempDir;

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
    fn dedup_prefers_enriched_entry() {
        let mut bare = make_entry(5432, Protocol::Tcp);
        bare.pid = 1001;
        bare.process = "wslrelay.exe".to_string();

        let mut enriched = make_entry(5432, Protocol::Tcp);
        enriched.pid = 1002;
        enriched.process = "com.docker.backend.exe".to_string();
        enriched.project = Some("my-postgres".to_string());
        enriched.app = Some("PostgreSQL");
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
        first.app = Some("Nginx");

        let mut second = make_entry(8080, Protocol::Tcp);
        second.pid = 1002;
        second.process = "nginx".to_string();
        second.app = Some("Nginx");

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
        proxy.app = Some("PostgreSQL");

        let mut real = make_entry(5432, Protocol::Tcp);
        real.pid = 1002;
        real.process = "postgres".to_string();
        real.app = Some("PostgreSQL");

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
        entry.app = Some("App");
        entry.uptime_secs = Some(100);
        entry.user = "admin".to_string();
        assert_eq!(enrichment_score(&entry), 6, "fully enriched should score 6");
    }

    #[test]
    fn linux_state_codes_match_expected_values() {
        assert_eq!(state_from_linux_code("01"), State::Established);
        assert_eq!(state_from_linux_code("0A"), State::Listen);
        assert_eq!(state_from_linux_code("0C"), State::NewSynReceived);
    }

    #[test]
    fn windows_state_codes_match_expected_values() {
        assert_eq!(state_from_windows_code(2), State::Listen);
        assert_eq!(state_from_windows_code(5), State::Established);
        assert_eq!(state_from_windows_code(12), State::DeleteTcb);
    }

    #[test]
    fn merge_state_marks_conflicts_unknown() {
        assert_eq!(
            merge_state(State::Established, State::TimeWait),
            State::Unknown,
            "mixed non-listener states should become unknown instead of guessing"
        );
    }

    #[test]
    fn merge_state_prefers_listen_for_shared_local_socket() {
        assert_eq!(
            merge_state(State::Established, State::Listen),
            State::Listen,
            "a listener on the same local socket should stay visible"
        );
    }

    #[test]
    fn merge_tcp_state_keeps_listen_when_states_conflict() {
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5432);
        let mut index = HashMap::new();

        merge_tcp_state(&mut index, socket, State::Established);
        merge_tcp_state(&mut index, socket, State::Listen);

        assert_eq!(
            index.get(&socket).copied(),
            Some(State::Listen),
            "the aggregate state for a shared local socket should prefer LISTEN"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_port_reader_extracts_big_endian_port_bytes() {
        let row = [0x00, 0x50, 0x00, 0x00];
        assert_eq!(
            read_windows_port(&row, 0),
            Some(80),
            "network-order port bytes should decode directly"
        );
    }

    #[test]
    fn docker_proxy_process_names_are_detected_case_insensitively() {
        assert!(is_docker_proxy_process("wslrelay.exe"));
        assert!(is_docker_proxy_process("COM.DOCKER.BACKEND.EXE"));
        assert!(is_docker_proxy_process("vpnkit"));
        assert!(!is_docker_proxy_process("nginx"));
    }

    #[test]
    fn docker_proxy_process_name_stripping_handles_non_ascii_input() {
        assert!(!is_docker_proxy_process("éabc"));
    }

    #[test]
    fn container_lookup_prefers_exact_address_matches() {
        let mut map = HashMap::new();
        map.insert(
            (Some(IpAddr::V4(Ipv4Addr::LOCALHOST)), 8080, Protocol::Tcp),
            docker::ContainerInfo {
                name: "loopback-app".to_string(),
                image: "node:22".to_string(),
            },
        );

        let exact = lookup_container(
            &map,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080),
            Protocol::Tcp,
            "node",
        );
        assert_eq!(exact.map(|info| info.name.as_str()), Some("loopback-app"));

        let mismatch = lookup_container(
            &map,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10)), 8080),
            Protocol::Tcp,
            "node",
        );
        assert!(
            mismatch.is_none(),
            "non-matching local addresses must not inherit container enrichment"
        );
    }

    #[test]
    fn container_lookup_uses_proxy_fallback_for_unique_port_mapping() {
        let mut map = HashMap::new();
        map.insert(
            (Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED)), 5432, Protocol::Tcp),
            docker::ContainerInfo {
                name: "postgres".to_string(),
                image: "postgres:16".to_string(),
            },
        );

        let container = lookup_container(
            &map,
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 5432),
            Protocol::Tcp,
            "wslrelay.exe",
        );
        assert_eq!(container.map(|info| info.name.as_str()), Some("postgres"));
    }

    #[test]
    fn project_root_cache_learns_visited_ancestors() {
        let root = TempDir::new().unwrap();
        fs::write(root.path().join("Cargo.toml"), "").unwrap();

        let first = root.path().join("src").join("db");
        let second = root.path().join("src").join("utils");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();

        let mut cache = HashMap::new();

        let first_result = lookup_cached_project_root(&first, &mut cache, None);
        assert_eq!(first_result.as_deref(), Some(root.path()));
        assert_eq!(
            cache.get(first.as_path()).and_then(Option::as_deref),
            Some(root.path()),
            "the original cwd should be cached"
        );
        assert_eq!(
            cache
                .get(first.parent().unwrap())
                .and_then(Option::as_deref),
            Some(root.path()),
            "visited ancestors should also be cached"
        );

        let second_result = lookup_cached_project_root(&second, &mut cache, None);
        assert_eq!(second_result.as_deref(), Some(root.path()));
        assert_eq!(
            cache.get(second.as_path()).and_then(Option::as_deref),
            Some(root.path()),
            "sibling directories should learn from the cached ancestor"
        );
    }

    #[test]
    fn project_root_cache_does_not_poison_unrelated_ancestors() {
        let workspace = TempDir::new().unwrap();
        let outer = workspace.path().join("workspace");
        let project_root = outer.join("app");
        let inside = project_root.join("src").join("db");
        let unrelated = outer.join("services").join("worker");

        fs::create_dir_all(&inside).unwrap();
        fs::create_dir_all(&unrelated).unwrap();
        fs::write(project_root.join("Cargo.toml"), "").unwrap();

        let mut cache = HashMap::new();

        let first_result = lookup_cached_project_root(&inside, &mut cache, None);
        assert_eq!(first_result.as_deref(), Some(project_root.as_path()));
        assert!(
            !cache.contains_key(outer.as_path()),
            "ancestors above the discovered project root must not be cached as project hits"
        );

        let unrelated_result = lookup_cached_project_root(&unrelated, &mut cache, None);
        assert!(
            unrelated_result.is_none(),
            "an unrelated path under the same ancestor must not inherit another project's root"
        );
    }
}
