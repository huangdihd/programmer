// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Local, inspectable long-term memory shared across sessions.
//!
//! Memories are split into a user-global file and a project file keyed by the
//! canonical repository root. Storage is deliberately plain versioned JSON:
//! the expected data set is small, atomic writes match session persistence, and
//! users can inspect or remove everything without a database-specific tool.

use crate::config::programmer_config::MemoryConfig;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: u32 = 1;

/// Structured L1 working state persisted alongside a compacted conversation.
/// The original summary remains the fallback when an older or model-produced
/// summary cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionMemory {
    pub(crate) schema_version: u32,
    pub(crate) intent: String,
    pub(crate) state: String,
    pub(crate) in_flight: String,
    pub(crate) next: String,
}

impl SessionMemory {
    pub(crate) fn from_summary(summary: &str) -> Option<Self> {
        let mut sections = [String::new(), String::new(), String::new(), String::new()];
        let mut current = None;
        for line in summary.lines() {
            let heading = line
                .trim()
                .trim_start_matches('#')
                .trim()
                .trim_start_matches(|character: char| {
                    character.is_ascii_digit() || character == '.'
                })
                .trim()
                .trim_matches('*')
                .trim_end_matches(':')
                .trim()
                .to_ascii_uppercase();
            let section = match heading.as_str() {
                "INTENT" => Some(0),
                "STATE" => Some(1),
                "IN FLIGHT" | "IN_FLIGHT" => Some(2),
                "NEXT" => Some(3),
                _ => None,
            };
            if let Some(index) = section {
                current = Some(index);
                continue;
            }
            if let Some(index) = current {
                if !sections[index].is_empty() {
                    sections[index].push('\n');
                }
                sections[index].push_str(line);
            }
        }
        if sections.iter().all(|section| section.trim().is_empty()) {
            return None;
        }
        Some(Self {
            schema_version: SCHEMA_VERSION,
            intent: sections[0].trim().to_string(),
            state: sections[1].trim().to_string(),
            in_flight: sections[2].trim().to_string(),
            next: sections[3].trim().to_string(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryScope {
    Global,
    Project,
}

impl MemoryScope {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "global" | "user" => Some(Self::Global),
            "project" | "workspace" => Some(Self::Project),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryKind {
    Preference,
    ProjectFact,
    Decision,
    Convention,
    Workflow,
    Constraint,
    KnownIssue,
}

impl MemoryKind {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "preference" => Some(Self::Preference),
            "project_fact" | "fact" => Some(Self::ProjectFact),
            "decision" => Some(Self::Decision),
            "convention" => Some(Self::Convention),
            "workflow" => Some(Self::Workflow),
            "constraint" => Some(Self::Constraint),
            "known_issue" | "issue" => Some(Self::KnownIssue),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Preference => "preference",
            Self::ProjectFact => "project_fact",
            Self::Decision => "decision",
            Self::Convention => "convention",
            Self::Workflow => "workflow",
            Self::Constraint => "constraint",
            Self::KnownIssue => "known_issue",
        }
    }

    /// Hours after which an unreinforced memory of this kind loses half its
    /// association weight. Durable facts decay slowly; volatile working notes
    /// decay fast, so a stale workflow never outranks a current preference.
    fn half_life_hours(self) -> f32 {
        match self {
            Self::Preference | Self::Constraint => 24.0 * 365.0,
            Self::Decision | Self::Convention => 24.0 * 180.0,
            Self::ProjectFact => 24.0 * 90.0,
            Self::KnownIssue => 24.0 * 45.0,
            Self::Workflow => 24.0 * 30.0,
        }
    }

    fn priority(self) -> f32 {
        match self {
            Self::Constraint | Self::Preference => 2.0,
            Self::Decision | Self::Convention => 1.5,
            Self::KnownIssue | Self::Workflow => 1.0,
            Self::ProjectFact => 0.5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemorySource {
    ExplicitUser,
    AgentTool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryConfidence {
    Explicit,
    Confirmed,
    Inferred,
}

impl MemoryConfidence {
    fn score(self) -> f32 {
        match self {
            Self::Explicit => 1.0,
            Self::Confirmed => 0.7,
            Self::Inferred => 0.2,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryStatus {
    #[default]
    Active,
    Superseded,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MemoryEntry {
    pub(crate) id: String,
    pub(crate) scope: MemoryScope,
    pub(crate) kind: MemoryKind,
    pub(crate) content: String,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
    #[serde(default)]
    pub(crate) last_used_at: Option<u64>,
    #[serde(default)]
    pub(crate) use_count: u32,
    pub(crate) source: MemorySource,
    pub(crate) confidence: MemoryConfidence,
    #[serde(default)]
    pub(crate) status: MemoryStatus,
    #[serde(default)]
    pub(crate) supersedes: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct MemoryFile {
    schema_version: u32,
    #[serde(default)]
    workspace_path: Option<String>,
    #[serde(default)]
    entries: Vec<MemoryEntry>,
}

impl MemoryFile {
    fn empty(workspace_path: Option<String>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            workspace_path,
            entries: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MemoryManager {
    global_path: PathBuf,
    project_path: PathBuf,
    workspace_root: PathBuf,
}

impl MemoryManager {
    pub(crate) fn for_current_dir() -> Result<Self, String> {
        let cwd = std::env::current_dir().map_err(|error| format!("current directory: {error}"))?;
        let config_dir = dirs::config_dir()
            .ok_or_else(|| "cannot locate the platform config directory".to_string())?;
        Ok(Self::new(config_dir.join("programmer"), cwd))
    }

    /// Build a store at explicit roots. Split from [`Self::for_current_dir`]
    /// so tests can point at a temporary directory.
    pub(crate) fn new(config_root: PathBuf, working_dir: PathBuf) -> Self {
        let workspace_root = workspace_root(&working_dir);
        let identity = workspace_root.to_string_lossy().replace('\\', "/");
        let workspace_id = blake3::hash(identity.as_bytes()).to_hex().to_string();
        let memory_dir = config_root.join("memory");
        Self {
            global_path: memory_dir.join("global.json"),
            project_path: memory_dir
                .join("projects")
                .join(format!("{workspace_id}.json")),
            workspace_root,
        }
    }

    pub(crate) fn list(&self, scope: Option<MemoryScope>) -> Result<Vec<MemoryEntry>, String> {
        let mut entries = Vec::new();
        if scope.is_none() || scope == Some(MemoryScope::Global) {
            entries.extend(self.load(MemoryScope::Global)?.entries);
        }
        if scope.is_none() || scope == Some(MemoryScope::Project) {
            entries.extend(self.load(MemoryScope::Project)?.entries);
        }
        entries.retain(|entry| entry.status == MemoryStatus::Active);
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.updated_at));
        Ok(entries)
    }

    pub(crate) fn remember(
        &self,
        scope: MemoryScope,
        kind: MemoryKind,
        content: String,
        tags: Vec<String>,
    ) -> Result<MemoryEntry, String> {
        validate_content(&content)?;
        let content = content.trim().to_string();
        let mut file = self.load(scope)?;
        if let Some(existing) = file.entries.iter().find(|entry| {
            entry.status == MemoryStatus::Active
                && entry.kind == kind
                && entry.content.eq_ignore_ascii_case(&content)
        }) {
            return Ok(existing.clone());
        }
        let now = now_secs();
        let entry = MemoryEntry {
            id: format!("mem_{}", uuid::Uuid::new_v4().simple()),
            scope,
            kind,
            content,
            tags: normalize_tags(tags),
            created_at: now,
            updated_at: now,
            last_used_at: None,
            use_count: 0,
            source: MemorySource::AgentTool,
            confidence: MemoryConfidence::Explicit,
            status: MemoryStatus::Active,
            supersedes: None,
        };
        file.entries.push(entry.clone());
        self.save(scope, &file)?;
        Ok(entry)
    }

    pub(crate) fn update(
        &self,
        id: &str,
        content: Option<String>,
        kind: Option<MemoryKind>,
        tags: Option<Vec<String>>,
    ) -> Result<MemoryEntry, String> {
        for scope in [MemoryScope::Global, MemoryScope::Project] {
            let mut file = self.load(scope)?;
            if let Some(entry) = file
                .entries
                .iter_mut()
                .find(|entry| entry.id == id && entry.status == MemoryStatus::Active)
            {
                if let Some(content) = content {
                    validate_content(&content)?;
                    entry.content = content.trim().to_string();
                }
                if let Some(kind) = kind {
                    entry.kind = kind;
                }
                if let Some(tags) = tags {
                    entry.tags = normalize_tags(tags);
                }
                entry.updated_at = now_secs();
                let updated = entry.clone();
                self.save(scope, &file)?;
                return Ok(updated);
            }
        }
        Err(format!("memory '{id}' was not found"))
    }

    /// Record that a recall actually returned `ids`, so their freshness is
    /// reinforced. Failures are ignored by callers: recall must never break a
    /// turn because a memory file could not be rewritten.
    pub(crate) fn touch(&self, ids: &[String]) -> Result<(), String> {
        if ids.is_empty() {
            return Ok(());
        }
        let now = now_secs();
        for scope in [MemoryScope::Global, MemoryScope::Project] {
            let mut file = self.load(scope)?;
            let mut changed = false;
            for entry in &mut file.entries {
                if ids.iter().any(|id| id == &entry.id) {
                    entry.last_used_at = Some(now);
                    entry.use_count = entry.use_count.saturating_add(1);
                    changed = true;
                }
            }
            if changed {
                self.save(scope, &file)?;
            }
        }
        Ok(())
    }

    pub(crate) fn forget(&self, id: &str) -> Result<(), String> {
        for scope in [MemoryScope::Global, MemoryScope::Project] {
            let mut file = self.load(scope)?;
            let original_len = file.entries.len();
            file.entries.retain(|entry| entry.id != id);
            if file.entries.len() != original_len {
                self.save(scope, &file)?;
                return Ok(());
            }
        }
        Err(format!("memory '{id}' was not found"))
    }

    pub(crate) fn retrieve(
        &self,
        query: &str,
        config: &MemoryConfig,
    ) -> Result<Vec<MemoryEntry>, String> {
        if !config.enabled || query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let query_tokens = tokens(query);
        if query_tokens.is_empty() {
            return Ok(Vec::new());
        }
        let mut scored = Vec::new();
        for (scope, limit) in [
            (MemoryScope::Global, config.max_global_results),
            (MemoryScope::Project, config.max_project_results),
        ] {
            let scope_enabled = match scope {
                MemoryScope::Global => config.global_enabled,
                MemoryScope::Project => config.project_enabled,
            };
            if !scope_enabled || limit == 0 {
                continue;
            }
            let mut scope_entries = self
                .load(scope)?
                .entries
                .into_iter()
                .filter(|entry| entry.status == MemoryStatus::Active)
                .filter_map(|entry| {
                    let score = relevance(&entry, &query_tokens);
                    (score > 0.0).then_some((score, entry))
                })
                .collect::<Vec<_>>();
            scope_entries
                .sort_by(|left, right| right.0.partial_cmp(&left.0).unwrap_or(Ordering::Equal));
            scored.extend(scope_entries.into_iter().take(limit));
        }
        scored.sort_by(|left, right| right.0.partial_cmp(&left.0).unwrap_or(Ordering::Equal));
        Ok(scored.into_iter().map(|(_, entry)| entry).collect())
    }

    pub(crate) fn render_list(entries: &[MemoryEntry]) -> String {
        if entries.is_empty() {
            return "No active memories.".to_string();
        }
        let now = now_secs();
        entries
            .iter()
            .map(|entry| {
                format!(
                    "{}  {:7}  {:12}  saved {:12}  {}",
                    entry.id,
                    entry.scope.label(),
                    entry.kind.label(),
                    age_label(entry, now),
                    entry.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn path(&self, scope: MemoryScope) -> &Path {
        match scope {
            MemoryScope::Global => &self.global_path,
            MemoryScope::Project => &self.project_path,
        }
    }

    fn directory(&self, scope: MemoryScope) -> PathBuf {
        self.path(scope).with_extension("")
    }

    fn load(&self, scope: MemoryScope) -> Result<MemoryFile, String> {
        let dir = self.directory(scope);
        let index = dir.join("MEMORY.md");
        if index.exists() {
            let mut entries = Vec::new();
            for line in std::fs::read_to_string(&index)
                .map_err(|e| format!("read {}: {e}", index.display()))?
                .lines()
            {
                if let Some(name) = line
                    .strip_prefix("- ")
                    .and_then(|s| s.split_whitespace().next())
                {
                    let p = dir.join(name);
                    if p.extension().and_then(|x| x.to_str()) == Some("md") && p.exists() {
                        let text = std::fs::read_to_string(&p)
                            .map_err(|e| format!("read {}: {e}", p.display()))?;
                        if let Some(metadata) = text
                            .strip_prefix("<!-- ")
                            .and_then(|s| s.split_once(" -->\n"))
                            .map(|x| x.0)
                        {
                            entries.push(
                                serde_json::from_str(metadata)
                                    .map_err(|e| format!("parse {}: {e}", p.display()))?,
                            );
                        }
                    }
                }
            }
            return Ok(MemoryFile {
                schema_version: SCHEMA_VERSION,
                workspace_path: self.workspace_path(scope),
                entries,
            });
        }
        // One-time, lossless migration from the legacy JSON store.
        let old = self.path(scope);
        if old.exists() {
            let file: MemoryFile = serde_json::from_slice(
                &std::fs::read(old).map_err(|e| format!("read {}: {e}", old.display()))?,
            )
            .map_err(|e| format!("parse {}: {e}", old.display()))?;
            self.save(scope, &file)?;
            return Ok(file);
        }
        Ok(MemoryFile::empty(self.workspace_path(scope)))
    }

    fn save(&self, scope: MemoryScope, file: &MemoryFile) -> Result<(), String> {
        let dir = self.directory(scope);
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let mut index = String::from("# Memory\n\n");
        for entry in &file.entries {
            let name = format!("{}.md", entry.id);
            let json =
                serde_json::to_string(entry).map_err(|e| format!("serialize memory: {e}"))?;
            std::fs::write(
                dir.join(&name),
                format!(
                    "<!-- {json} -->\n# {}\n\n{}\n",
                    entry.kind.label(),
                    entry.content
                ),
            )
            .map_err(|e| format!("write memory: {e}"))?;
            index.push_str(&format!(
                "- {name} {}\n",
                entry.content.lines().next().unwrap_or("")
            ));
        }
        std::fs::write(dir.join("MEMORY.md"), index).map_err(|e| format!("write index: {e}"))
    }

    fn workspace_path(&self, scope: MemoryScope) -> Option<String> {
        (scope == MemoryScope::Project).then(|| self.workspace_root.display().to_string())
    }
}

fn workspace_root(working_dir: &Path) -> PathBuf {
    let canonical = working_dir
        .canonicalize()
        .unwrap_or_else(|_| working_dir.to_path_buf());
    canonical
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .unwrap_or(&canonical)
        .to_path_buf()
}

fn normalize_tags(tags: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    tags.into_iter()
        .map(|tag| tag.trim().to_ascii_lowercase())
        .filter(|tag| !tag.is_empty() && seen.insert(tag.clone()))
        .take(16)
        .collect()
}

fn validate_content(content: &str) -> Result<(), String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err("memory content must not be empty".to_string());
    }
    if trimmed.chars().count() > 2_000 {
        return Err("memory content exceeds the 2000 character limit".to_string());
    }
    let lower = trimmed.to_ascii_lowercase();
    let sensitive_markers = [
        "-----begin private key-----",
        "-----begin rsa private key-----",
        "authorization: bearer ",
        "api_key=",
        "api-key=",
        "password=",
        "secret_access_key",
    ];
    if sensitive_markers
        .iter()
        .any(|marker| lower.contains(marker))
        || lower.split_whitespace().any(looks_like_secret_token)
    {
        return Err("memory content appears to contain a credential or secret".to_string());
    }
    Ok(())
}

fn looks_like_secret_token(token: &str) -> bool {
    let token = token.trim_matches(|character: char| {
        !character.is_ascii_alphanumeric() && character != '-' && character != '_'
    });
    (token.starts_with("sk-") && token.len() >= 20)
        || (token.starts_with("ghp_") && token.len() >= 20)
        || (token.starts_with("github_pat_") && token.len() >= 20)
}

fn tokens(text: &str) -> HashSet<String> {
    let mut tokens = HashSet::new();
    for word in text.split(|character: char| {
        !character.is_alphanumeric()
            && character != '_'
            && character != '-'
            && character != '/'
            && character != '.'
    }) {
        let word = word.trim().to_ascii_lowercase();
        let characters = word.chars().collect::<Vec<_>>();
        if characters.len() >= 2 {
            tokens.insert(word);
        }
        if characters.iter().any(|character| !character.is_ascii()) {
            tokens.extend(
                characters
                    .windows(2)
                    .map(|pair| pair.iter().collect::<String>()),
            );
        }
    }
    tokens
}

/// How strongly a memory still counts, as a Claude Code style freshness
/// weight: exponential decay since the memory was last reinforced, damped by
/// per-kind half-lives and lifted a little every time a recall actually used
/// it. Recalled memories therefore stay warm, while never-recalled ones fade
/// without ever being deleted.
fn freshness(entry: &MemoryEntry, now: u64) -> f32 {
    let reinforced_at = entry.last_used_at.unwrap_or(0).max(entry.updated_at);
    let age_hours = now.saturating_sub(reinforced_at) as f32 / 3600.0;
    let decay = 0.5_f32.powf(age_hours / entry.kind.half_life_hours());
    let reinforcement = 1.0 + (entry.use_count as f32).min(20.0) * 0.05;
    (decay * reinforcement).min(1.0)
}

fn relevance(entry: &MemoryEntry, query_tokens: &HashSet<String>) -> f32 {
    let content_tokens = tokens(&entry.content);
    let tag_tokens = entry
        .tags
        .iter()
        .flat_map(|tag| tokens(tag))
        .collect::<HashSet<_>>();
    let content_overlap = query_tokens.intersection(&content_tokens).count() as f32;
    let tag_overlap = query_tokens.intersection(&tag_tokens).count() as f32;
    if content_overlap == 0.0 && tag_overlap == 0.0 {
        return 0.0;
    }
    let keyword = content_overlap * 2.0
        + tag_overlap * 4.0
        + entry.kind.priority()
        + entry.confidence.score();
    keyword * freshness(entry, now_secs())
}

/// Whole days since a memory was last written. Age is measured from
/// `updated_at`, the last time its content changed, not from `last_used_at`
/// — being recalled says nothing about whether the claim is still true.
pub(crate) fn age_days(entry: &MemoryEntry, now: u64) -> u64 {
    now.saturating_sub(entry.updated_at) / 86_400
}

/// Human-readable age. Models are poor at date arithmetic, and a raw
/// timestamp does not trigger staleness reasoning the way "47 days ago" does.
pub(crate) fn age_label(entry: &MemoryEntry, now: u64) -> String {
    match age_days(entry, now) {
        0 => "today".to_string(),
        1 => "yesterday".to_string(),
        days => format!("{days} days ago"),
    }
}

/// Claude Code style staleness caveat for memories older than a day, and
/// `None` for fresh ones — warning about today's memory is just noise.
///
/// Stale memories are dropped in weight but never deleted, so this is the
/// only thing standing between a model and an old claim: it addresses the
/// case where a `file:line` citation makes a stale statement sound *more*
/// authoritative rather than less.
pub(crate) fn freshness_caveat(entry: &MemoryEntry, now: u64) -> Option<String> {
    let days = age_days(entry, now);
    if days <= 1 {
        return None;
    }
    Some(format!(
        "This memory is {days} days old. Memories are point-in-time observations, not live state — \
         claims about code behavior or file:line citations may be outdated. \
         Verify against current code before asserting as fact."
    ))
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager() -> MemoryManager {
        let root =
            std::env::temp_dir().join(format!("programmer-memory-test-{}", uuid::Uuid::new_v4()));
        MemoryManager::new(root.join("config"), root.join("workspace"))
    }

    #[test]
    fn structured_session_memory_parses_compaction_headings() {
        let memory = SessionMemory::from_summary(
            "## INTENT\nAdd memory\n## STATE\nStore exists\n## IN FLIGHT\nTesting\n## NEXT\nRun tests",
        )
        .unwrap();

        assert_eq!(memory.intent, "Add memory");
        assert_eq!(memory.state, "Store exists");
        assert_eq!(memory.in_flight, "Testing");
        assert_eq!(memory.next, "Run tests");
    }

    #[test]
    fn project_and_global_memories_are_isolated() {
        let manager = manager();
        manager
            .remember(
                MemoryScope::Project,
                MemoryKind::Decision,
                "Use JSON storage".into(),
                vec![],
            )
            .unwrap();
        manager
            .remember(
                MemoryScope::Global,
                MemoryKind::Preference,
                "Answer concisely".into(),
                vec![],
            )
            .unwrap();

        assert_eq!(manager.list(Some(MemoryScope::Project)).unwrap().len(), 1);
        assert_eq!(manager.list(Some(MemoryScope::Global)).unwrap().len(), 1);
        assert_eq!(manager.list(None).unwrap().len(), 2);
    }

    #[test]
    fn retrieval_ranks_matching_memory_and_obeys_scope_limits() {
        let manager = manager();
        manager
            .remember(
                MemoryScope::Project,
                MemoryKind::Decision,
                "Store sessions as JSON".into(),
                vec!["session".into()],
            )
            .unwrap();
        manager
            .remember(
                MemoryScope::Project,
                MemoryKind::Workflow,
                "Run cargo test".into(),
                vec!["test".into()],
            )
            .unwrap();
        let config = MemoryConfig {
            max_project_results: 1,
            global_enabled: false,
            ..Default::default()
        };

        let entries = manager.retrieve("session JSON format", &config).unwrap();

        assert_eq!(entries.len(), 1);
        assert!(entries[0].content.contains("JSON"));
        assert!(
            manager
                .retrieve("unrelated banana", &config)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn retrieval_supports_cjk_bigrams() {
        let manager = manager();
        manager
            .remember(
                MemoryScope::Project,
                MemoryKind::Convention,
                "测试前先运行格式化检查".into(),
                vec![],
            )
            .unwrap();

        let entries = manager
            .retrieve("请运行格式化", &MemoryConfig::default())
            .unwrap();

        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn update_and_forget_are_persisted() {
        let manager = manager();
        let entry = manager
            .remember(
                MemoryScope::Project,
                MemoryKind::ProjectFact,
                "Old fact".into(),
                vec![],
            )
            .unwrap();
        manager
            .update(&entry.id, Some("New fact".into()), None, None)
            .unwrap();
        assert_eq!(manager.list(None).unwrap()[0].content, "New fact");
        manager.forget(&entry.id).unwrap();
        assert!(manager.list(None).unwrap().is_empty());
    }

    fn entry_with_age(kind: MemoryKind, age_hours: u64, use_count: u32) -> MemoryEntry {
        let now = now_secs();
        let created_at = now.saturating_sub(age_hours * 3600);
        MemoryEntry {
            id: "mem_test".into(),
            scope: MemoryScope::Project,
            kind,
            content: "Store sessions as JSON".into(),
            tags: Vec::new(),
            created_at,
            updated_at: created_at,
            last_used_at: None,
            use_count,
            source: MemorySource::AgentTool,
            confidence: MemoryConfidence::Explicit,
            status: MemoryStatus::Active,
            supersedes: None,
        }
    }

    #[test]
    fn freshness_decays_with_age_and_never_reaches_zero() {
        let now = now_secs();
        let fresh = entry_with_age(MemoryKind::ProjectFact, 0, 0);
        let stale = entry_with_age(MemoryKind::ProjectFact, 24 * 90, 0);
        let ancient = entry_with_age(MemoryKind::ProjectFact, 24 * 360, 0);

        assert!(freshness(&fresh, now) > 0.99);
        assert!(freshness(&stale, now) < freshness(&fresh, now));
        // Half-life decay, not expiry: old memories stay recalled, just weaker.
        assert!(freshness(&ancient, now) > 0.0);
    }

    #[test]
    fn durable_kinds_decay_slower_than_volatile_ones() {
        let now = now_secs();
        let preference = entry_with_age(MemoryKind::Preference, 24 * 90, 0);
        let workflow = entry_with_age(MemoryKind::Workflow, 24 * 90, 0);

        assert!(freshness(&preference, now) > freshness(&workflow, now));
    }

    #[test]
    fn recall_reinforces_freshness() {
        let now = now_secs();
        let unused = entry_with_age(MemoryKind::Workflow, 24 * 60, 0);
        let mut reused = entry_with_age(MemoryKind::Workflow, 24 * 60, 0);
        reused.last_used_at = Some(now);

        assert!(freshness(&reused, now) > freshness(&unused, now));
        assert!(freshness(&reused, now) > 0.99);
    }

    #[test]
    fn freshness_caveat_is_silent_for_fresh_memories_and_names_the_age_for_old_ones() {
        let now = now_secs();
        let today = entry_with_age(MemoryKind::ProjectFact, 2, 0);
        let yesterday = entry_with_age(MemoryKind::ProjectFact, 30, 0);
        let old = entry_with_age(MemoryKind::ProjectFact, 24 * 47, 0);

        assert_eq!(age_label(&today, now), "today");
        assert_eq!(age_label(&yesterday, now), "yesterday");
        assert_eq!(age_label(&old, now), "47 days ago");
        assert!(freshness_caveat(&today, now).is_none());
        // A day-old memory is still fresh: warning there would only be noise.
        assert!(freshness_caveat(&yesterday, now).is_none());

        let caveat = freshness_caveat(&old, now).expect("caveat for a 47 day old memory");
        assert!(caveat.contains("This memory is 47 days old"));
        assert!(caveat.contains("point-in-time observations, not live state"));
    }

    #[test]
    fn memory_age_is_measured_from_the_last_edit_not_from_recall() {
        let now = now_secs();
        let mut entry = entry_with_age(MemoryKind::Preference, 24 * 30, 0);
        entry.last_used_at = Some(now);

        // Being recalled today does not make a month-old claim fresh.
        assert_eq!(age_days(&entry, now), 30);
        assert!(freshness_caveat(&entry, now).is_some());
    }

    #[test]
    fn touch_records_recall_usage_and_persists_it() {
        let manager = manager();
        let entry = manager
            .remember(
                MemoryScope::Project,
                MemoryKind::ProjectFact,
                "Store sessions as JSON".into(),
                vec![],
            )
            .unwrap();
        assert_eq!(entry.use_count, 0);
        assert_eq!(entry.last_used_at, None);

        manager.touch(std::slice::from_ref(&entry.id)).unwrap();

        let stored = manager.list(Some(MemoryScope::Project)).unwrap();
        assert_eq!(stored[0].use_count, 1);
        assert!(stored[0].last_used_at.is_some());
        // Unknown ids are ignored rather than failing the recall.
        manager.touch(&["mem_missing".into()]).unwrap();
    }

    #[test]
    fn credentials_are_rejected() {
        assert!(validate_content("api_key=super-secret-value").is_err());
        assert!(validate_content("sk-abcdefghijklmnopqrstuvwxyz").is_err());
        assert!(validate_content("Prefer cargo check before tests").is_ok());
    }
}
