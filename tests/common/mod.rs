#![allow(dead_code)]

//! Shared helpers for the integration tests.

use opencode_gear::config::{build_effective, Effective};
use opencode_gear::defaults::{load_defaults, GearSource};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A temporary directory removed on drop.
pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("ocg-test-{}-{}", std::process::id(), id));
        fs::create_dir_all(&path).expect("create test dir");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    pub fn project(&self) -> PathBuf {
        let project = self.join("project");
        fs::create_dir_all(&project).expect("create project");
        project
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn repo_config_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("config")
}

pub fn copy_dir_recursive(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create dir");
    for entry in fs::read_dir(from).expect("read dir") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir_recursive(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// A gear home with `config/` copied from the repository.
pub fn gear_home(dir: &TestDir) -> PathBuf {
    let home = dir.join("gear");
    copy_dir_recursive(&repo_config_dir(), &home.join("config"));
    home
}

pub fn load_disk_effective(
    home: &Path,
    project: &Path,
    user: Option<&Path>,
    project_config: Option<&Path>,
) -> Effective {
    let defaults = load_defaults(&GearSource::Dir(home.to_path_buf())).expect("defaults");
    let user_path = user
        .map(Path::to_path_buf)
        .unwrap_or_else(|| project.join("no-user.json"));
    let project_path = project_config
        .map(Path::to_path_buf)
        .unwrap_or_else(|| project.join(".opencode-gear.json"));
    build_effective(
        defaults,
        Some(home.to_path_buf()),
        project,
        &user_path,
        &project_path,
        None,
    )
    .expect("effective")
}

pub fn load_embedded_effective(project: &Path) -> Effective {
    let defaults = load_defaults(&GearSource::Embedded).expect("defaults");
    let user_path = project.join("no-user.json");
    let project_path = project.join(".opencode-gear.json");
    build_effective(defaults, None, project, &user_path, &project_path, None).expect("effective")
}

pub fn write_json(path: &Path, value: &Value) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(
        path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(value).expect("serialize")
        ),
    )
    .expect("write json");
}

pub fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).expect("read json")).expect("parse json")
}

pub fn patch_json<F: FnOnce(&mut Value)>(path: &Path, mutate: F) {
    let mut value = read_json(path);
    mutate(&mut value);
    write_json(path, &value);
}

pub fn get<'a>(value: &'a Value, key: &str) -> &'a Value {
    value
        .get(key)
        .unwrap_or_else(|| panic!("missing key '{key}'"))
}
