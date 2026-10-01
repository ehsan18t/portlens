//! Terminal capability detection.
//!
//! Detects terminal width and UTF-8 border support using platform-specific
//! APIs. Quarantines all `unsafe` FFI calls to a single submodule.

use std::io::{self, IsTerminal};

#[derive(Clone, Copy)]
enum TerminalStream {
    Stdout,
    Stderr,
}

impl TerminalStream {
    fn is_terminal(self) -> bool {
        match self {
            Self::Stdout => io::stdout().is_terminal(),
            Self::Stderr => io::stderr().is_terminal(),
        }
    }
}

pub(super) fn stdout_terminal_width() -> Option<usize> {
    terminal_width(TerminalStream::Stdout)
}

pub(super) fn stderr_terminal_width() -> Option<usize> {
    terminal_width(TerminalStream::Stderr)
}

/// Whether box-drawing borders are safe for the table on stdout.
// Const-eligible only on non-Windows, where the check is a constant `true`.
#[cfg_attr(not(windows), allow(clippy::missing_const_for_fn))]
pub(super) fn stdout_supports_utf8_borders() -> bool {
    terminal_supports_utf8_borders(TerminalStream::Stdout)
}

/// Whether box-drawing borders are safe for the tips panel on stderr.
// Const-eligible only on non-Windows, where the check is a constant `true`.
#[cfg_attr(not(windows), allow(clippy::missing_const_for_fn))]
pub(super) fn stderr_supports_utf8_borders() -> bool {
    terminal_supports_utf8_borders(TerminalStream::Stderr)
}

/// Width available on `stream`, or `None` for unlimited.
///
/// Output that is redirected to a file or pipe is never truncated: `COLUMNS`
/// is honoured only when the stream is a terminal. Shells and CI runners
/// sometimes export `COLUMNS`, and there is no way to tell that apart from a
/// deliberate setting, so trusting it for piped output could silently cut
/// process names out of `portlens | grep ...`.
fn terminal_width(stream: TerminalStream) -> Option<usize> {
    resolve_width(stream.is_terminal(), env_terminal_width, || {
        platform_terminal_width(stream)
    })
}

fn resolve_width(
    is_terminal: bool,
    columns: impl FnOnce() -> Option<usize>,
    platform: impl FnOnce() -> Option<usize>,
) -> Option<usize> {
    if !is_terminal {
        return None;
    }
    columns().or_else(platform)
}

fn env_terminal_width() -> Option<usize> {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|width| *width > 0)
}

#[cfg(unix)]
fn platform_terminal_width(stream: TerminalStream) -> Option<usize> {
    let fd = match stream {
        TerminalStream::Stdout if io::stdout().is_terminal() => libc::STDOUT_FILENO,
        TerminalStream::Stderr if io::stderr().is_terminal() => libc::STDERR_FILENO,
        TerminalStream::Stdout | TerminalStream::Stderr => return None,
    };

    let mut size = std::mem::MaybeUninit::<libc::winsize>::zeroed();
    let result = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, size.as_mut_ptr()) };
    if result != 0 {
        return None;
    }

    let size = unsafe { size.assume_init() };
    let width = usize::from(size.ws_col);
    (width > 0).then_some(width)
}

#[cfg(windows)]
fn platform_terminal_width(stream: TerminalStream) -> Option<usize> {
    #[repr(C)]
    struct Coord {
        x: i16,
        y: i16,
    }

    #[repr(C)]
    struct SmallRect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }

    #[repr(C)]
    struct ConsoleScreenBufferInfo {
        size: Coord,
        cursor_position: Coord,
        attributes: u16,
        window: SmallRect,
        maximum_window_size: Coord,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(handle: i32) -> *mut std::ffi::c_void;
        fn GetConsoleScreenBufferInfo(
            console_output: *mut std::ffi::c_void,
            console_screen_buffer_info: *mut ConsoleScreenBufferInfo,
        ) -> i32;
    }

    const INVALID_HANDLE_VALUE: isize = -1;
    const STD_OUTPUT_HANDLE: i32 = -11;
    const STD_ERROR_HANDLE: i32 = -12;

    let handle_id = match stream {
        TerminalStream::Stdout if io::stdout().is_terminal() => STD_OUTPUT_HANDLE,
        TerminalStream::Stderr if io::stderr().is_terminal() => STD_ERROR_HANDLE,
        TerminalStream::Stdout | TerminalStream::Stderr => return None,
    };

    let handle = unsafe { GetStdHandle(handle_id) };
    if handle.is_null() || handle as isize == INVALID_HANDLE_VALUE {
        return None;
    }

    let mut info = std::mem::MaybeUninit::<ConsoleScreenBufferInfo>::zeroed();
    let ok = unsafe { GetConsoleScreenBufferInfo(handle, info.as_mut_ptr()) };
    if ok == 0 {
        return None;
    }

    let info = unsafe { info.assume_init() };
    let width = i32::from(info.window.right) - i32::from(info.window.left) + 1;
    usize::try_from(width).ok().filter(|value| *value > 0)
}

#[cfg(not(any(unix, windows)))]
fn platform_terminal_width(_stream: TerminalStream) -> Option<usize> {
    None
}

