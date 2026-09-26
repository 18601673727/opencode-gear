//! Opt-in WorkNode/Run control substrate. Legacy Mission/replay JSON remains the
//! authority for legacy controller operations until an explicit migration.
//! This repository alone owns the SQLite tables below; no legacy state is
//! mirrored into them. Caches, logs and artifacts are not control truth.

use crate::error::{GearError, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use typed_index_collections::TiVec;

macro_rules! local_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub usize);
        impl From<usize> for $name {
            fn from(value: usize) -> Self {
                Self(value)
            }
        }
        impl From<$name> for usize {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}
local_id!(WorkNodeId);
local_id!(RunId);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionId(String);

impl MissionId {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if !crate::orchestration::checkpoint::is_safe_id(&value) {
            return Err(GearError::config("invalid MissionId"));
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkState {
    Ready,
    Running,
    Completed,
    Failed,
    Cancelled,
}
impl WorkState {
    fn text(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
    fn parse(s: &str) -> Result<Self> {
        match s {
            "ready" => Ok(Self::Ready),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(invalid("invalid WorkNode state")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Active,
    Completed,
    Failed,
    Cancelled,
    Superseded,
    Fenced,
}
impl RunState {
    fn text(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Superseded => "superseded",
            Self::Fenced => "fenced",
        }
    }
    fn parse(s: &str) -> Result<Self> {
        match s {
            "active" => Ok(Self::Active),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "superseded" => Ok(Self::Superseded),
            "fenced" => Ok(Self::Fenced),
            _ => Err(invalid("invalid Run state")),
        }
    }
}

/// Frozen executor selection. No lifecycle API exposes a mutable contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunContract {
    pub executor: String,
    pub model: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkNode {
    pub parent_node_id: Option<WorkNodeId>,
    pub spawned_by_run_id: Option<RunId>,
    pub state: WorkState,
    pub active_run_id: Option<RunId>,
    pub generation: u64,
    pub payload: String,
    pub priority: i64,
    pub not_before: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub node_id: WorkNodeId,
    pub generation: u64,
    contract: RunContract,
    /// External runtime/session identity; never the canonical RunId.
    pub runtime_execution_id: Option<String>,
    pub state: RunState,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub result: Option<String>,
}
impl Run {
    pub fn contract(&self) -> &RunContract {
        &self.contract
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    pub node_id: WorkNodeId,
    pub depends_on_node_id: WorkNodeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub seq: u64,
    pub kind: String,
    pub payload: String,
    pub by_run_id: Option<RunId>,
}

#[derive(Debug)]
pub struct MissionState {
    pub id: MissionId,
    pub work_nodes: TiVec<WorkNodeId, WorkNode>,
    pub runs: TiVec<RunId, Run>,
    pub dependencies: Vec<Dependency>,
    pub events: Vec<Event>,
}

impl MissionState {
    pub fn root(&self) -> WorkNodeId {
        WorkNodeId(0)
    }
    pub fn children(&self, parent: WorkNodeId) -> Vec<WorkNodeId> {
        self.work_nodes
            .iter_enumerated()
            .filter_map(|(id, node)| (node.parent_node_id == Some(parent)).then_some(id))
            .collect()
    }
    /// The durable ready set, not a separate queue. A dependency is satisfied
    /// only by a completed node, regardless of ownership topology.
    pub fn ready(&self, now: i64) -> Vec<WorkNodeId> {
        self.work_nodes
            .iter_enumerated()
            .filter_map(|(id, node)| {
                (node.state == WorkState::Ready
                    && node.active_run_id.is_none()
                    && node.not_before <= now
                    && self
                        .dependencies
                        .iter()
                        .filter(|d| d.node_id == id)
                        .all(|d| {
                            self.work_nodes[d.depends_on_node_id].state == WorkState::Completed
                        }))
                .then_some(id)
            })
            .collect()
    }
    pub fn authoritative(&self, node: WorkNodeId, run: RunId) -> bool {
        self.work_nodes.get(node).is_some_and(|n| {
            n.active_run_id == Some(run)
                && self.runs.get(run).is_some_and(|r| {
                    r.node_id == node && r.generation == n.generation && r.state == RunState::Active
                })
        })
    }
}

fn invalid(message: &str) -> GearError {
    GearError::config(message)
}
fn sql(error: rusqlite::Error) -> GearError {
    GearError::config(format!("substrate SQLite: {error}"))
}
fn id(value: i64) -> Result<usize> {
    usize::try_from(value).map_err(|_| invalid("negative or oversized local ID"))
}
fn num(value: usize) -> Result<i64> {
    i64::try_from(value).map_err(|_| invalid("local ID exceeds SQLite range"))
}
fn optional_id(value: Option<i64>) -> Result<Option<usize>> {
    value.map(id).transpose()
}
fn json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| invalid(&format!("substrate JSON: {e}")))
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS missions (
 mission_id TEXT PRIMARY KEY, created_at INTEGER NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS work_nodes (
 mission_id TEXT NOT NULL REFERENCES missions(mission_id), node_id INTEGER NOT NULL CHECK(node_id >= 0),
 parent_node_id INTEGER, spawned_by_run_id INTEGER, state TEXT NOT NULL,
 active_run_id INTEGER, generation INTEGER NOT NULL CHECK(generation >= 0), payload TEXT NOT NULL,
 priority INTEGER NOT NULL, not_before INTEGER NOT NULL,
 PRIMARY KEY(mission_id,node_id),
 FOREIGN KEY(mission_id,parent_node_id) REFERENCES work_nodes(mission_id,node_id),
 FOREIGN KEY(mission_id,spawned_by_run_id) REFERENCES runs(mission_id,run_id),
 FOREIGN KEY(mission_id,active_run_id) REFERENCES runs(mission_id,run_id) DEFERRABLE INITIALLY DEFERRED
) STRICT;
CREATE TABLE IF NOT EXISTS runs (
 mission_id TEXT NOT NULL, run_id INTEGER NOT NULL CHECK(run_id >= 0), node_id INTEGER NOT NULL,
 generation INTEGER NOT NULL CHECK(generation > 0), contract TEXT NOT NULL, runtime_execution_id TEXT,
 state TEXT NOT NULL, created_at INTEGER NOT NULL, finished_at INTEGER, result TEXT,
 PRIMARY KEY(mission_id,run_id),
 FOREIGN KEY(mission_id,node_id) REFERENCES work_nodes(mission_id,node_id)
) STRICT;
CREATE TABLE IF NOT EXISTS dependencies (
 mission_id TEXT NOT NULL, node_id INTEGER NOT NULL, depends_on_node_id INTEGER NOT NULL,
 CHECK(node_id != depends_on_node_id), PRIMARY KEY(mission_id,node_id,depends_on_node_id),
 FOREIGN KEY(mission_id,node_id) REFERENCES work_nodes(mission_id,node_id),
 FOREIGN KEY(mission_id,depends_on_node_id) REFERENCES work_nodes(mission_id,node_id)
) STRICT;
CREATE TABLE IF NOT EXISTS domain_events (
 mission_id TEXT NOT NULL REFERENCES missions(mission_id), seq INTEGER NOT NULL CHECK(seq > 0),
 kind TEXT NOT NULL, payload TEXT NOT NULL, by_run_id INTEGER,
 PRIMARY KEY(mission_id,seq),
 FOREIGN KEY(mission_id,by_run_id) REFERENCES runs(mission_id,run_id)
) STRICT;
"#;

/// One connection owned at the repository boundary. Reopen in another process
/// to reconstruct; SQLite WAL serializes writers and readers independently.
pub struct SubstrateRepository {
    conn: Connection,
    path: PathBuf,
}
impl SubstrateRepository {
    pub fn open(root: &Path) -> Result<Self> {
        crate::runtime::install::ensure_gitignore(root)?;
        let path = crate::orchestration::state::state_dir(root).join("substrate.sqlite3");
        let marker = path.with_extension("initialized");
        if marker.exists() && !path.is_file() {
            return Err(invalid(
                "canonical substrate database is missing after initialization",
            ));
        }
        std::fs::create_dir_all(path.parent().unwrap())
            .map_err(|e| GearError::io("create substrate directory", e))?;
        let conn = Connection::open(&path).map_err(sql)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(sql)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(sql)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(sql)?;
        conn.execute_batch(SCHEMA).map_err(sql)?;
        if !marker.exists() {
            std::fs::write(&marker, b"sqlite-worknode-v1\n")
                .map_err(|e| GearError::write(&marker, e))?;
        }
        Ok(Self { conn, path })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn journal_mode(&self) -> Result<String> {
        self.conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .map_err(sql)
    }

    pub fn create_mission(
        &mut self,
        mission: &MissionId,
        payload: &str,
        now: i64,
    ) -> Result<MissionState> {
        let tx = self.conn.transaction().map_err(sql)?;
        tx.execute(
            "INSERT INTO missions VALUES (?1,?2)",
            params![mission.as_str(), now],
        )
        .map_err(sql)?;
        tx.execute(
            "INSERT INTO work_nodes VALUES (?1,0,NULL,NULL,'ready',NULL,0,?2,0,0)",
            params![mission.as_str(), payload],
        )
        .map_err(sql)?;
        event(&tx, mission, "mission_created", "{}", None)?;
        tx.commit().map_err(sql)?;
        self.load(mission)?
            .ok_or_else(|| invalid("created mission missing"))
    }

    /// Establish a live Mission and its authoritative root Run in one commit.
    pub fn create_live_mission(
        &mut self,
        mission: &MissionId,
        payload: &str,
        contract: RunContract,
        runtime_execution_id: &str,
        now: i64,
    ) -> Result<RunId> {
        if runtime_execution_id.is_empty() {
            return Err(invalid("missing runtime binding"));
        }
        let tx = self.conn.transaction().map_err(sql)?;
        tx.execute(
            "INSERT INTO missions VALUES (?1,?2)",
            params![mission.as_str(), now],
        )
        .map_err(sql)?;
        tx.execute(
            "INSERT INTO work_nodes VALUES (?1,0,NULL,NULL,'running',0,1,?2,0,0)",
            params![mission.as_str(), payload],
        )
        .map_err(sql)?;
        insert_run(
            &tx,
            mission,
            0,
            WorkNodeId(0),
            1,
            &contract,
            Some(runtime_execution_id),
            now,
        )?;
        event(&tx, mission, "mission_created", "{}", None)?;
        event(
            &tx,
            mission,
            "run_started",
            &json(&serde_json::json!({"node_id":0,"run_id":0}))?,
            Some(RunId(0)),
        )?;
        tx.commit().map_err(sql)?;
        Ok(RunId(0))
    }

    pub fn load(&mut self, mission: &MissionId) -> Result<Option<MissionState>> {
        let tx = self.conn.transaction().map_err(sql)?;
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM missions WHERE mission_id=?1)",
                [mission.as_str()],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if !exists {
            return Ok(None);
        }
        let mut state = MissionState {
            id: mission.clone(),
            work_nodes: TiVec::new(),
            runs: TiVec::new(),
            dependencies: Vec::new(),
            events: Vec::new(),
        };
        let mut stmt = tx.prepare("SELECT node_id,parent_node_id,spawned_by_run_id,state,active_run_id,generation,payload,priority,not_before FROM work_nodes WHERE mission_id=?1 ORDER BY node_id").map_err(sql)?;
        let rows = stmt
            .query_map([mission.as_str()], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, i64>(8)?,
                ))
            })
            .map_err(sql)?;
        for row in rows {
            let (key, parent, spawned, status, active, generation, payload, priority, not_before) =
                row.map_err(sql)?;
            if id(key)? != state.work_nodes.len() {
                return Err(invalid("non-dense WorkNode IDs"));
            }
            state.work_nodes.push(WorkNode {
                parent_node_id: optional_id(parent)?.map(WorkNodeId),
                spawned_by_run_id: optional_id(spawned)?.map(RunId),
                state: WorkState::parse(&status)?,
                active_run_id: optional_id(active)?.map(RunId),
                generation: u64::try_from(generation).map_err(|_| invalid("invalid generation"))?,
                payload,
                priority,
                not_before,
            });
        }
        if state.work_nodes.is_empty() || state.work_nodes[WorkNodeId(0)].parent_node_id.is_some() {
            return Err(invalid("mission must have one root WorkNode"));
        }
        let mut stmt = tx.prepare("SELECT run_id,node_id,generation,contract,runtime_execution_id,state,created_at,finished_at,result FROM runs WHERE mission_id=?1 ORDER BY run_id").map_err(sql)?;
        let rows = stmt
            .query_map([mission.as_str()], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, Option<i64>>(7)?,
                    r.get::<_, Option<String>>(8)?,
                ))
            })
            .map_err(sql)?;
        for row in rows {
            let (
                key,
                node,
                generation,
                contract,
                runtime_execution_id,
                status,
                created_at,
                finished_at,
                result,
            ) = row.map_err(sql)?;
            if id(key)? != state.runs.len() {
                return Err(invalid("non-dense Run IDs"));
            }
            state.runs.push(Run {
                node_id: WorkNodeId(id(node)?),
                generation: u64::try_from(generation).map_err(|_| invalid("invalid generation"))?,
                contract: serde_json::from_str(&contract)
                    .map_err(|e| invalid(&format!("invalid RunContract: {e}")))?,
                runtime_execution_id,
                state: RunState::parse(&status)?,
                created_at,
                finished_at,
                result,
            });
        }
        for (key, node) in state.work_nodes.iter_enumerated() {
            if key == WorkNodeId(0) && node.spawned_by_run_id.is_some()
                || key != WorkNodeId(0) && node.parent_node_id.is_none()
            {
                return Err(invalid("invalid ownership root"));
            }
            if node
                .parent_node_id
                .is_some_and(|parent| state.work_nodes.get(parent).is_none())
            {
                return Err(invalid("invalid ownership parent"));
            }
            if node.parent_node_id.is_some_and(|p| p.0 >= key.0) {
                return Err(invalid("ownership must precede child"));
            }
            if let Some(spawned) = node.spawned_by_run_id {
                let parent = node
                    .parent_node_id
                    .ok_or_else(|| invalid("missing parent"))?;
                if !state
                    .runs
                    .get(spawned)
                    .is_some_and(|run| run.node_id == parent)
                {
                    return Err(invalid("invalid spawn provenance"));
                }
            }
            if let Some(run) = node.active_run_id {
                if !state.authoritative(key, run) {
                    return Err(invalid("invalid active Run witness"));
                }
            }
        }
        for (key, run) in state.runs.iter_enumerated() {
            let node = state
                .work_nodes
                .get(run.node_id)
                .ok_or_else(|| invalid("invalid Run owner"))?;
            if run.generation == 0
                || run.generation > node.generation
                || (run.state == RunState::Active && node.active_run_id != Some(key))
            {
                return Err(invalid("invalid Run generation or authority"));
            }
        }
        let mut stmt = tx.prepare("SELECT node_id,depends_on_node_id FROM dependencies WHERE mission_id=?1 ORDER BY node_id,depends_on_node_id").map_err(sql)?;
        let rows = stmt
            .query_map([mission.as_str()], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(sql)?;
        for row in rows {
            let (a, b) = row.map_err(sql)?;
            if state.work_nodes.get(WorkNodeId(id(a)?)).is_none()
                || state.work_nodes.get(WorkNodeId(id(b)?)).is_none()
            {
                return Err(invalid("invalid dependency endpoint"));
            }
            state.dependencies.push(Dependency {
                node_id: WorkNodeId(id(a)?),
                depends_on_node_id: WorkNodeId(id(b)?),
            });
        }
        let mut stmt = tx.prepare("SELECT seq,kind,payload,by_run_id FROM domain_events WHERE mission_id=?1 ORDER BY seq").map_err(sql)?;
        let rows = stmt
            .query_map([mission.as_str()], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            })
            .map_err(sql)?;
        for row in rows {
            let (seq, kind, payload, by_run) = row.map_err(sql)?;
            if id(seq)? != state.events.len() + 1 {
                return Err(invalid("non-contiguous event sequence"));
            }
            state.events.push(Event {
                seq: seq as u64,
                kind,
                payload,
                by_run_id: optional_id(by_run)?.map(RunId),
            });
        }
        Ok(Some(state))
    }

