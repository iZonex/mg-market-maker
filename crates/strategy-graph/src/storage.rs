//! Disk I/O for graphs.
//!
//! Graphs are authored in the UI, saved as JSON, deployed to the
//! engine. This module is the read/write boundary.
//!
//! Layout on disk:
//!   `{root}/{name}.json`   — canonical graph JSON
//!   `{root}/.deploys.jsonl` — append-only deploy log (name, hash,
//!                             operator, deployed_at) — compliance
//!                             feeds off this.
//!
//! Writes are atomic: tmp + rename so a partial write never leaves a
//! half-baked file at the canonical path (an engine tick evaluating
//! concurrently would crash-load otherwise).

use crate::graph::{Graph, CURRENT_SCHEMA_VERSION};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

impl Graph {
    /// Parse a graph from its JSON form. Version is checked up front;
    /// an unknown version fails with a clear error so a stale bundle
    /// from a future release doesn't silently load with half the
    /// fields defaulted.
    pub fn from_json(raw: &str) -> Result<Self> {
        let graph: Graph = serde_json::from_str(raw).context("parse graph json")?;
        if graph.version != CURRENT_SCHEMA_VERSION {
            return Err(anyhow!(
                "graph schema version {} is unsupported (current: {})",
                graph.version,
                CURRENT_SCHEMA_VERSION
            ));
        }
        Ok(graph)
    }

    /// Pretty-printed JSON. `content_hash` intentionally uses the
    /// compact form (one-line serialisation) so whitespace in the
    /// pretty-printed on-disk file doesn't drift from the audit hash.
    pub fn to_json_pretty(&self) -> Result<String> {
        serde_json::to_string_pretty(self).context("serialise graph json")
    }
}

/// One record appended to `.deploys.jsonl` every time a graph is
/// saved. Regulator / audit sweeps join this against
/// `mm_risk::audit::AuditLog` entries carrying the same hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployRecord {
    pub name: String,
    pub hash: String,
    pub operator: String,
    pub deployed_at: chrono::DateTime<chrono::Utc>,
    pub scope: String,
}

/// Filesystem-backed store. One directory per deployment (`data/
/// strategy_graphs/` by default, configurable).
#[derive(Debug, Clone)]
pub struct GraphStore {
    root: PathBuf,
}

