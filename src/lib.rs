//! # `PortLens`
//!
//! `PortLens` is a cross-platform CLI tool that lists open network ports and their associated
//! processes. Provides a fast, readable alternative to `netstat` and `ss`.
//!
//! This crate is CLI-first. The public modules below exist to support the
//! benchmark harness and internal reuse, and should be treated as unstable
//! implementation details rather than a supported library API.
//!
//! ## Module structure
//!
//! - [`types`] - `PortEntry` struct and shared enums
//! - [`collector`] - socket enumeration + process/project/app enrichment
//! - [`filter`] - CLI filters and developer-relevance filter
//! - [`display`] - renders results as bordered/compact table or JSON
//!   - `table` — column definitions and table rendering engine
//!   - `tips` — "Quick Actions" footer panel with adaptive layout
//!   - `terminal` — terminal width detection and UTF-8 support probing
//! - [`docker`] - Docker/Podman container detection via socket API
//! - [`project`] - project root detection via marker file walk
//! - [`framework`] - app/framework detection from images, configs, process names

#[doc(hidden)]
pub mod collector;
#[doc(hidden)]
pub mod display;
#[doc(hidden)]
pub mod docker;
#[doc(hidden)]
pub mod filter;
#[doc(hidden)]
pub mod framework;
#[doc(hidden)]
pub mod kill;
#[doc(hidden)]
pub mod project;
#[doc(hidden)]
pub mod types;
#[doc(hidden)]
pub mod update;