    /// Stale/fenced runs fail closed before any authoritative mutation.
    pub fn create_child(
        &mut self,
        mission: &MissionId,
        parent: WorkNodeId,
        by: RunId,
        payload: &str,
        now: i64,
    ) -> Result<WorkNodeId> {
        self.create_child_work(mission, parent, by, payload, &[], now)
    }

    /// One atomic CreateChildWork: ownership, provenance, and dependency DAG.
    pub fn create_child_work(
        &mut self,
        mission: &MissionId,
        parent: WorkNodeId,
        by: RunId,
        payload: &str,
        dependencies: &[WorkNodeId],
        now: i64,
    ) -> Result<WorkNodeId> {
        let tx = self.conn.transaction().map_err(sql)?;
        require_authority(&tx, mission, parent, by)?;
        let next = next_id(&tx, "work_nodes", "node_id", mission)?;
        let mut unique = std::collections::BTreeSet::new();
        for dep in dependencies {
            if dep.0 >= next || !unique.insert(dep.0) {
                return Err(invalid("invalid or repeated Dependency"));
            }
            node_witness(&tx, mission, *dep)?;
        }
        tx.execute(
            "INSERT INTO work_nodes VALUES (?1,?2,?3,?4,'ready',NULL,0,?5,0,?6)",
            params![
                mission.as_str(),
                num(next)?,
                num(parent.0)?,
                num(by.0)?,
                payload,
                now
            ],
        )
        .map_err(sql)?;
        for dep in unique {
            tx.execute(
                "INSERT INTO dependencies VALUES (?1,?2,?3)",
                params![mission.as_str(), num(next)?, num(dep)?],
            )
            .map_err(sql)?;
        }
        event(
            &tx,
            mission,
            "child_created",
            &json(
                &serde_json::json!({"node_id":next,"parent_node_id":parent.0,"dependencies":dependencies.iter().map(|id| id.0).collect::<Vec<_>>()}),
            )?,
            Some(by),
        )?;
        if dependencies_complete(&tx, mission, WorkNodeId(next))? {
            event(
                &tx,
                mission,
                "worknode_ready",
                &json(&serde_json::json!({"node_id":next}))?,
                Some(by),
            )?;
        }
        tx.commit().map_err(sql)?;
        Ok(WorkNodeId(next))
    }

