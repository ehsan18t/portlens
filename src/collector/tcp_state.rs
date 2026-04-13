//! OS-specific TCP connection state polling.
//!
//! Returns a populated [`TcpStateIndex`] by reading the kernel TCP table:
//!
//! - **Linux**: parses `/proc/net/tcp` and `/proc/net/tcp6`.
//! - **Windows**: calls `GetExtendedTcpTable` via FFI.
//! - **Other**: returns an empty index (state enrichment unavailable).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::types::State;

/// Maps a local socket address to its aggregated TCP state.
pub(super) type TcpStateIndex = HashMap<SocketAddr, State>;

// ---------------------------------------------------------------------------
// Linux: /proc/net/tcp{,6}
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
pub(super) fn load_tcp_state_index() -> TcpStateIndex {
    let mut index = TcpStateIndex::new();
    extend_linux_tcp_state_index("/proc/net/tcp", false, &mut index);
    extend_linux_tcp_state_index("/proc/net/tcp6", true, &mut index);
    index
}

#[cfg(target_os = "linux")]
fn extend_linux_tcp_state_index(path: &str, ipv6: bool, index: &mut TcpStateIndex) {
    use std::io::{BufRead as _, BufReader};

    let Ok(file) = std::fs::File::open(path) else {
        return;
    };

    let mut reader = BufReader::new(file);
    let mut line = String::new();

    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        let parsed = if ipv6 {
            parse_linux_tcp6_table_entry(&line)
        } else {
            parse_linux_tcp_table_entry(&line)
        };

        if let Some((socket, state)) = parsed {
            merge_tcp_state(index, socket, state);
        }
        line.clear();
    }
}

#[cfg(target_os = "linux")]
fn tokenize_proc_tcp_line(line: &str) -> Option<(&str, &str)> {
    let mut fields = line.split_whitespace();
    let _index = fields.next()?;
    let local_addr = fields.next()?;
    let _remote_addr = fields.next()?;
    let state = fields.next()?;
    Some((local_addr, state))
}

