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

//! Agent skills: reusable prompt + constraint modules loaded from
//! `SKILL.md` files. Compatible with the [Vercel Labs skills
//! ecosystem](https://github.com/vercel-labs/skills).
//!
//! Skills are discovered in:
//! - Built-in: compiled into the `programmer` binary
//! - Shared: `~/.agents/skills/<name>/SKILL.md`
//! - Global: the platform config directory's `programmer/skills/<name>/SKILL.md`
//! - Project: `.programmer/skills/<name>/SKILL.md`
//!
//! Project skills shadow global, shared, and built-in skills with the same
//! name. Global skills shadow shared and built-in skills; shared skills shadow
//! built-ins.

pub mod skill;

use skill::{Skill, SkillSource};
use std::path::PathBuf;

const BUILTIN_SKILLS: &[&str] = &[include_str!("builtin/programmer-guide/SKILL.md")];

/// Manages installed and activated skills for the session.
#[derive(Debug, Clone, Default)]
pub struct SkillRegistry {
    /// All discovered skills, keyed by name, with later sources overriding
    /// earlier ones in built-in → shared → global → project order.
    skills: std::collections::HashMap<String, Skill>,
    /// Names of currently activated skills (in activation order).
    activated: Vec<String>,
}

impl SkillRegistry {
    /// Load every skill source in increasing precedence order.
    pub(crate) fn load() -> Self {
        let mut registry = SkillRegistry::default();

        // Built-ins are the lowest-precedence source.
        registry.load_builtins();
        // Cross-agent skills are available without Programmer-specific setup.
        if let Some(dir) = shared_skills_dir() {
            registry.scan_dir(&dir, SkillSource::Shared);
        }
        // Programmer-global skills override shared ones.
        if let Some(dir) = global_skills_dir() {
            registry.scan_dir(&dir, SkillSource::Global);
        }
        // Project skills override every other source.
        let project_dir = project_skills_dir();
        registry.scan_dir(&project_dir, SkillSource::Project);

        registry
    }

    fn load_builtins(&mut self) {
        for &raw in BUILTIN_SKILLS {
            if let Some(skill) = Skill::from_builtin(raw) {
                self.skills.insert(skill.name.clone(), skill);
            }
        }
    }

    /// Scan a directory for skills. Each subdirectory containing a `SKILL.md`
    /// counts as one skill. Direct `.md` files also work.
    fn scan_dir(&mut self, dir: &std::path::Path, source: SkillSource) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let skill = match Skill::from_path(&path) {
                Some(s) => s,
                None => continue,
            }
            .with_source(source);
            // Later sources replace earlier ones with the same name.
            self.skills.insert(skill.name.clone(), skill);
        }
    }

    // -- activation --

    /// Activate a skill by name. Returns `true` if the skill was found and
    /// activated. If the skill is already active it is a no-op (returns true).
    pub(crate) fn activate(&mut self, name: &str) -> bool {
        if !self.skills.contains_key(name) {
            return false;
        }
        // Move to the end if already present, otherwise append.
        self.activated.retain(|n| n != name);
        self.activated.push(name.to_string());
        true
    }

    /// Deactivate a single skill by name. Returns `true` if it was active.
    pub(crate) fn deactivate(&mut self, name: &str) -> bool {
        let before = self.activated.len();
        self.activated.retain(|n| n != name);
        self.activated.len() != before
    }

    /// Toggle a skill's active state. Returns the new state (`true` = active),
    /// or `None` if no such skill is installed.
    pub(crate) fn toggle(&mut self, name: &str) -> Option<bool> {
        if !self.skills.contains_key(name) {
            return None;
        }
        if self.is_active(name) {
            self.deactivate(name);
            Some(false)
        } else {
            self.activate(name);
            Some(true)
        }
    }

    /// Whether a specific skill is currently active.
    pub(crate) fn is_active(&self, name: &str) -> bool {
        self.activated.iter().any(|n| n == name)
    }

    /// Deactivate all skills.
    pub(crate) fn clear(&mut self) {
        self.activated.clear();
    }

    /// Names of currently activated skills.
    pub(crate) fn activated_names(&self) -> &[String] {
        &self.activated
    }

    /// Set the entire active set (used on session restore).
    pub(crate) fn set_activated(&mut self, names: &[String]) {
        self.activated.clear();
        for name in names {
            if self.skills.contains_key(name) {
                self.activated.push(name.clone());
            }
        }
    }

    // -- listing --

    /// All loaded skill names, in alphabetical order.
    pub(crate) fn names(&self) -> Vec<&String> {
        let mut names: Vec<&String> = self.skills.keys().collect();
        names.sort();
        names
    }

    /// Get a loaded skill by name.
    pub(crate) fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// Whether any skills are currently active.
    pub(crate) fn has_active(&self) -> bool {
        !self.activated.is_empty()
    }

    // -- prompt generation --

    /// Build the combined prompt text for all active skills. Returns `None`
    /// when no skills are active.
    pub(crate) fn combined_prompt(&self) -> Option<String> {
        if self.activated.is_empty() {
            return None;
        }

        let mut parts = Vec::new();
        for name in &self.activated {
            if let Some(skill) = self.skills.get(name) {
                parts.push(skill.to_prompt());
            }
        }
        if parts.is_empty() {
            return None;
        }
        Some(parts.join("\n\n"))
    }
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Project skill directory: `.programmer/skills/`.
fn project_skills_dir() -> PathBuf {
    PathBuf::from(".programmer").join("skills")
}