    pub fn add_dependency(
        &mut self,
        mission: &MissionId,
        node: WorkNodeId,
        prerequisite: WorkNodeId,
    ) -> Result<()> {
        let tx = self.conn.transaction().map_err(sql)?;
        // Dependency edits belong to planning, before either node is dispatched.
        let state = load_states_for_dependency(&tx, mission, node, prerequisite)?;
        if node == prerequisite || state.iter().any(|s| *s != "ready") {
            return Err(invalid("dependency requires distinct ready nodes"));
        }
        let cycle: bool = tx.query_row("WITH RECURSIVE ancestors(id) AS (SELECT depends_on_node_id FROM dependencies WHERE mission_id=?1 AND node_id=?2 UNION SELECT d.depends_on_node_id FROM dependencies d JOIN ancestors a ON d.node_id=a.id WHERE d.mission_id=?1) SELECT EXISTS(SELECT 1 FROM ancestors WHERE id=?3)", params![mission.as_str(),num(prerequisite.0)?,num(node.0)?], |r| r.get(0)).map_err(sql)?;
        if cycle {
            return Err(invalid("dependency cycle"));
        }
        tx.execute(
            "INSERT INTO dependencies VALUES (?1,?2,?3)",
            params![mission.as_str(), num(node.0)?, num(prerequisite.0)?],
        )
        .map_err(sql)?;
        event(
            &tx,
            mission,
            "dependency_added",
            &json(&serde_json::json!({"node_id":node.0,"depends_on_node_id":prerequisite.0}))?,
            None,
        )?;
        tx.commit().map_err(sql)
    }

