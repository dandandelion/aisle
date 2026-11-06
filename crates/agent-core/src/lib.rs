//! agent-core: portable sideagent logic. No UI, no Linux-only deps outside
//! the `capture` module (cfg(unix)). See docs/spec.md.

pub mod agent;
pub mod config;
pub mod context;
pub mod error;
pub mod image;
pub mod provider;
pub mod store;

#[cfg(unix)]
pub mod capture;
