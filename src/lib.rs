//! # portview
//!
//! A cross-platform CLI tool that lists open network ports and their associated
//! processes. Provides a fast, readable alternative to `netstat` and `ss`.
//!
//! ## Module structure
//!
//! - [`types`] - `PortEntry` struct shared across all modules
//! - [`collector`] - socket enumeration via `listeners` + process metadata via `sysinfo`
//! - [`filter`] - applies user-specified CLI filters before display
//! - [`display`] - renders results as an aligned table or JSON

pub mod collector;
pub mod display;
pub mod docker;
pub mod filter;
pub mod types;
