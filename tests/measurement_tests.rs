//! Deterministic, offline measurement fixture for the local flows.
//!
//! This test builds a representative repository in a temporary directory and
//! measures the context engine (cold, warm, incremental) and log distillation.
//! Byte counts are deterministic and asserted; timings use `Instant` and are
//! only printed, never asserted. Run it with output to record a measurement:
//!
//! ```text
//! cargo test --test measurement_tests -- --nocapture
//! ```
//!
//! The fixture is intentionally small and uniform: it is one representative
//! sample on one machine, not a benchmark and not universal.

use opencode_gear::clock::FixedClock;
use opencode_gear::context::{ContextConfig, ContextEngine};
use opencode_gear::process::FakeGitHost;
use opencode_gear::verification::distill::distill;
use std::fs;
use std::path::Path;
use std::time::Instant;

const MODULES: usize = 120;

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn rust_module(index: usize) -> String {
    format!(
        "// module {index}\npub fn parse_{index}() -> u32 {{\n    {index}\n}}\n\npub fn helper_{index}(value: u32) -> u32 {{\n    value + {index}\n}}\n"
    )
}

fn fixture(root: &Path) {
    for index in 0..MODULES {
        write(
            &root.join(format!("src/module_{index}.rs")),
            &rust_module(index),
        );
    }
    write(&root.join("README.md"), "# Measurement fixture\n");
    write(
        &root.join("Cargo.toml"),
        "[package]\nname = \"measurement-fixture\"\nversion = \"0.0.0\"\n",
    );
}

fn noisy_log() -> String {
    let mut out = String::new();
    for index in 0..400 {
        out.push_str(&format!(
            "Compiling dependency-{} v0.1.{}\n",
            index % 20,
            index % 5
        ));
    }
    for _ in 0..60 {
        out.push_str("warning: unused variable: `value`\n");
    }
    out.push_str("error[E0308]: mismatched types at src/module_1.rs:4:5\n");
    out.push_str("error[E0308]: mismatched types at src/module_1.rs:4:5\n");
    out.push_str("test result: FAILED. 3 passed; 1 failed; 0 ignored\n");
    out
}

fn capsule_bytes(plan: &opencode_gear::context::ranking::ContextPlan) -> u64 {
    plan.capsule
        .as_ref()
        .and_then(|capsule| serde_json::to_vec(capsule).ok())
        .map(|bytes| bytes.len() as u64)
        .unwrap_or(0)
}

fn percent(reduced: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        ((reduced as f64) * 1000.0 / (total as f64)).round() / 10.0
    }
}

#[test]
fn measurement_context_and_log_distillation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root);

    let config = ContextConfig::default();
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let engine = ContextEngine::new(root, config, &git, &clock);
    let task = "update module_1 parser";

    // Cold: build the repo map, the symbol index and the first plan.
    let started = Instant::now();
    let cold = engine.plan(task, None).unwrap();
    let cold_ms = started.elapsed().as_secs_f64() * 1_000.0;
    assert!(
        !cold.from_cache,
        "the first plan must not come from the cache"
    );
    assert!(cold.plan.capsule.is_some(), "a task capsule must be built");
    assert!(cold.plan.candidate_bytes > 0);
    assert!(cold.plan.selected_bytes > 0);
    assert!(cold.plan.selected_bytes < cold.plan.candidate_bytes);

    // Warm: the identical plan is served from the dependency-checked cache.
    let started = Instant::now();
    let warm = engine.plan(task, None).unwrap();
    let warm_ms = started.elapsed().as_secs_f64() * 1_000.0;
    assert!(
        warm.from_cache,
        "an unchanged tree must hit the context cache"
    );
    assert_eq!(warm.plan.candidate_bytes, cold.plan.candidate_bytes);

    // Incremental: one selected file changes, so the index reuses the rest and
    // the cache is invalidated for exactly this plan.
    write(&root.join("src/module_1.rs"), &rust_module(9_999));
    let started = Instant::now();
    let incremental = engine.plan(task, None).unwrap();
    let incremental_ms = started.elapsed().as_secs_f64() * 1_000.0;
    assert!(
        !incremental.from_cache,
        "a changed dependency must miss the cache"
    );
    assert!(incremental.index_report.metrics.updated >= 1);
    assert!(incremental.index_report.metrics.reused >= 1);

    // Log distillation: a noisy raw log becomes a small deterministic summary.
    let noisy = noisy_log();
    let raw_bytes = noisy.len() as u64;
    let distilled = distill(&noisy, "", false);
    let distilled_bytes = serde_json::to_vec(&distilled).unwrap().len() as u64;
    assert!(
        distilled_bytes < raw_bytes,
        "distillation must reduce a noisy log"
    );

    let context_reduction = cold
        .plan
        .candidate_bytes
        .saturating_sub(cold.plan.selected_bytes) as u64;
    let context_reduction_percent = percent(context_reduction, cold.plan.candidate_bytes as u64);
    let log_reduction = raw_bytes.saturating_sub(distilled_bytes);
    let log_reduction_percent = percent(log_reduction, raw_bytes);

    // One machine-readable block; the documentation copies these exact values.
    println!("OCG_MEASUREMENT version=1 fixture_modules={MODULES}");
    println!(
        "context candidates={} selected={} capsule={} reduction={} reduction_percent={:.1}",
        cold.plan.candidate_bytes,
        cold.plan.selected_bytes,
        capsule_bytes(&cold.plan),
        context_reduction,
        context_reduction_percent
    );
    println!(
        "log raw={} distilled={} reduction={} reduction_percent={:.1}",
        raw_bytes, distilled_bytes, log_reduction, log_reduction_percent
    );
    println!("timing cold_ms={cold_ms:.3} warm_ms={warm_ms:.3} incremental_ms={incremental_ms:.3}");
    println!("OCG_MEASUREMENT_END");
}
