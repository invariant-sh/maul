//! Maul library surface — proxy, config, and (later) faults/budget/report.
//!
//! The binary (`main.rs`) is a thin boot wrapper around this crate so behavior
//! stays unit- and integration-testable without spinning the full server.

pub mod config;
pub mod proxy;
