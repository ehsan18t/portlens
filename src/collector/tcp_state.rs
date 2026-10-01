//! OS-specific TCP connection state polling.
//!
//! Returns a populated [`TcpStateIndex`] by reading the kernel TCP table:
//!
//! - **Linux**: parses `/proc/net/tcp` and `/proc/net/tcp6`.
//! - **Windows**: calls `GetExtendedTcpTable` via FFI.
//! - **Other**: returns an empty index (state enrichment unavailable).
//!
//! Windows rows carry the owning PID, and `listeners` reports one socket per
//! `(local address, pid)` pair, so Windows states are keyed by both. Linux
//! rows only carry an inode, so Linux states stay keyed by local address.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::types::State;
use log::debug;

/// One slot in the [`TcpStateIndex`]: a local socket plus the owning PID when
/// the OS table reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TcpStateKey {
    socket: SocketAddr,
    pid: Option<u32>,
}

/// Aggregated TCP states keyed by local socket and, where available, owning PID.
///
/// PID-keyed slots (Windows) only ever answer lookups for that exact PID, so a
/// row owned by one process can never relabel a socket owned by another.
/// PID-agnostic slots (Linux) answer lookups for any PID on that socket.
#[derive(Debug, Default)]
pub(super) struct TcpStateIndex {
    states: HashMap<TcpStateKey, State>,
}

impl TcpStateIndex {
    /// Fold one kernel table row into the index.
    ///
    /// Rows that share a slot are combined with [`merge_state`].
    pub(super) fn merge(&mut self, socket: SocketAddr, pid: Option<u32>, state: State) {
        use std::collections::hash_map::Entry;

        match self.states.entry(TcpStateKey { socket, pid }) {
            Entry::Occupied(mut slot) => {
                slot.insert(merge_state(*slot.get(), state));
            }
            Entry::Vacant(slot) => {
                slot.insert(state);
            }
        }
    }

    /// Return the TCP state for the socket owned by `pid`.
    ///
    /// An exact `(socket, pid)` slot wins; otherwise a PID-agnostic slot for
    /// the socket is used. Returns `None` when neither exists.
    pub(super) fn lookup(&self, socket: SocketAddr, pid: u32) -> Option<State> {
        self.states
            .get(&TcpStateKey {
                socket,
                pid: Some(pid),
            })
            .or_else(|| self.states.get(&TcpStateKey { socket, pid: None }))
            .copied()
    }
}

// ---------------------------------------------------------------------------
// Linux: /proc/net/tcp{,6}
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
pub(super) fn load_tcp_state_index() -> TcpStateIndex {
    let mut index = TcpStateIndex::default();
    extend_linux_tcp_state_index("/proc/net/tcp", false, &mut index);
    extend_linux_tcp_state_index("/proc/net/tcp6", true, &mut index);
    debug!(
        "loaded linux tcp state index: entries={}",
        index.states.len()
    );
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

        // /proc/net/tcp exposes an inode rather than a PID, so Linux slots
        // stay PID-agnostic.
        if let Some((socket, state)) = parsed {
            index.merge(socket, None, state);
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

/// Byte layout of one `GetExtendedTcpTable` `OWNER_PID` row.
#[cfg(any(test, windows))]
struct WindowsRowLayout {
    size: usize,
    state_offset: usize,
    pid_offset: usize,
    socket_from_row: fn(&[u8]) -> Option<SocketAddr>,
}

/// `MIB_TCPROW_OWNER_PID`: state, local addr, local port, remote addr,
/// remote port, owning PID (six `u32` fields).
#[cfg(any(test, windows))]
const WINDOWS_TCP4_LAYOUT: WindowsRowLayout = WindowsRowLayout {
    size: 24,
    state_offset: 0,
    pid_offset: 20,
    socket_from_row: windows_tcpv4_socket,
};

/// `MIB_TCP6ROW_OWNER_PID`: local addr (16), local scope id, local port,
/// remote addr (16), remote scope id, remote port, state, owning PID.
#[cfg(any(test, windows))]
const WINDOWS_TCP6_LAYOUT: WindowsRowLayout = WindowsRowLayout {
    size: 56,
    state_offset: 48,
    pid_offset: 52,
    socket_from_row: windows_tcpv6_socket,
};

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
    let mut index = TcpStateIndex::default();
    if let Some(table) = read_windows_tcp_table(AF_INET) {
        extend_windows_tcp_state_index(&table, &WINDOWS_TCP4_LAYOUT, &mut index);
    }
    if let Some(table) = read_windows_tcp_table(AF_INET6) {
        extend_windows_tcp_state_index(&table, &WINDOWS_TCP6_LAYOUT, &mut index);
    }
    debug!(
        "loaded windows tcp state index: entries={}",
        index.states.len()
    );
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

#[cfg(any(test, windows))]
fn extend_windows_tcp_state_index(
    table: &[u8],
    layout: &WindowsRowLayout,
    index: &mut TcpStateIndex,
) {
    let Some(rows_count) = windows_rows_count(table) else {
        return;
    };

    for row in table[4..].chunks_exact(layout.size).take(rows_count) {
        let Some((socket, pid, state)) = parse_windows_tcp_row(row, layout) else {
            continue;
        };
        // PID 0 ([System Process]) only owns orphaned TIME_WAIT rows and never
        // a real listener, so it must not surface as a killable LISTEN socket.
        if pid == 0 && state == State::Listen {
            continue;
        }
        index.merge(socket, Some(pid), state);
    }
}

#[cfg(any(test, windows))]
fn parse_windows_tcp_row(
    row: &[u8],
    layout: &WindowsRowLayout,
) -> Option<(SocketAddr, u32, State)> {
    let state_code = read_u32_ne(row, layout.state_offset)?;
    let pid = read_u32_ne(row, layout.pid_offset)?;
    let socket = (layout.socket_from_row)(row)?;
    Some((socket, pid, state_from_windows_code(state_code)))
}

#[cfg(any(test, windows))]
fn windows_tcpv4_socket(row: &[u8]) -> Option<SocketAddr> {
    let local_addr = read_u32_ne(row, 4)?;
    let port = read_windows_port(row, 8)?;
    Some(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::from(u32::from_be(local_addr))),
        port,
    ))
}