#[cfg(target_os = "linux")]
fn parse_linux_tcp_table_entry(line: &str) -> Option<(SocketAddr, State)> {
    let (local_addr_hex, state_hex) = tokenize_proc_tcp_line(line)?;

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
    let (local_addr_hex, state_hex) = tokenize_proc_tcp_line(line)?;

    let (ip_hex, port_hex) = local_addr_hex.split_once(':')?;
    if ip_hex.len() != 32 {
        return None;
    }

    let mut bytes = [0_u8; 16];
    for (index, slot) in bytes.iter_mut().enumerate() {
        let offset = index * 2;
        *slot = u8::from_str_radix(&ip_hex[offset..offset + 2], 16).ok()?;
    }

    // /proc/net/tcp6 writes each 4-byte word in host byte order, so
    // native-endian is the correct (and only) reader on both LE and BE.
    let ip_a = u32::from_ne_bytes(bytes[0..4].try_into().ok()?);
    let ip_b = u32::from_ne_bytes(bytes[4..8].try_into().ok()?);
    let ip_c = u32::from_ne_bytes(bytes[8..12].try_into().ok()?);
    let ip_d = u32::from_ne_bytes(bytes[12..16].try_into().ok()?);
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

// ---------------------------------------------------------------------------
// Windows: GetExtendedTcpTable FFI
// ---------------------------------------------------------------------------

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
pub(super) fn load_tcp_state_index() -> TcpStateIndex {
    let mut index = TcpStateIndex::new();
    if let Some(table) = read_windows_tcp_table(AF_INET) {
        extend_windows_tcpv4_state_index(&table, &mut index);
    }
    if let Some(table) = read_windows_tcp_table(AF_INET6) {
        extend_windows_tcpv6_state_index(&table, &mut index);
    }
    index
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

        // Pad the reported size by ~20% to account for new connections
        // appearing between the size query and the actual read (TOCTOU).
        let padded_size = size.saturating_add(size / 5).max(size.saturating_add(256));
        let Ok(buffer_len) = usize::try_from(padded_size) else {
            return None;
        };
        let mut buffer = vec![0_u8; buffer_len];
        let mut actual_size = padded_size;
        let result = unsafe {
            get_extended_tcp_table(
                buffer.as_mut_ptr().cast(),
                &raw mut actual_size,
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
    extend_windows_tcp_state_index(table, WINDOWS_TCP4_ROW_SIZE, 0, index, windows_tcpv4_socket);
}

#[cfg(windows)]
fn extend_windows_tcpv6_state_index(table: &[u8], index: &mut TcpStateIndex) {
    extend_windows_tcp_state_index(
        table,
        WINDOWS_TCP6_ROW_SIZE,
        48,
        index,
        windows_tcpv6_socket,
    );
}

#[cfg(windows)]
fn extend_windows_tcp_state_index(
    table: &[u8],
    row_size: usize,
    state_offset: usize,
    index: &mut TcpStateIndex,
    socket_from_row: fn(&[u8]) -> Option<SocketAddr>,
) {
    let Some(rows_count) = windows_rows_count(table) else {
        return;
    };

    for row in table[4..].chunks_exact(row_size).take(rows_count) {
        let Some(state_code) = read_u32_ne(row, state_offset) else {
            continue;
        };
        let Some(socket) = socket_from_row(row) else {
            continue;
        };
        merge_tcp_state(index, socket, state_from_windows_code(state_code));
    }
}

#[cfg(windows)]
fn windows_tcpv4_socket(row: &[u8]) -> Option<SocketAddr> {
    let local_addr = read_u32_ne(row, 4)?;
    let port = read_windows_port(row, 8)?;
    Some(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::from(u32::from_be(local_addr))),
        port,
    ))
}

#[cfg(windows)]
fn windows_tcpv6_socket(row: &[u8]) -> Option<SocketAddr> {
    let local_addr_bytes = row.get(0..16)?;
    let port = read_windows_port(row, 20)?;
    let local_addr = <[u8; 16]>::try_from(local_addr_bytes).ok()?;
    Some(SocketAddr::new(
        IpAddr::V6(Ipv6Addr::from(local_addr)),
        port,
    ))
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

// ---------------------------------------------------------------------------
// Fallback: no TCP state enrichment
// ---------------------------------------------------------------------------

#[cfg(not(any(target_os = "linux", windows)))]
pub(super) fn load_tcp_state_index() -> TcpStateIndex {
    TcpStateIndex::new()
}

// ---------------------------------------------------------------------------
// Shared state merging
// ---------------------------------------------------------------------------

pub(super) fn merge_tcp_state(index: &mut TcpStateIndex, socket: SocketAddr, state: State) {
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
const fn state_from_linux_code(code: &str) -> State {
    let Ok(parsed) = u8::from_str_radix(code, 16) else {
        return State::Unknown;
    };
    match parsed {
        0x01 => State::Established,
        0x02 => State::SynSent,
        0x03 => State::SynReceived,
        0x04 => State::FinWait1,
        0x05 => State::FinWait2,
        0x06 => State::TimeWait,
        0x07 => State::Close,
        0x08 => State::CloseWait,
        0x09 => State::LastAck,
        0x0A => State::Listen,
        0x0B => State::Closing,
        0x0C => State::NewSynReceived,
        _ => State::Unknown,
    }
}

#[cfg(any(test, windows))]
const fn state_from_windows_code(code: u32) -> State {
    match code {
        1 => State::Close,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_state_codes_match_expected_values() {
        assert_eq!(state_from_linux_code("01"), State::Established);
        assert_eq!(state_from_linux_code("0A"), State::Listen);
        assert_eq!(state_from_linux_code("0C"), State::NewSynReceived);
    }

    #[test]
    fn windows_state_codes_match_expected_values() {
        assert_eq!(state_from_windows_code(1), State::Close);
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
}
