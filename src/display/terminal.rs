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

pub(super) fn stdout_terminal_width() -> Option<usize> {
    terminal_width(TerminalStream::Stdout)
}

pub(super) fn stderr_terminal_width() -> Option<usize> {
    terminal_width(TerminalStream::Stderr)
}

fn terminal_width(stream: TerminalStream) -> Option<usize> {
    env_terminal_width().or_else(|| platform_terminal_width(stream))
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

/// Check whether the terminal can display UTF-8 box-drawing characters.
///
/// On Windows the check uses several heuristics (cheapest first):
///
/// 1. **Windows Terminal** -- the `WT_SESSION` environment variable is
///    set by Windows Terminal, which always supports UTF-8.
/// 2. **Console code page** -- a code page of 65001 means the console is
///    in explicit UTF-8 mode.
/// 3. **Windows version** -- Windows 10 and newer (major >= 10) render
///    UTF-8 box-drawing correctly in virtually all terminal emulators.
///    Older releases (Windows 7/8) fall back to ASCII.
#[cfg(windows)]
pub(super) fn terminal_supports_utf8_borders() -> bool {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetConsoleOutputCP() -> u32;
    }

    const UTF8_CODE_PAGE: u32 = 65001;

    // Windows Terminal always supports UTF-8 box-drawing.
    if std::env::var_os("WT_SESSION").is_some() {
        return true;
    }

    // Safety: `GetConsoleOutputCP` is a simple syscall with no preconditions.
    if (unsafe { GetConsoleOutputCP() }) == UTF8_CODE_PAGE {
        return true;
    }

    // Windows 10+ (major version >= 10) renders UTF-8 correctly in most
    // terminal emulators. Only truly ancient releases need the ASCII fallback.
    is_windows_10_or_newer()
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

/// Check whether the terminal can display UTF-8 box-drawing characters.
///
/// On non-Windows platforms, returns `true` unconditionally because
/// virtually all modern Unix terminals support UTF-8.
#[cfg(not(windows))]
pub(super) const fn terminal_supports_utf8_borders() -> bool {
    true
}