    pub fn start_run(
        &mut self,
        mission: &MissionId,
        node: WorkNodeId,
        contract: RunContract,
        now: i64,
    ) -> Result<RunId> {
        self.start_run_with_binding(mission, node, contract, None, now)
    }

    fn start_run_with_binding(
        &mut self,
        mission: &MissionId,
        node: WorkNodeId,
        contract: RunContract,
        binding: Option<&str>,
        now: i64,
    ) -> Result<RunId> {
        let tx = self.conn.transaction().map_err(sql)?;
        let run = start_run_transaction(&tx, mission, node, &contract, binding, now)?;
        tx.commit().map_err(sql)?;
        Ok(run)
    }

    /// Start a ready WorkNode with an explicit external runtime binding.
    pub fn dispatch_run(
        &mut self,
        mission: &MissionId,
        node: WorkNodeId,
        by: RunId,
        contract: RunContract,
        binding: &str,
        now: i64,
    ) -> Result<RunId> {
        if binding.is_empty() {
            return Err(invalid("missing runtime binding"));
        }
        let tx = self.conn.transaction().map_err(sql)?;
        let parent: Option<i64> = tx
            .query_row(
                "SELECT parent_node_id FROM work_nodes WHERE mission_id=?1 AND node_id=?2",
                params![mission.as_str(), num(node.0)?],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?
            .ok_or_else(|| invalid("unknown WorkNode"))?;
        let parent = parent.ok_or_else(|| invalid("root Run already established"))?;
        require_authority(&tx, mission, WorkNodeId(id(parent)?), by)?;
        let new_run = start_run_transaction(&tx, mission, node, &contract, Some(binding), now)?;
        tx.commit().map_err(sql)?;
        Ok(new_run)
    }

    /// Shared root/child replacement primitive; commit before updating any
    /// caller's TiVec projection. Reload after commit to reconcile memory.
    pub fn replace_run(
        &mut self,
        mission: &MissionId,
        node: WorkNodeId,
        old: RunId,
        contract: RunContract,
        now: i64,
    ) -> Result<RunId> {
        self.replace_run_with_binding(mission, node, old, contract, None, now)
    }

    fn replace_run_with_binding(
        &mut self,
        mission: &MissionId,
        node: WorkNodeId,
        old: RunId,
        contract: RunContract,
        binding: Option<&str>,
        now: i64,
    ) -> Result<RunId> {
        let tx = self.conn.transaction().map_err(sql)?;
        let (state, generation, active, _) = node_witness(&tx, mission, node)?;
        if active != Some(old.0) && !(state == "failed" && active.is_none()) {
            return Err(invalid("stale Run cannot replace current authority"));
        }
        if active.is_some() {
            require_authority(&tx, mission, node, old)?;
        } else {
            let failed: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE mission_id=?1 AND run_id=?2 AND node_id=?3 AND generation=?4 AND state='failed')",params![mission.as_str(),num(old.0)?,num(node.0)?,generation],|r|r.get(0)).map_err(sql)?;
            if !failed {
                return Err(invalid("stale Run cannot replace current authority"));
            }
        }
        let next = next_id(&tx, "runs", "run_id", mission)?;
        let new_generation = generation
            .checked_add(1)
            .ok_or_else(|| invalid("generation overflow"))?;
        tx.execute("UPDATE runs SET state='fenced',finished_at=?4 WHERE mission_id=?1 AND run_id=?2 AND state IN ('active','failed') AND generation=?3",params![mission.as_str(),num(old.0)?,generation,now]).map_err(sql)?;
        insert_run(
            &tx,
            mission,
            next,
            node,
            new_generation,
            &contract,
            binding,
            now,
        )?;
        tx.execute("UPDATE work_nodes SET state='running',generation=?3,active_run_id=?4 WHERE mission_id=?1 AND node_id=?2",params![mission.as_str(),num(node.0)?,new_generation,num(next)?]).map_err(sql)?;
        event(
            &tx,
            mission,
            "run_fenced",
            &json(&serde_json::json!({"node_id":node.0,"run_id":old.0}))?,
            Some(old),
        )?;
        event(
            &tx,
            mission,
            "run_replaced",
            &json(&serde_json::json!({"node_id":node.0,"old_run_id":old.0,"new_run_id":next}))?,
            Some(RunId(next)),
        )?;
        tx.commit().map_err(sql)?;
        Ok(RunId(next))
    }

