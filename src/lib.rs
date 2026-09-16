#![forbid(unsafe_code)]
//! OpenCode Gear: project-agnostic multi-model orchestration for OpenCode.
//!
//! The library resolves the layered configuration (embedded defaults, user
//! override, project override, CLI/environment throttle), validates it,
//! assembles prompts and emits a deterministic OpenCode config. The `ocg`
//! binary is a thin CLI over these modules.

pub mod build;
pub mod cli;
pub mod config;
pub mod defaults;
pub mod error;
pub mod json;
pub mod model;
pub mod observability;
pub mod process;
pub mod prompt;
pub mod report;
pub mod validate;
