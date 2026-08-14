#![forbid(unsafe_code)]
//! Headless, testable core for the File Tunnel Rust desktop app.

pub mod lifecycle;
pub mod observability;
pub mod transfer;

#[cfg(feature = "native-ui")]
pub mod desktop;