    /// Shared root/worker replacement with a new external binding.
    pub fn replace_bound_run(
        &mut self,
        mission: &MissionId,
        node: WorkNodeId,
        old: RunId,
        contract: RunContract,
        binding: &str,
        now: i64,
    ) -> Result<RunId> {
        if binding.is_empty() {
            return Err(invalid("missing runtime binding"));
        }
        self.replace_run_with_binding(mission, node, old, contract, Some(binding), now)
    }

    pub fn finish_run(
        &mut self,
        mission: &MissionId,
        node: WorkNodeId,
        run: RunId,
        outcome: RunState,
        result: Option<&str>,
        now: i64,
    ) -> Result<()> {
        if matches!(outcome, RunState::Active | RunState::Fenced) {
            return Err(invalid("invalid terminal outcome"));
        }
        let tx = self.conn.transaction().map_err(sql)?;
        require_authority(&tx, mission, node, run)?;
        tx.execute(
            "UPDATE runs SET state=?3,finished_at=?4,result=?5 WHERE mission_id=?1 AND run_id=?2",
            params![mission.as_str(), num(run.0)?, outcome.text(), now, result],
        )
        .map_err(sql)?;
        let work = match outcome {
            RunState::Completed => WorkState::Completed,
            RunState::Cancelled => WorkState::Cancelled,
            _ => WorkState::Failed,
        };
        tx.execute(
            "UPDATE work_nodes SET state=?3,active_run_id=NULL WHERE mission_id=?1 AND node_id=?2",
            params![mission.as_str(), num(node.0)?, work.text()],
        )
        .map_err(sql)?;
        event(
            &tx,
            mission,
            match outcome {
                RunState::Completed => "run_completed",
                RunState::Failed => "run_failed",
                _ => "run_finished",
            },
            &json(&serde_json::json!({"node_id":node.0,"run_id":run.0,"state":outcome.text()}))?,
            Some(run),
        )?;
        if work == WorkState::Completed {
            let mut stmt = tx.prepare("SELECT node_id FROM dependencies WHERE mission_id=?1 AND depends_on_node_id=?2 ORDER BY node_id").map_err(sql)?;
            let nodes = stmt
                .query_map(params![mission.as_str(), num(node.0)?], |r| {
                    r.get::<_, i64>(0)
                })
                .map_err(sql)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(sql)?;
            drop(stmt);
            for dependent in nodes {
                let dependent = WorkNodeId(id(dependent)?);
                if node_witness(&tx, mission, dependent)?.0 == "ready"
                    && dependencies_complete(&tx, mission, dependent)?
                {
                    event(
                        &tx,
                        mission,
                        "worknode_ready",
                        &json(&serde_json::json!({"node_id":dependent.0}))?,
                        Some(run),
                    )?;
                }
            }
        }
        tx.commit().map_err(sql)
    }

