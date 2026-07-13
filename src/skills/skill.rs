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

//! A single skill — loaded from a `SKILL.md` file with YAML frontmatter.
//!
//! Format:
//! ```markdown
//! ---
//! name: my-skill
//! description: What this skill does.
//! metadata:
//!   author: someone
//!   version: "1.0.0"
//! ---
//!
//! # Body
//! Markdown body…
//! ```

use std::path::{Path, PathBuf};

/// Where a skill file was loaded from.
#[derive(Debug, Clone)]
pub(crate) enum SkillSource {
    /// Project-scoped: `.programmer/skills/<name>/SKILL.md`.
    Project(PathBuf),
    /// User-global: `~/.config/programmer/skills/<name>/SKILL.md`.
    Global(PathBuf),
}

/// A skill loaded from a `SKILL.md` file.
#[derive(Debug, Clone)]
pub(crate) struct Skill {
    pub(crate) name: String,
    pub(crate) description: String,
    /// Arbitrary metadata pulled from the YAML frontmatter (e.g. `author`,
    /// `version`, `argument-hint`). Stored as a JSON value so skills can carry
    /// structured hints for the model.
    pub(crate) metadata: serde_json::Value,
    /// The markdown body (everything after the closing `---` divider).
    pub(crate) body: String,
    /// Where this skill was loaded from.
    pub(crate) source: SkillSource,
}

impl Skill {
    /// Parse a `SKILL.md` file from `path`, which may be a directory
    /// containing a `SKILL.md` or a direct `.md` file.
    pub(crate) fn from_path(path: &Path) -> Option<Self> {
        let file = if path.is_dir() {
            path.join("SKILL.md")
        } else {
            path.to_path_buf()
        };

        let raw = std::fs::read_to_string(&file).ok()?;
        let (front, body) = split_frontmatter(&raw)?;
        let (name, description, metadata) = parse_frontmatter(&front)?;

        Some(Skill {
            name,
            description,
            metadata,
            body: body.to_string(),
            source: SkillSource::Project(file),
        })
    }

    /// Re-tag the source as global.
    pub(crate) fn with_global_source(mut self) -> Self {
        if let SkillSource::Project(p) = self.source {
            self.source = SkillSource::Global(p);
        }
        self
    }

    /// The full prompt text injected into the system prompt when this skill
    /// is activated: title + description + body.
    pub(crate) fn to_prompt(&self) -> String {
        let mut prompt = format!(
            "## Skill: {}\n\n*{}. If this skill is active, follow its instructions.*\n\n{}",
            self.name, self.description, self.body
        );
        // 80 KiB safety cap — skills shouldn't be enormous, but avoid token bombs.
        if prompt.len() > 81920 {
            let truncate_at = 81920 - 128;
            prompt.truncate(truncate_at);
            prompt.push_str("\n\n[... skill truncated — too large ...]");
        }
        prompt
    }
}

// ---------------------------------------------------------------------------
// Frontmatter parsing
// ---------------------------------------------------------------------------

/// Split raw file content into (frontmatter, body). Returns `None` if the
/// file doesn't start with `---`.
fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let rest = raw.strip_prefix("---\n")?;
    let (front, body) = rest.split_once("\n---")?;
    // body may start with a newline; trim exactly one leading \n.
    let body = body.strip_prefix('\n').unwrap_or(body);
    Some((front, body))
}

/// Parse YAML frontmatter into (name, description, metadata).
/// The name is required; everything else is optional.
fn parse_frontmatter(front: &str) -> Option<(String, String, serde_json::Value)> {
    let mut name: Option<String> = None;
    let mut description = String::new();
    let mut meta = serde_json::Map::new();
    let mut in_metadata = false;
    let mut metadata_indent = 0usize;

    for line in front.lines() {
        // Handle nested metadata block.
        if in_metadata {
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            if trimmed.is_empty() || indent <= metadata_indent {
                // Exited the metadata block — fall through to parse this line
                // as a regular top-level key.
                in_metadata = false;
            } else {
                // Simple nested key: "  author: vercel"
                if let Some((k, v)) = trimmed.split_once(':') {
                    let v = v.trim().trim_matches('"');
                    meta.insert(k.trim().to_string(), serde_json::Value::String(v.to_string()));
                }
                continue;
            }
        }

        // Top-level keys.
        let trimmed = line.trim();
        if let Some((k, v)) = trimmed.split_once(':') {
            let key = k.trim();
            let val = v.trim().trim_matches('"');
            match key {
                "name" => name = Some(val.to_string()),
                "description" => {
                    if !description.is_empty() {
                        description.push(' ');
                    }
                    description.push_str(val);
                }
                "metadata" => {
                    in_metadata = true;
                    metadata_indent = line.len() - line.trim_start().len();
                }
                _ => {
                    meta.insert(key.to_string(), serde_json::Value::String(val.to_string()));
                }
            }
        }
    }

    let name = name?;
    Some((name, description, serde_json::Value::Object(meta)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_skill() {
        let raw = "---\nname: test-skill\ndescription: A test skill\n---\n\n# Body\nSome body text.";
        let (front, body) = split_frontmatter(raw).unwrap();
        assert!(front.contains("name: test-skill"));
        assert_eq!(body, "\n# Body\nSome body text.");

        let (name, desc, meta) = parse_frontmatter(front).unwrap();
        assert_eq!(name, "test-skill");
        assert_eq!(desc, "A test skill");
        assert!(meta.as_object().unwrap().is_empty());
    }

    #[test]
    fn parse_with_metadata() {
        let raw = "---\nname: advanced\ndescription: An advanced skill\nmetadata:\n  author: vercel\n  version: \"1.0.0\"\nlicense: MIT\n---\n\n# Advanced\nBody here.";
        let (front, _body) = split_frontmatter(raw).unwrap();
        let (name, desc, meta) = parse_frontmatter(front).unwrap();
        assert_eq!(name, "advanced");
        assert_eq!(desc, "An advanced skill");
        let obj = meta.as_object().unwrap();
        assert_eq!(obj["author"], "vercel");
        assert_eq!(obj["version"], "1.0.0");
        assert_eq!(obj["license"], "MIT");
    }

    #[test]
    fn no_frontmatter() {
        assert!(split_frontmatter("just plain text").is_none());
    }

    #[test]
    fn missing_name() {
        let raw = "---\ndescription: no name\n---\n\nBody.";
        let (front, _) = split_frontmatter(raw).unwrap();
        assert!(parse_frontmatter(front).is_none());
    }

    #[test]
    fn to_prompt_includes_name_and_body() {
        let skill = Skill {
            name: "test".into(),
            description: "a test".into(),
            metadata: serde_json::Value::Object(Default::default()),
            body: "Do the thing.".into(),
            source: SkillSource::Project(PathBuf::from(".")),
        };
        let prompt = skill.to_prompt();
        assert!(prompt.contains("## Skill: test"));
        assert!(prompt.contains("Do the thing."));
    }
}
