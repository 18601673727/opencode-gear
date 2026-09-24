#![forbid(unsafe_code)]
//! OpenCode Gear: project-agnostic multi-model orchestration for OpenCode.
//!
//! The library resolves the layered configuration (embedded defaults, user
//! override, project override, CLI/environment throttle), validates it,
//! assembles prompts and emits a deterministic OpenCode config. It also owns
//! the managed OpenCode runtime (discovery, install, update checks and Gear
//! self-update). The `ocg` binary is a thin CLI over these modules.

pub mod build;
pub mod capabilities;
pub mod cli;
pub mod clock;
pub mod config;
pub mod config_command;
pub mod context;
pub mod defaults;
pub mod error;
pub mod http;
pub mod json;
pub mod model;
pub mod observability;
pub mod orchestration;
pub mod platform;
pub mod preflight;
pub mod process;
pub mod project;
pub mod prompt;
pub mod proxy;
pub mod report;
pub mod reports;
pub mod resources;
pub mod runtime;
pub mod telemetry;
pub mod validate;
pub mod verification;
pub mod yaml;