#[cfg(any(test, windows))]
fn windows_tcpv6_socket(row: &[u8]) -> Option<SocketAddr> {
    let local_addr_bytes = row.get(0..16)?;
    let port = read_windows_port(row, 20)?;
    let local_addr = <[u8; 16]>::try_from(local_addr_bytes).ok()?;
    Some(SocketAddr::new(
        IpAddr::V6(Ipv6Addr::from(local_addr)),
        port,
    ))
}

#[cfg(any(test, windows))]
fn windows_rows_count(table: &[u8]) -> Option<usize> {
    usize::try_from(read_u32_ne(table, 0)?).ok()
}

#[cfg(any(test, windows))]
fn read_u32_ne(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let raw = bytes.get(offset..end)?;
    let array: [u8; 4] = raw.try_into().ok()?;
    Some(u32::from_ne_bytes(array))
}

#[cfg(any(test, windows))]
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
    debug!("tcp state enrichment unavailable on this platform");
    TcpStateIndex::default()
}

// ---------------------------------------------------------------------------
// Shared state merging
// ---------------------------------------------------------------------------

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
    fn pid_agnostic_merge_keeps_listen_when_states_conflict() {
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5432);
        let mut index = TcpStateIndex::default();

        index.merge(socket, None, State::Established);
        index.merge(socket, None, State::Listen);

        assert_eq!(
            index.lookup(socket, 1234),
            Some(State::Listen),
            "the aggregate state for a shared local socket should prefer LISTEN"
        );
    }

    #[test]
    fn pid_agnostic_slot_answers_lookups_for_any_pid() {
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 22);
        let mut index = TcpStateIndex::default();
        index.merge(socket, None, State::Listen);

        assert_eq!(
            index.lookup(socket, 42),
            Some(State::Listen),
            "Linux slots carry no PID and must keep serving every PID on the socket"
        );
    }

    // Synthetic GetExtendedTcpTable rows. Field encodings follow the Win32
    // docs: dwState and dwOwningPid are host-order u32, dwLocalAddr holds
    // the address in network order, and dwLocalPort keeps the port in
    // network order in its low two bytes.

    const STATE_LISTEN: u32 = 2;
    const STATE_ESTABLISHED: u32 = 5;
    const STATE_CLOSE_WAIT: u32 = 8;
    const STATE_TIME_WAIT: u32 = 11;

    fn tcpv4_row(state_code: u32, ip: Ipv4Addr, port: u16, pid: u32) -> Vec<u8> {
        let mut row = vec![0_u8; WINDOWS_TCP4_LAYOUT.size];
        row[0..4].copy_from_slice(&state_code.to_ne_bytes());
        row[4..8].copy_from_slice(&ip.octets());
        row[8..10].copy_from_slice(&port.to_be_bytes());
        row[20..24].copy_from_slice(&pid.to_ne_bytes());
        row
    }

    fn tcpv6_row(state_code: u32, ip: Ipv6Addr, port: u16, pid: u32) -> Vec<u8> {
        let mut row = vec![0_u8; WINDOWS_TCP6_LAYOUT.size];
        row[0..16].copy_from_slice(&ip.octets());
        row[20..22].copy_from_slice(&port.to_be_bytes());
        row[48..52].copy_from_slice(&state_code.to_ne_bytes());
        row[52..56].copy_from_slice(&pid.to_ne_bytes());
        row
    }

    fn windows_table(rows: &[Vec<u8>]) -> Vec<u8> {
        let count = u32::try_from(rows.len()).unwrap();
        let mut table = count.to_ne_bytes().to_vec();
        for row in rows {
            table.extend_from_slice(row);
        }
        table
    }

    fn index_from_tcpv4_rows(rows: &[Vec<u8>]) -> TcpStateIndex {
        let mut index = TcpStateIndex::default();
        extend_windows_tcp_state_index(&windows_table(rows), &WINDOWS_TCP4_LAYOUT, &mut index);
        index
    }

    #[test]
    fn windows_tcpv4_row_parses_socket_pid_and_state() {
        let row = tcpv4_row(STATE_LISTEN, Ipv4Addr::new(192, 168, 1, 5), 8080, 4321);
        assert_eq!(
            parse_windows_tcp_row(&row, &WINDOWS_TCP4_LAYOUT),
            Some((
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5)), 8080),
                4321,
                State::Listen
            )),
            "the synthetic row encoding should round-trip through the parser"
        );
    }

    #[test]
    fn pid0_time_wait_rows_do_not_produce_listen_on_listener_address() {
        let ip = Ipv4Addr::LOCALHOST;
        let socket = SocketAddr::new(IpAddr::V4(ip), 58393);
        let index = index_from_tcpv4_rows(&[
            tcpv4_row(STATE_LISTEN, ip, 58393, 27380),
            tcpv4_row(STATE_TIME_WAIT, ip, 58393, 0),
            tcpv4_row(STATE_TIME_WAIT, ip, 58393, 0),
            tcpv4_row(STATE_TIME_WAIT, ip, 58393, 0),
        ]);

        assert_eq!(
            index.lookup(socket, 0),
            Some(State::TimeWait),
            "PID 0 rows sharing a listener's address must keep their own TIME_WAIT state"
        );
        assert_eq!(
            index.lookup(socket, 27380),
            Some(State::Listen),
            "the real listener must stay LISTEN"
        );
    }

    #[test]
    fn pid0_listen_row_is_never_indexed() {
        let ip = Ipv4Addr::LOCALHOST;
        let index = index_from_tcpv4_rows(&[tcpv4_row(STATE_LISTEN, ip, 9000, 0)]);

        assert_eq!(
            index.lookup(SocketAddr::new(IpAddr::V4(ip), 9000), 0),
            None,
            "PID 0 never owns a real listener, so it must not be labeled LISTEN"
        );
    }

    #[test]
    fn distinct_pids_on_shared_local_address_keep_their_own_state() {
        let ip = Ipv4Addr::new(10, 0, 0, 5);
        let socket = SocketAddr::new(IpAddr::V4(ip), 443);
        let index = index_from_tcpv4_rows(&[
            tcpv4_row(STATE_LISTEN, ip, 443, 300),
            tcpv4_row(STATE_ESTABLISHED, ip, 443, 100),
            tcpv4_row(STATE_CLOSE_WAIT, ip, 443, 200),
        ]);

        assert_eq!(
            index.lookup(socket, 300),
            Some(State::Listen),
            "the listener owner keeps LISTEN"
        );
        assert_eq!(
            index.lookup(socket, 100),
            Some(State::Established),
            "an accepted connection owned by another PID must not inherit LISTEN"
        );
        assert_eq!(
            index.lookup(socket, 200),
            Some(State::CloseWait),
            "ESTABLISHED and CLOSE_WAIT rows from different PIDs must not merge to UNKNOWN"
        );
    }

    #[test]
    fn pid_keyed_slots_do_not_answer_for_other_pids() {
        let ip = Ipv4Addr::LOCALHOST;
        let index = index_from_tcpv4_rows(&[tcpv4_row(STATE_LISTEN, ip, 3000, 1234)]);

        assert_eq!(
            index.lookup(SocketAddr::new(IpAddr::V4(ip), 3000), 999),
            None,
            "a PID-keyed state must not leak to a different process on the same socket"
        );
    }

    #[test]
    fn windows_tcpv6_rows_are_keyed_by_pid() {
        let ip = Ipv6Addr::LOCALHOST;
        let socket = SocketAddr::new(IpAddr::V6(ip), 8080);
        let table = windows_table(&[
            tcpv6_row(STATE_LISTEN, ip, 8080, 50),
            tcpv6_row(STATE_TIME_WAIT, ip, 8080, 0),
        ]);
        let mut index = TcpStateIndex::default();
        extend_windows_tcp_state_index(&table, &WINDOWS_TCP6_LAYOUT, &mut index);

        assert_eq!(
            index.lookup(socket, 50),
            Some(State::Listen),
            "IPv6 listener owner keeps LISTEN"
        );
        assert_eq!(
            index.lookup(socket, 0),
            Some(State::TimeWait),
            "IPv6 PID 0 rows keep TIME_WAIT"
        );
    }

    #[test]
    fn windows_table_walk_ignores_rows_beyond_buffer() {
        let ip = Ipv4Addr::LOCALHOST;
        let mut table = windows_table(&[tcpv4_row(STATE_LISTEN, ip, 7000, 7)]);
        // Claim three rows while only one is present.
        table[0..4].copy_from_slice(&3_u32.to_ne_bytes());
        let mut index = TcpStateIndex::default();
        extend_windows_tcp_state_index(&table, &WINDOWS_TCP4_LAYOUT, &mut index);

        assert_eq!(
            index.lookup(SocketAddr::new(IpAddr::V4(ip), 7000), 7),
            Some(State::Listen),
            "an overstated row count must not read past the buffer"
        );
    }

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
