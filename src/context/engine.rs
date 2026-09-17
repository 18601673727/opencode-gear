//! The context engine: one object that ties the repo map, index, diff, ranking
//! and cache together. It is intentionally synchronous, local and read-only
//! apart from `.opencode-gear/index/` and `.opencode-gear/cache/`.

use crate::clock::Clock;
use crate::context::cache::{self, CacheKey, CacheStats, CleanReport, ContextCache};
use crate::context::config::ContextConfig;
use crate::context::freshness::{Provenance, ENGINE_VERSION, SCHEMA_VERSION};
use crate::context::gitdiff::{self, DiffSummary, GitSnapshot};
use crate::context::index::{self, ContextIndex, IndexReport};
use crate::context::ranking::{self, ContextPlan};
use crate::context::repomap::{self, RepoMap};
use crate::context::symbols::SymbolRef;
use crate::error::Result;
use crate::process::GitHost;
use std::path::{Path, PathBuf};

/// One prepared context snapshot: the map, diff and index report.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub map: RepoMap,
    pub diff: DiffSummary,
    pub index_report: IndexReport,
}

/// The outcome of a plan request.
#[derive(Debug, Clone)]
pub struct PlanOutcome {
    pub plan: ContextPlan,
    pub from_cache: bool,
    /// Non-fatal warnings (for example a failed cache write).
    pub warnings: Vec<String>,
    pub index_report: IndexReport,
}

/// The engine for one project root.
pub struct ContextEngine<'a> {
    root: PathBuf,
    config: ContextConfig,
    git: &'a dyn GitHost,
    clock: &'a dyn Clock,
}