/// Cross-agent shared skill directory: `~/.agents/skills/`.
fn shared_skills_dir() -> Option<PathBuf> {
    let dir = dirs::home_dir()?.join(".agents").join("skills");
    Some(dir)
}

/// Programmer-global skill directory under the platform config directory.
fn global_skills_dir() -> Option<PathBuf> {
    let dir = dirs::config_dir()?.join("programmer").join("skills");
    Some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_has_no_active() {
        let reg = SkillRegistry::default();
        assert!(!reg.has_active());
        assert!(reg.names().is_empty());
        assert!(reg.combined_prompt().is_none());
    }

    #[test]
    fn activate_unknown_returns_false() {
        let mut reg = SkillRegistry::default();
        assert!(!reg.activate("nonexistent"));
    }

    #[test]
    fn combined_prompt_empty_when_no_active_skills() {
        let reg = SkillRegistry::default();
        assert!(reg.combined_prompt().is_none());
    }

    #[test]
    fn built_in_programmer_guide_is_available() {
        let mut reg = SkillRegistry::default();
        reg.load_builtins();

        let skill = reg
            .get("programmer-guide")
            .expect("built-in skill should load");
        assert_eq!(skill.source, SkillSource::BuiltIn);
        assert!(skill.body.contains("Programmer's identity"));
        assert!(reg.activate("programmer-guide"));
        assert!(
            reg.combined_prompt()
                .unwrap()
                .contains("## Skill: programmer-guide")
        );
    }

    #[test]
    fn load_skills_from_disk() {
        use std::io::Write;

        let dir = std::env::temp_dir().join(format!("programmer_skills_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let skills_dir = dir.join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        // Create a skill: skills/hello/SKILL.md
        let skill_dir = skills_dir.join("hello");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let mut file = std::fs::File::create(skill_dir.join("SKILL.md")).unwrap();
        writeln!(file, "---").unwrap();
        writeln!(file, "name: hello").unwrap();
        writeln!(file, "description: greets the world").unwrap();
        writeln!(file, "---").unwrap();
        writeln!(file).unwrap();
        writeln!(file, "# Hello").unwrap();
        writeln!(file, "Hello, world!").unwrap();

        // Direct .md file as a skill source
        let direct_skill = skills_dir.join("react-best-practices.md");
        let mut file = std::fs::File::create(&direct_skill).unwrap();
        writeln!(file, "---").unwrap();
        writeln!(file, "name: react-best-practices").unwrap();
        writeln!(file, "description: react patterns").unwrap();
        writeln!(file, "---").unwrap();
        writeln!(file).unwrap();
        writeln!(file, "Use React.memo").unwrap();

        let mut reg = SkillRegistry::default();
        reg.scan_dir(&skills_dir, SkillSource::Project);

        let mut names = reg.names();
        names.sort();
        assert_eq!(names, vec!["hello", "react-best-practices"]);

        assert!(reg.get("hello").unwrap().body.contains("Hello, world!"));
        assert!(
            reg.get("react-best-practices")
                .unwrap()
                .body
                .contains("React.memo")
        );

        // Activate
        assert!(reg.activate("hello"));
        assert!(reg.has_active());
        let prompt = reg.combined_prompt().unwrap();
        assert!(prompt.contains("## Skill: hello"));

        // Activate another — should stack
        assert!(reg.activate("react-best-practices"));
        assert_eq!(reg.activated_names().len(), 2);

        // Deactivate
        reg.clear();
        assert!(!reg.has_active());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn disk_skill_sources_follow_shared_global_project_precedence() {
        fn write_skill(dir: &std::path::Path, body: &str) {
            let skill_dir = dir.join("same-name");
            std::fs::create_dir_all(&skill_dir).unwrap();
            std::fs::write(
                skill_dir.join("SKILL.md"),
                format!("---\nname: same-name\ndescription: precedence test\n---\n\n{body}\n"),
            )
            .unwrap();
        }

        let root = std::env::temp_dir().join(format!(
            "programmer_skill_precedence_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let shared = root.join("shared");
        let global = root.join("global");
        let project = root.join("project");
        write_skill(&shared, "shared body");
        write_skill(&global, "global body");
        write_skill(&project, "project body");

        let mut reg = SkillRegistry::default();
        reg.scan_dir(&shared, SkillSource::Shared);
        assert_eq!(reg.get("same-name").unwrap().source, SkillSource::Shared);
        reg.scan_dir(&global, SkillSource::Global);
        assert_eq!(reg.get("same-name").unwrap().source, SkillSource::Global);
        reg.scan_dir(&project, SkillSource::Project);
        let selected = reg.get("same-name").unwrap();
        assert_eq!(selected.source, SkillSource::Project);
        assert!(selected.body.contains("project body"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