    /// Late evidence is not authority: it does not change WorkNode state.
    pub fn record_fenced_result(
        &mut self,
        mission: &MissionId,
        run: RunId,
        result: &str,
    ) -> Result<()> {
        let tx = self.conn.transaction().map_err(sql)?;
        let updated = tx.execute(
            "UPDATE runs SET result=?3 WHERE mission_id=?1 AND run_id=?2 AND state='fenced' AND result IS NULL",
            params![mission.as_str(), num(run.0)?, result],
        ).map_err(sql)?;
        if updated != 1 {
            return Err(invalid("Run is not fenced or already has a result"));
        }
        event(&tx, mission, "fenced_result", "{}", Some(run))?;
        tx.commit().map_err(sql)
    }
}

fn next_id(tx: &Transaction<'_>, table: &str, column: &str, mission: &MissionId) -> Result<usize> {
    // Only internal static identifiers are passed here, never user input.
    let n: i64 = tx
        .query_row(
            &format!("SELECT COALESCE(MAX({column})+1,0) FROM {table} WHERE mission_id=?1"),
            [mission.as_str()],
            |r| r.get(0),
        )
        .map_err(sql)?;
    id(n)
}
fn event(
    tx: &Transaction<'_>,
    mission: &MissionId,
    kind: &str,
    payload: &str,
    by: Option<RunId>,
) -> Result<()> {
    let seq: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(seq)+1,1) FROM domain_events WHERE mission_id=?1",
            [mission.as_str()],
            |r| r.get(0),
        )
        .map_err(sql)?;
    tx.execute(
        "INSERT INTO domain_events VALUES (?1,?2,?3,?4,?5)",
        params![
            mission.as_str(),
            seq,
            kind,
            payload,
            by.map(|v| num(v.0)).transpose()?
        ],
    )
    .map_err(sql)?;
    Ok(())
}
fn node_witness(
    tx: &Transaction<'_>,
    mission: &MissionId,
    node: WorkNodeId,
) -> Result<(String, i64, Option<usize>, i64)> {
    let row = tx.query_row("SELECT state,generation,active_run_id,not_before FROM work_nodes WHERE mission_id=?1 AND node_id=?2",params![mission.as_str(),num(node.0)?],|r| Ok((r.get(0)?,r.get(1)?,r.get::<_,Option<i64>>(2)?,r.get(3)?))).optional().map_err(sql)?.ok_or_else(|| invalid("unknown WorkNode"))?;
    Ok((row.0, row.1, optional_id(row.2)?, row.3))
}
fn require_authority(
    tx: &Transaction<'_>,
    mission: &MissionId,
    node: WorkNodeId,
    run: RunId,
) -> Result<()> {
    let (state, generation, active, _) = node_witness(tx, mission, node)?;
    let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE mission_id=?1 AND run_id=?2 AND node_id=?3 AND generation=?4 AND state='active')",params![mission.as_str(),num(run.0)?,num(node.0)?,generation],|r| r.get(0)).map_err(sql)?;
    if state != "running" || active != Some(run.0) || !valid {
        return Err(invalid("Run is not authoritative"));
    }
    Ok(())
}
fn insert_run(
    tx: &Transaction<'_>,
    mission: &MissionId,
    run: usize,
    node: WorkNodeId,
    generation: i64,
    contract: &RunContract,
    runtime_execution_id: Option<&str>,
    now: i64,
) -> Result<()> {
    if contract.executor.is_empty() || contract.model.is_empty() || contract.role.is_empty() {
        return Err(invalid("incomplete RunContract"));
    }
    tx.execute(
        "INSERT INTO runs VALUES (?1,?2,?3,?4,?5,?6,'active',?7,NULL,NULL)",
        params![
            mission.as_str(),
            num(run)?,
            num(node.0)?,
            generation,
            json(contract)?,
            runtime_execution_id,
            now
        ],
    )
    .map_err(sql)?;
    Ok(())
}