impl<'a> ContextEngine<'a> {
    pub fn new(
        root: impl Into<PathBuf>,
        config: ContextConfig,
        git: &'a dyn GitHost,
        clock: &'a dyn Clock,
    ) -> Self {
        Self {
            root: root.into(),
            config,
            git,
            clock,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config(&self) -> &ContextConfig {
        &self.config
    }

    /// Build the repo map together with the git snapshot it depends on.
    pub fn repo_map(&self) -> Result<(RepoMap, GitSnapshot)> {
        let snapshot = GitSnapshot::collect(&self.root, self.git);
        let map = repomap::build(&self.root, &self.config, &snapshot)?;
        Ok((map, snapshot))
    }

    /// Load the persisted index, if any.
    pub fn load_index(&self) -> Option<ContextIndex> {
        index::load(&self.root)
    }

    /// Incrementally update the index for a map/snapshot.
    pub fn update_index(
        &self,
        map: &RepoMap,
        snapshot: &GitSnapshot,
        previous: Option<&ContextIndex>,
    ) -> Result<(ContextIndex, IndexReport)> {
        index::update(
            &self.root,
            map,
            snapshot,
            &self.config,
            previous,
            self.clock.now_unix(),
        )
    }

    /// Compute the current diff summary against the index.
    pub fn diff(&self, snapshot: &GitSnapshot, index: &ContextIndex) -> Result<DiffSummary> {
        gitdiff::compute(&self.root, self.git, snapshot, index, &self.config)
    }

    /// Prepared map + diff + index without a task. Used on launch.
    pub fn prepare(&self) -> Result<Prepared> {
        let (map, snapshot) = self.repo_map()?;
        let previous = self.load_index();
        let (index, index_report) = self.update_index(&map, &snapshot, previous.as_ref())?;
        let diff = self.diff(&snapshot, &index)?;
        Ok(Prepared {
            map,
            diff,
            index_report,
        })
    }

    /// Build a deterministic plan for `task`, using the cache when fresh.
    pub fn plan(&self, task: &str, role: Option<&str>) -> Result<PlanOutcome> {
        let (map, snapshot) = self.repo_map()?;
        let previous = self.load_index();
        let (index, index_report) = self.update_index(&map, &snapshot, previous.as_ref())?;
        let now = self.clock.now_unix();
        let query = format!("{task}|{}", role.unwrap_or(""));
        let key = CacheKey::new(
            &index.repo_id,
            &self.config.fingerprint(),
            &ranking::fingerprint_text(&query),
            &gitdiff::snapshot_fingerprint(&snapshot),
        );

        // A cache hit skips ranking, slicing and file reads entirely. The entry
        // is only returned when its git identity, dependency fingerprints and
        // slice contents are still valid.
        if self.config.cache {
            let context_cache = ContextCache::new(&self.root);
            if let Some(cached) = context_cache.get(&self.root, &key) {
                return Ok(PlanOutcome {
                    plan: cached,
                    from_cache: true,
                    warnings: Vec::new(),
                    index_report,
                });
            }
        }

        let diff = self.diff(&snapshot, &index)?;
        let ranked = ranking::rank(&map, &index, &diff, task, &self.config);
        let mut plan = ranking::assemble(
            &self.root,
            task,
            role,
            &map,
            &index,
            &diff,
            ranked,
            &self.config,
            now,
        )?;
        let mut warnings = Vec::new();
        if self.config.cache {
            let dependencies = cache::dependencies(&plan);
            if let Err(error) = ContextCache::new(&self.root).put(&key, &dependencies, &plan, now) {
                // A cache write failure must never discard a computed plan.
                let warning = format!("context cache was not written: {error}");
                plan.notes.push(warning.clone());
                warnings.push(warning);
            }
        }

        Ok(PlanOutcome {
            plan,
            from_cache: false,
            warnings,
            index_report,
        })
    }

    /// Search symbols by case-insensitive substring.
    pub fn search_symbols(&self, query: &str, limit: usize) -> Result<Vec<SymbolRef>> {
        let index = self.current_index()?;
        Ok(index.search(query, limit))
    }

    /// The first non-import declaration named `name`.
    pub fn definition(&self, name: &str) -> Result<Option<SymbolRef>> {
        let index = self.current_index()?;
        Ok(index.definition(name))
    }

    /// A source slice for a symbol reference.
    pub fn source_slice(&self, reference: &SymbolRef) -> Result<Option<String>> {
        let index = self.current_index()?;
        index.source_slice(&self.root, reference)
    }

    /// Imports declared in a file.
    pub fn imports(&self, path: &str) -> Result<Vec<SymbolRef>> {
        let index = self.current_index()?;
        Ok(index.imports(path))
    }

    /// Probable references to a name across indexed files. This reads indexed
    /// (non-sensitive) file contents and matches on word boundaries; it is a
    /// deterministic heuristic, not a type checker.
    pub fn probable_references(&self, name: &str, limit: usize) -> Result<Vec<SymbolRef>> {
        let index = self.current_index()?;
        let mut out = Vec::new();
        for file in &index.files {
            if !file.supported || file.excluded || file.binary || file.huge {
                continue;
            }
            let path = self.root.join(&file.path);
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (offset, line) in content.lines().enumerate() {
                if !crate::context::symbols::contains_word(line, name) {
                    continue;
                }
                let line_number = (offset + 1) as u32;
                let enclosing = file.symbols.iter().find(|symbol| {
                    symbol.start_line <= line_number && symbol.end_line >= line_number
                });
                out.push(SymbolRef {
                    path: file.path.clone(),
                    name: name.to_string(),
                    kind: enclosing
                        .map(|symbol| symbol.kind)
                        .unwrap_or(crate::context::symbols::SymbolKind::Reference),
                    start_line: line_number,
                    end_line: line_number,
                });
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
        Ok(out)
    }

    /// Symbols related to `reference`: same-file declarations plus probable
    /// references to its name in other files.
    pub fn related(&self, reference: &SymbolRef) -> Result<Vec<SymbolRef>> {
        let index = self.current_index()?;
        let mut related: Vec<SymbolRef> = index
            .symbols_in(&reference.path)
            .into_iter()
            .filter(|symbol| {
                symbol.name != reference.name || symbol.start_line != reference.start_line
            })
            .collect();
        for candidate in self.probable_references(&reference.name, 50)? {
            if candidate.path != reference.path {
                related.push(candidate);
            }
        }
        related.sort_by(|a, b| {
            (&a.path, a.start_line, &a.name).cmp(&(&b.path, b.start_line, &b.name))
        });
        related.dedup();
        Ok(related)
    }

    /// Load the index, rebuilding it when missing or stale.
    fn current_index(&self) -> Result<ContextIndex> {
        let (map, snapshot) = self.repo_map()?;
        let previous = self.load_index();
        let (index, _) = self.update_index(&map, &snapshot, previous.as_ref())?;
        Ok(index)
    }

    pub fn cache_stats(&self) -> CacheStats {
        ContextCache::new(&self.root).stats()
    }

    pub fn cache_clean(&self) -> Result<CleanReport> {
        ContextCache::new(&self.root).clean()
    }

    /// Provenance for a fresh engine identity.
    pub fn provenance(&self) -> Provenance {
        Provenance {
            engine_version: ENGINE_VERSION.to_string(),
            schema_version: SCHEMA_VERSION,
            repo_id: index::repo_id(&self.root, &crate::context::gitdiff::GitState::default()),
            ..Provenance::default()
        }
    }
}