impl GraphStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .with_context(|| format!("create graph store dir {}", root.display()))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, name: &str) -> Result<PathBuf> {
        validate_name(name)?;
        Ok(self.root.join(format!("{name}.json")))
    }

    /// Save `graph` under its declared name. Atomic: writes to a tmp
    /// file in the same directory, then renames.
    ///
    /// Every save also writes a historical snapshot at
    /// `history/{name}/{hash}.json`. The current pointer at
    /// `{name}.json` is always the latest; the history snapshot
    /// is the audit-friendly immutable record. Rollback reads the
    /// historical file keyed by hash.
    ///
    /// If `operator` is provided a `DeployRecord` is appended to
    /// `.deploys.jsonl`.
    pub fn save(&self, graph: &Graph, operator: Option<&str>) -> Result<String> {
        let path = self.path_for(&graph.name)?;
        let body = graph.to_json_pretty()?;
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)
                .with_context(|| format!("create tmp {}", tmp.display()))?;
            f.write_all(body.as_bytes())?;
            f.sync_data()?;
        }
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("atomic rename into {}", path.display()))?;

        let hash = graph.content_hash();

        // Historical snapshot — immutable, keyed by hash. Lets
        // operators roll back to an earlier deploy without
        // depending on external backups.
        let history_dir = self.root.join("history").join(&graph.name);
        std::fs::create_dir_all(&history_dir)
            .with_context(|| format!("create history dir {}", history_dir.display()))?;
        let history_path = history_dir.join(format!("{hash}.json"));
        if !history_path.exists() {
            let tmp = history_path.with_extension("json.tmp");
            {
                let mut f = std::fs::File::create(&tmp)?;
                f.write_all(body.as_bytes())?;
                f.sync_data()?;
            }
            std::fs::rename(&tmp, &history_path)?;
        }

        if let Some(op) = operator {
            self.append_deploy_log(&DeployRecord {
                name: graph.name.clone(),
                hash: hash.clone(),
                operator: op.to_string(),
                deployed_at: chrono::Utc::now(),
                scope: format!("{:?}", graph.scope),
            })?;
        }
        Ok(hash)
    }

    /// Load a specific historical version by name + hash.
    /// Used by the rollback UI: click a prior deploy in the
    /// history table, the frontend fetches this, re-deploys via
    /// the admin endpoint. Returns `None` if the hash isn't in
    /// history (e.g. history predates the versioning feature).
    pub fn load_by_hash(&self, name: &str, hash: &str) -> Result<Graph> {
        validate_name(name)?;
        // Hash is hex; reject anything else so a malicious hash
        // can't escape the history dir.
        if !hash.chars().all(|c| c.is_ascii_hexdigit()) || hash.len() > 128 {
            anyhow::bail!("invalid hash");
        }
        let path = self
            .root
            .join("history")
            .join(name)
            .join(format!("{hash}.json"));
        let body =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        Graph::from_json(&body)
    }

    /// Load a graph by name.
    pub fn load(&self, name: &str) -> Result<Graph> {
        let path = self.path_for(name)?;
        let body =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        Graph::from_json(&body)
    }

    /// Names of every persisted graph (by stripped `.json`).
    pub fn list(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(&self.root) else {
            return Ok(out);
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if let Some(stem) = path
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(|s| s.strip_suffix(".json"))
            {
                if stem.starts_with('.') {
                    continue;
                }
                out.push(stem.to_string());
            }
        }
        out.sort();
        Ok(out)
    }

    /// Deploy history in file order (oldest first).
    pub fn deploys(&self) -> Result<Vec<DeployRecord>> {
        let path = self.root.join(".deploys.jsonl");
        let Ok(body) = std::fs::read_to_string(&path) else {
            return Ok(Vec::new());
        };
        Ok(body
            .lines()
            .filter_map(|l| serde_json::from_str::<DeployRecord>(l).ok())
            .collect())
    }

    fn append_deploy_log(&self, rec: &DeployRecord) -> Result<()> {
        let path = self.root.join(".deploys.jsonl");
        let line = serde_json::to_string(rec)? + "\n";
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        f.write_all(line.as_bytes())?;
        f.sync_data()?;
        Ok(())
    }
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(anyhow!("graph name is empty"));
    }
    if name.len() > 128 {
        return Err(anyhow!("graph name too long (> 128 chars)"));
    }
    // Allow only safe filename chars — no path traversal, no hidden
    // files, no control chars.
    for c in name.chars() {
        let ok = c.is_ascii_alphanumeric() || c == '-' || c == '_';
        if !ok {
            return Err(anyhow!(
                "graph name contains disallowed character {c:?} (allowed: [a-zA-Z0-9_-])"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Node, Scope};
    use crate::types::NodeId;

    fn sample_graph() -> Graph {
        let mut g = Graph::empty("sample", Scope::Symbol("BTCUSDT".into()));
        g.nodes.push(Node {
            id: NodeId::new(),
            kind: "Out.SpreadMult".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g
    }

    #[test]
    fn roundtrip_preserves_structure() {
        let orig = sample_graph();
        let json = orig.to_json_pretty().unwrap();
        let back = Graph::from_json(&json).unwrap();
        assert_eq!(orig.name, back.name);
        assert_eq!(orig.scope, back.scope);
        assert_eq!(orig.nodes.len(), back.nodes.len());
        assert_eq!(orig.nodes[0].kind, back.nodes[0].kind);
    }

    #[test]
    fn hash_survives_roundtrip() {
        let orig = sample_graph();
        let json = orig.to_json_pretty().unwrap();
        let back = Graph::from_json(&json).unwrap();
        assert_eq!(orig.content_hash(), back.content_hash());
    }

    #[test]
    fn rejects_unknown_version() {
        let orig = sample_graph();
        let mut v = serde_json::to_value(&orig).unwrap();
        v["version"] = serde_json::json!(999);
        let json = serde_json::to_string(&v).unwrap();
        let err = Graph::from_json(&json).unwrap_err();
        assert!(err.to_string().contains("unsupported"));
    }

    #[test]
    fn rejects_malformed_json() {
        let err = Graph::from_json("{ not json").unwrap_err();
        assert!(err.to_string().contains("parse graph json"));
    }

    #[test]
    fn store_save_load_roundtrip() {
        let dir =
            std::env::temp_dir().join(format!("mm_strategy_graph_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = GraphStore::new(&dir).unwrap();
        let g = sample_graph();
        let hash = store.save(&g, Some("alice")).unwrap();
        assert_eq!(hash, g.content_hash());

        let loaded = store.load("sample").unwrap();
        assert_eq!(loaded.name, g.name);
        let names = store.list().unwrap();
        assert_eq!(names, vec!["sample"]);

        let deploys = store.deploys().unwrap();
        assert_eq!(deploys.len(), 1);
        assert_eq!(deploys[0].operator, "alice");
        assert_eq!(deploys[0].hash, hash);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn historical_save_enables_load_by_hash() {
        let dir =
            std::env::temp_dir().join(format!("mm_strategy_graph_hist_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = GraphStore::new(&dir).unwrap();
        let g = sample_graph();
        let hash = store.save(&g, Some("alice")).unwrap();
        let back = store
            .load_by_hash("sample", &hash)
            .expect("historical load");
        assert_eq!(back.content_hash(), hash);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_by_hash_rejects_path_traversal() {
        let dir = std::env::temp_dir().join(format!("mm_sg_hash_trav_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = GraphStore::new(&dir).unwrap();
        assert!(store.load_by_hash("sample", "../../passwd").is_err());
        assert!(store.load_by_hash("sample", "not!hex").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_path_traversal_in_name() {
        let dir = std::env::temp_dir().join(format!("mm_strategy_trav_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = GraphStore::new(&dir).unwrap();
        let mut g = sample_graph();
        g.name = "../../etc/passwd".into();
        let err = store.save(&g, None).unwrap_err();
        assert!(err.to_string().contains("disallowed character"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_empty_and_long_names() {
        assert!(validate_name("").is_err());
        assert!(validate_name(&"x".repeat(129)).is_err());
        assert!(validate_name("ok_one").is_ok());
    }
}