fn start_run_transaction(
    tx: &Transaction<'_>,
    mission: &MissionId,
    node: WorkNodeId,
    contract: &RunContract,
    binding: Option<&str>,
    now: i64,
) -> Result<RunId> {
    let state = node_witness(tx, mission, node)?;
    if state.0 != "ready"
        || state.2.is_some()
        || now < state.3
        || !dependencies_complete(tx, mission, node)?
    {
        return Err(invalid("WorkNode not ready"));
    }
    let next = next_id(tx, "runs", "run_id", mission)?;
    let generation = state
        .1
        .checked_add(1)
        .ok_or_else(|| invalid("generation overflow"))?;
    insert_run(tx, mission, next, node, generation, contract, binding, now)?;
    tx.execute("UPDATE work_nodes SET state='running',generation=?3,active_run_id=?4 WHERE mission_id=?1 AND node_id=?2",params![mission.as_str(),num(node.0)?,generation,num(next)?]).map_err(sql)?;
    event(
        tx,
        mission,
        "run_started",
        &json(&serde_json::json!({"node_id":node.0,"run_id":next}))?,
        Some(RunId(next)),
    )?;
    Ok(RunId(next))
}
fn dependencies_complete(
    tx: &Transaction<'_>,
    mission: &MissionId,
    node: WorkNodeId,
) -> Result<bool> {
    let missing: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM dependencies d JOIN work_nodes n ON n.mission_id=d.mission_id AND n.node_id=d.depends_on_node_id WHERE d.mission_id=?1 AND d.node_id=?2 AND n.state!='completed')",params![mission.as_str(),num(node.0)?],|r| r.get(0)).map_err(sql)?;
    Ok(!missing)
}
fn load_states_for_dependency(
    tx: &Transaction<'_>,
    mission: &MissionId,
    node: WorkNodeId,
    prerequisite: WorkNodeId,
) -> Result<Vec<String>> {
    let mut states = Vec::new();
    for id in [node, prerequisite] {
        let (state, _, active, _) = node_witness(tx, mission, id)?;
        if active.is_some() {
            return Err(invalid("cannot edit dispatched dependencies"));
        }
        states.push(state);
    }
    Ok(states)
}