/// Check whether `stream` can display UTF-8 box-drawing characters.
///
/// On Windows the check uses several heuristics (cheapest first):
///
/// 1. **Console code page** -- a code page of 65001 means the console is
///    in explicit UTF-8 mode, so even redirected output is decoded as UTF-8
///    by shells that read it with the console encoding.
/// 2. **Redirection** -- when the stream is a file or pipe, the bytes are
///    decoded by whoever reads them. Windows `PowerShell` 5.1 decodes native
///    output with the console's legacy code page, so box-drawing characters
///    in `portlens > out.txt` turn into mojibake. Redirected output uses
///    ASCII borders unless the code page is UTF-8.
/// 3. **Windows Terminal** -- the `WT_SESSION` environment variable is
///    set by Windows Terminal, which always supports UTF-8.
/// 4. **Windows version** -- Windows 10 and newer (major >= 10) render
///    UTF-8 box-drawing correctly in virtually all terminal emulators.
///    Older releases (Windows 7/8) fall back to ASCII.
#[cfg(windows)]
fn terminal_supports_utf8_borders(stream: TerminalStream) -> bool {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetConsoleOutputCP() -> u32;
    }

    const UTF8_CODE_PAGE: u32 = 65001;

    // Safety: `GetConsoleOutputCP` is a simple syscall with no preconditions.
    // It returns 0 when the process has no console at all.
    let utf8_code_page = (unsafe { GetConsoleOutputCP() }) == UTF8_CODE_PAGE;

    windows_utf8_borders(
        utf8_code_page,
        stream.is_terminal(),
        std::env::var_os("WT_SESSION").is_some(),
        is_windows_10_or_newer,
    )
}

/// Decision logic behind the Windows UTF-8 border check, free of OS calls.
#[cfg(any(windows, test))]
fn windows_utf8_borders(
    utf8_code_page: bool,
    is_terminal: bool,
    in_windows_terminal: bool,
    is_windows_10_or_newer: impl FnOnce() -> bool,
) -> bool {
    if utf8_code_page {
        return true;
    }
    if !is_terminal {
        return false;
    }
    in_windows_terminal || is_windows_10_or_newer()
}

/// Query the Windows NT kernel for the OS major version.
///
/// Uses `RtlGetVersion` from `ntdll.dll` because the older
/// `GetVersionExW` is subject to manifest-based compatibility shims
/// that can report stale version numbers.
#[cfg(windows)]
fn is_windows_10_or_newer() -> bool {
    // The struct layout matches OSVERSIONINFOW from the Windows SDK.
    // The field name must match the Windows API naming convention.
    #[allow(clippy::struct_field_names)]
    #[repr(C)]
    struct OsVersionInfo {
        os_version_info_size: u32,
        major_version: u32,
        _minor_version: u32,
        _build_number: u32,
        _platform_id: u32,
        _sz_csd_version: [u16; 128],
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfo) -> i32;
    }

    let mut info = std::mem::MaybeUninit::<OsVersionInfo>::zeroed();
    // Safety: `RtlGetVersion` writes into our stack-allocated struct and
    // always succeeds (returns STATUS_SUCCESS == 0).
    unsafe {
        // The struct size is well under u32::MAX; truncation cannot happen.
        #[allow(clippy::cast_possible_truncation)]
        let size = std::mem::size_of::<OsVersionInfo>() as u32;
        (*info.as_mut_ptr()).os_version_info_size = size;
        if RtlGetVersion(info.as_mut_ptr()) == 0 {
            return (*info.as_ptr()).major_version >= 10;
        }
    }
    // If RtlGetVersion fails (should never happen), fall back to ASCII.
    false
}

/// Check whether `stream` can display UTF-8 box-drawing characters.
///
/// On non-Windows platforms, returns `true` unconditionally because
/// virtually all modern Unix terminals and pipelines use UTF-8.
#[cfg(not(windows))]
const fn terminal_supports_utf8_borders(_stream: TerminalStream) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirected_output_is_never_truncated_even_with_columns_set() {
        let width = resolve_width(false, || Some(40), || Some(120));
        assert_eq!(width, None, "piped output must keep full rows");
    }

    #[test]
    fn terminal_output_prefers_columns_over_detected_width() {
        assert_eq!(resolve_width(true, || Some(40), || Some(120)), Some(40));
        assert_eq!(resolve_width(true, || None, || Some(120)), Some(120));
        assert_eq!(resolve_width(true, || None, || None), None);
    }

    #[test]
    fn redirected_windows_output_uses_ascii_unless_code_page_is_utf8() {
        assert!(
            !windows_utf8_borders(false, false, true, || true),
            "a file or pipe on a legacy code page gets ASCII borders"
        );
        assert!(
            windows_utf8_borders(true, false, false, || false),
            "a UTF-8 console code page keeps box drawing when redirected"
        );
    }

    #[test]
    fn windows_console_output_keeps_existing_heuristics() {
        assert!(windows_utf8_borders(false, true, true, || false));
        assert!(windows_utf8_borders(false, true, false, || true));
        assert!(
            !windows_utf8_borders(false, true, false, || false),
            "pre-Windows 10 consoles fall back to ASCII"
        );
    }
}
