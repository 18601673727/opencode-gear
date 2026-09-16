#![forbid(unsafe_code)]
//! OpenCode Gear: project-agnostic multi-model orchestration for OpenCode.
//!
//! The library resolves the layered configuration (embedded defaults, user
//! override, project override, CLI/environment throttle), validates it,
//! assembles prompts and emits a deterministic OpenCode config. It also owns
//! the managed OpenCode runtime (discovery, install, update checks and Gear
//! self-update). The `ocg` binary is a thin CLI over these modules.

pub mod build;
pub mod cli;
pub mod clock;
pub mod config;
pub mod defaults;
pub mod error;
pub mod http;
pub mod json;
pub mod model;
pub mod observability;
pub mod platform;
pub mod process;
pub mod prompt;
pub mod report;
pub mod runtime;
pub mod validate;
