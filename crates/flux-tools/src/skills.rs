//! Skill tools — lazy-loading access to Agent Skills packages.
//!
//! A skill is a self-contained capability package: a directory carrying a
//! `SKILL.md` (frontmatter `name` + `description`, body = instructions)
//! plus optional references/scripts/assets. Discovery is progressive
//! disclosure BY TOOLS — nothing is injected into any prompt. The two
//! tools' own descriptions (which ride every request's `tools` array) are
//! the only always-visible surface; the content itself loads only when
//! the model calls `skill_read` (and `skill_list` scans on every call, so
//! installed skills appear without any restart semantics).
//!
//! Locations (both follow the same `<root>/.flux/skills` layout):
//! - project: `<workdir>/.flux/skills/` — inside the chat boundary
//! - global: `$HOME/.flux/skills/` (`USERPROFILE` fallback) —
//!   user-installed content, OUTSIDE the workdir boundary by design (the
//!   same trust tier as MCP servers launched from the DB: the user opted
//!   in by installing it)
//!
//! The boundary exception is structural, not a hole: `skill_read` is
//! NAME-keyed — the model never passes a path to it. The requested file
//! resolves relative to the discovered skill's directory under a strict
//! containment check (canonicalize + prefix, symlink-safe), so a read
//! cannot leave the skill directory regardless of the arguments.

use flux_core::{CoreError, ToolCtx};
use std::path::{Path, PathBuf};

/// Cap for one skill file read (scan and read share it) — SKILL.md files
/// are instructions, not data dumps; anything larger is not a skill file.
const MAX_SKILL_FILE_BYTES: u64 = 256 * 1024;
/// Depth cap for the discovery walk below a skills root — skill packages
/// are shallow; a deep tree is content, not a skill location.
const MAX_SCAN_DEPTH: usize = 8;

// ── Discovery ───────────────────────────────────────────────────────────────

/// One discovered skill: identity (name + description) plus the directory
/// the model's reads are scoped to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    /// Directory containing the skill's `SKILL.md` (canonicalized).
    pub root: PathBuf,
    /// `true` = project-local; project entries win name collisions.
    pub project: bool,
}

/// Discover skills from the given locations: global first, then project —
/// on a name collision the PROJECT entry wins (the more specific location
/// is the one the user means). Missing directories are simply absent
/// skill sources. The result is sorted by name for stable output.
pub fn discover_skills(global_dir: Option<&Path>, project_dir: Option<&Path>) -> Vec<SkillEntry> {
    let mut entries: Vec<SkillEntry> = Vec::new();
    if let Some(global) = global_dir {
        scan_skills_dir(global, false, &mut entries);
    }
    if let Some(project) = project_dir {
        let mut project_entries: Vec<SkillEntry> = Vec::new();
        scan_skills_dir(project, true, &mut project_entries);
        for pe in project_entries {
            match entries.iter_mut().find(|e| e.name == pe.name) {
                Some(existing) => *existing = pe, // project overrides global
                None => entries.push(pe),
            }
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

/// The global skills dir: `$HOME/.flux/skills` (`USERPROFILE` fallback).
/// `None` when no home directory is set — project skills still work.
fn global_skills_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| Path::new(&home).join(".flux").join("skills"))
}

/// Walk a skills root (explicit stack, symlink-safe, depth-capped) and
/// collect every directory carrying a usable `SKILL.md`. Skill directories
/// are not descended into — a skill's own references/assets are content,
/// not nested skill locations.
fn scan_skills_dir(root: &Path, project: bool, out: &mut Vec<SkillEntry>) {
    let Ok(root_real) = root.canonicalize() else {
        return; // absent location — no skills from here
    };
    let mut stack: Vec<(PathBuf, usize)> = vec![(root_real.clone(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if let Some(entry) = try_skill(&dir, project) {
            out.push(entry);
            continue;
        }
        if depth >= MAX_SCAN_DEPTH {
            continue;
        }
        let Ok(children) = std::fs::read_dir(&dir) else {
            continue; // unreadable — skip (same posture as walk_dir)
        };
        for child in children.filter_map(|e| e.ok()) {
            let Ok(real) = child.path().canonicalize() else {
                continue; // dangling symlink — skip
            };
            if !real.starts_with(&root_real) {
                continue; // symlink escape — never leave the skills root
            }
            if real.is_dir() {
                stack.push((real, depth + 1));
            }
        }
    }
}

/// A directory is a skill when its `SKILL.md` parses to a non-empty
/// description and a usable name (declared `name` if valid, else the
/// directory name if that is valid).
fn try_skill(dir: &Path, project: bool) -> Option<SkillEntry> {
    let md_path = dir.join("SKILL.md");
    let meta = std::fs::metadata(&md_path).ok()?;
    if !meta.is_file() || meta.len() > MAX_SKILL_FILE_BYTES {
        return None;
    }
    let content = std::fs::read_to_string(&md_path).ok()?;
    let (name, description) = parse_frontmatter(&content)?;
    let description = description?;
    let name = match name {
        Some(declared) if valid_skill_name(&declared) => declared,
        // Missing or invalid declared name: fall back to the directory
        // name when it is itself a valid skill name (lenient, matching
        // how other Agent Skills readers behave); otherwise the skill
        // has no stable name to address it by — skip it.
        _ => {
            let fallback = dir.file_name()?.to_str()?.to_string();
            if valid_skill_name(&fallback) {
                fallback
            } else {
                return None;
            }
        }
    };
    Some(SkillEntry {
        name,
        description,
        root: dir.to_path_buf(),
        project,
    })
}

/// Extract `(name, description)` from a leading `---` frontmatter block.
/// Lenient line parsing of the YAML subset the Agent Skills standard
/// requires: scalar `key: value` pairs, optional surrounding quotes;
/// unknown fields and the first-frontmatter-only rule (first key wins)
/// keep odd files loadable. `None` = no frontmatter block at all.
fn parse_frontmatter(content: &str) -> Option<(Option<String>, Option<String>)> {
    let mut lines = content.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let (mut name, mut description) = (None, None);
    for line in lines {
        let line = line.trim_end();
        if line.trim() == "---" {
            break; // frontmatter closes
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = unquote(value.trim());
        match key.trim() {
            "name" if name.is_none() => name = Some(value).filter(|v| !v.is_empty()),
            "description" if description.is_none() => {
                description = Some(value).filter(|v| !v.is_empty())
            }
            _ => {}
        }
    }
    Some((name, description))
}

/// Strip one layer of matching surrounding quotes from a scalar value.
fn unquote(value: &str) -> String {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

/// Agent Skills name rules: 1–64 chars, lowercase a-z / digits / hyphens,
/// no leading/trailing/consecutive hyphens.
fn valid_skill_name(name: &str) -> bool {
    const MAX_NAME_LEN: usize = 64;
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
}

/// Validate that a directory is a loadable skill (the server's install
/// path calls this before materializing a copy): `SKILL.md` present,
/// frontmatter parses to a non-empty description, and a usable name
/// (declared or directory-name fallback). Returns the resolved identity
/// with a canonicalized root.
pub fn validate_skill_dir(dir: &Path) -> Result<SkillEntry, String> {
    let real = dir
        .canonicalize()
        .map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    try_skill(&real, false).ok_or_else(|| {
        "not a valid skill directory: it must contain a SKILL.md with frontmatter \
         `name` and a non-empty `description`"
            .to_string()
    })
}

// ── skill_list ──────────────────────────────────────────────────────────────

/// Render the discovery result for the model. The empty case is guidance,
/// not an error — "no skills" is a normal state the model should learn
/// from, not a failure to retry.
fn format_skill_list(entries: &[SkillEntry]) -> String {
    if entries.is_empty() {
        return "No skills installed. A skill is a directory carrying a SKILL.md \
                file (frontmatter `name` + `description`, body = instructions). \
                Project skills live in `.flux/skills/` under the chat workdir; \
                global skills in `~/.flux/skills/`."
            .into();
    }
    let mut out =
        String::from("Available skills — load one's full instructions with skill_read:\n");
    for e in entries {
        let source = if e.project { "project" } else { "global" };
        out.push_str(&format!("\n- {} ({}) — {}", e.name, source, e.description));
    }
    out
}

/// List available skills (name + source + description). The always-cheap
/// discovery surface of progressive disclosure: descriptions ride this
/// result, full instructions load on demand via `skill_read`.
#[derive(Default, flux_macros::Tool, ::serde::Deserialize)]
#[tool(
    name = "skill_list",
    description = "List the available skills — self-contained capability packages (specialized workflows, instructions, helper scripts) that can be loaded on demand. Each entry carries a name, its source (project/global), and a description of when it applies. Call this at the start of a non-trivial task that might match a specialized skill, or before concluding that a capability is missing."
)]
pub struct SkillListTool {}

impl SkillListTool {
    pub fn new() -> Self {
        Self {}
    }

    async fn execute(&self, ctx: ToolCtx) -> Result<String, CoreError> {
        // The project location rides the chat boundary (resolve(".") = the
        // canonical workdir); the global location is the user's home.
        let workdir = ctx.resolve(".")?;
        let entries = discover_skills(
            global_skills_dir().as_deref(),
            Some(&workdir.join(".flux").join("skills")),
        );
        Ok(format_skill_list(&entries))
    }
}

// ── skill_read ──────────────────────────────────────────────────────────────

/// Read one file from a discovered skill, strictly contained in the
/// skill's directory. Name-keyed: unknown names are tool errors the model
/// self-corrects from (re-list via `skill_list`).
fn read_skill_file(
    entries: &[SkillEntry],
    name: &str,
    rel: Option<&str>,
) -> Result<String, CoreError> {
    let entry = entries.iter().find(|e| e.name == name).ok_or_else(|| {
        CoreError::Tool(format!(
            "unknown skill `{name}` — call skill_list for the available names"
        ))
    })?;
    let rel = rel.unwrap_or("SKILL.md");
    if rel.is_empty() {
        return Err(CoreError::Tool("empty path".into()));
    }
    // Containment-scoped read: resolve against the canonical skill root,
    // then verify the target stays inside. `..` components, absolute
    // paths (Path::join replaces), and symlinked escapes all die at the
    // prefix check — the read cannot leave the skill directory.
    let root_real = entry
        .root
        .canonicalize()
        .map_err(|e| CoreError::Tool(format!("skill directory unreadable: {e}")))?;
    let target_real = root_real
        .join(rel)
        .canonicalize()
        .map_err(|e| CoreError::Tool(format!("cannot read `{rel}` in skill `{name}`: {e}")))?;
    if !target_real.starts_with(&root_real) {
        return Err(CoreError::Tool(format!(
            "path `{rel}` escapes the skill directory"
        )));
    }
    if !target_real.is_file() {
        return Err(CoreError::Tool(format!(
            "`{rel}` in skill `{name}` is not a file"
        )));
    }
    let meta = std::fs::metadata(&target_real)
        .map_err(|e| CoreError::Tool(format!("stat failed: {e}")))?;
    if meta.len() > MAX_SKILL_FILE_BYTES {
        return Err(CoreError::Tool(format!(
            "`{rel}` is {} bytes — over the {MAX_SKILL_FILE_BYTES}-byte skill-read cap",
            meta.len()
        )));
    }
    std::fs::read_to_string(&target_real).map_err(|e| CoreError::Tool(format!("read failed: {e}")))
}

/// Read a skill's full instructions (its `SKILL.md`), or one file inside
/// that skill's directory.
#[derive(Default, flux_macros::Tool, ::serde::Deserialize)]
#[tool(
    name = "skill_read",
    description = "Read a skill's full instructions (its SKILL.md), or one file inside that skill's directory (references, scripts, assets — paths the skill text points to). Pass the skill `name` from skill_list; `path` is relative to the skill directory and cannot leave it. Follow the returned instructions for the current task."
)]
pub struct SkillReadTool {
    /// Skill name as listed by skill_list.
    name: String,
    /// File to read, relative to the skill's directory. Defaults to SKILL.md.
    path: Option<String>,
}

impl SkillReadTool {
    pub fn new() -> Self {
        Self::default()
    }

    async fn execute(&self, ctx: ToolCtx) -> Result<String, CoreError> {
        let workdir = ctx.resolve(".")?;
        let entries = discover_skills(
            global_skills_dir().as_deref(),
            Some(&workdir.join(".flux").join("skills")),
        );
        read_skill_file(&entries, &self.name, self.path.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{boundary_ctx, processed, setup};
    use flux_core::Tool;
    use serde_json::json;
    use std::fs;

    fn write_skill(root: &Path, dir_name: &str, name: &str, description: &str) -> PathBuf {
        let dir = root.join(dir_name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n"),
        )
        .unwrap();
        dir
    }

    // ── frontmatter ──

    #[test]
    fn frontmatter_parses_name_and_description() {
        let (name, description) =
            parse_frontmatter("---\nname: pdf\ndescription: PDF tools.\n---\nbody").unwrap();
        assert_eq!(name.as_deref(), Some("pdf"));
        assert_eq!(description.as_deref(), Some("PDF tools."));
    }

    #[test]
    fn frontmatter_is_lenient() {
        // Quoted values, CRLF, unknown fields, trailing spaces.
        let (name, description) = parse_frontmatter(
            "---\r\nlicense: MIT\r\nname: \"quoted name\"\r\ndescription: 'single'  \r\nmeta: x\r\n---\r\nbody",
        )
        .unwrap();
        assert_eq!(name.as_deref(), Some("quoted name"));
        assert_eq!(description.as_deref(), Some("single"));
    }

    #[test]
    fn frontmatter_missing_block_or_description_is_none() {
        assert!(parse_frontmatter("no frontmatter here").is_none());
        // Block present but no description → description None.
        let (_, description) = parse_frontmatter("---\nname: x\n---\nbody").unwrap();
        assert!(description.is_none());
        // Empty description filters to None (try_skill then skips).
        let (_, description) = parse_frontmatter("---\nname: x\ndescription:\n---\n").unwrap();
        assert!(description.is_none());
    }

    #[test]
    fn skill_name_rules() {
        assert!(valid_skill_name("pdf-processing"));
        assert!(valid_skill_name("a"));
        assert!(!valid_skill_name(""));
        assert!(!valid_skill_name("PDF"));
        assert!(!valid_skill_name("-lead"));
        assert!(!valid_skill_name("trail-"));
        assert!(!valid_skill_name("double--hyphen"));
        assert!(!valid_skill_name(&"x".repeat(65)));
    }

    // ── discovery ──

    #[test]
    fn discovery_finds_skills_and_skips_invalid() {
        let dir = setup();
        let skills = dir.path().join("skills");
        write_skill(
            &skills,
            "pdf-tools",
            "pdf-tools",
            "PDF extraction workflows.",
        );
        write_skill(&skills, "no-desc", "no-desc", ""); // empty description — skipped
        // No frontmatter at all — skipped.
        let bare = skills.join("bare");
        fs::create_dir_all(&bare).unwrap();
        fs::write(bare.join("SKILL.md"), "just text").unwrap();

        let found = discover_skills(None, Some(&skills));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "pdf-tools");
        assert!(found[0].project);
        assert!(found[0].description.contains("PDF extraction"));
    }

    #[test]
    fn discovery_name_falls_back_to_directory() {
        let dir = setup();
        let skills = dir.path().join("skills");
        // Declared name invalid → directory name (valid) wins.
        let d = write_skill(&skills, "dir-name", "Invalid Name!", "Does the thing.");
        fs::write(
            d.join("SKILL.md"),
            "---\nname: Invalid Name!\ndescription: Does the thing.\n---\n",
        )
        .unwrap();
        let found = discover_skills(None, Some(&skills));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "dir-name");
    }

    #[test]
    fn project_overrides_global_on_collision() {
        let dir = setup();
        let global = dir.path().join("global");
        let project = dir.path().join("project");
        write_skill(&global, "shared", "shared", "Global version.");
        write_skill(&project, "shared", "shared", "Project version.");
        write_skill(&global, "only-global", "only-global", "Global only.");

        let found = discover_skills(Some(&global), Some(&project));
        assert_eq!(found.len(), 2);
        let shared = found.iter().find(|e| e.name == "shared").unwrap();
        assert_eq!(shared.description, "Project version.");
        assert!(shared.project);
        assert!(found.iter().any(|e| e.name == "only-global" && !e.project));
    }

    #[test]
    fn discovery_is_resilient_to_missing_dirs_and_symlink_escapes() {
        let dir = setup();
        // Absent locations — no error, no entries.
        assert!(discover_skills(Some(&dir.path().join("nope")), None).is_empty());

        #[cfg(unix)]
        {
            // A symlinked skill dir pointing OUTSIDE the skills root is
            // not discovered (the walk never leaves the root).
            let skills = dir.path().join("skills");
            fs::create_dir_all(&skills).unwrap();
            let outside = dir.path().join("outside");
            write_skill(&outside, "escaped", "escaped", "Outside the root.");
            std::os::unix::fs::symlink(outside.join("escaped"), skills.join("escaped")).unwrap();
            assert!(discover_skills(None, Some(&skills)).is_empty());
        }
    }

    // ── formatting ──

    #[test]
    fn empty_list_is_guidance_not_error() {
        let out = format_skill_list(&[]);
        assert!(out.contains("No skills installed"));
        assert!(out.contains(".flux/skills"));
    }

    #[test]
    fn list_carries_name_source_description() {
        let entries = vec![SkillEntry {
            name: "pdf-tools".into(),
            description: "PDF workflows.".into(),
            root: PathBuf::from("/nowhere"),
            project: true,
        }];
        let out = format_skill_list(&entries);
        assert!(out.contains("- pdf-tools (project) — PDF workflows."));
        assert!(out.contains("skill_read"));
    }

    // ── skill_read core ──

    #[test]
    fn read_defaults_to_skill_md_and_supports_relative_paths() {
        let dir = setup();
        let skill_dir = write_skill(dir.path(), "pdf", "pdf", "PDF workflows.");
        let entries = discover_skills(None, Some(dir.path()));

        let whole = read_skill_file(&entries, "pdf", None).unwrap();
        assert!(whole.contains("name: pdf"));
        // The path form reaches files the skill text points to.
        fs::create_dir_all(skill_dir.join("references")).unwrap();
        fs::write(skill_dir.join("references/api.md"), "API reference body").unwrap();
        let reference = read_skill_file(&entries, "pdf", Some("references/api.md")).unwrap();
        assert_eq!(reference, "API reference body");
    }

    #[test]
    fn read_rejects_unknown_skills_and_escapes() {
        let dir = setup();
        let skill_dir = write_skill(dir.path(), "pdf", "pdf", "PDF workflows.");
        fs::write(skill_dir.join("secret.txt"), "inside").unwrap();
        fs::write(dir.path().join("outside.txt"), "outside").unwrap();
        let entries = discover_skills(None, Some(dir.path()));

        // Unknown name — a self-correcting tool error.
        let err = read_skill_file(&entries, "nope", None).unwrap_err();
        assert!(err.to_string().contains("unknown skill"));

        // `..` traversal, absolute paths, and absolute replacements all
        // die at the containment check.
        for escape in ["../outside.txt", "/etc/hostname", ".."] {
            let err = read_skill_file(&entries, "pdf", Some(escape)).unwrap_err();
            assert!(
                err.to_string().contains("escapes") || err.to_string().contains("cannot read"),
                "escape `{escape}` not rejected: {err}"
            );
        }
    }

    // ── tool surfaces (through the registry contract) ──

    #[tokio::test]
    async fn skill_list_tool_lists_project_skills() {
        let dir = setup();
        write_skill(
            &dir.path().join(".flux/skills"),
            "pdf",
            "pdf",
            "PDF workflows.",
        );
        let tool = SkillListTool::new();
        let out = tool
            .call(processed(vec![]), boundary_ctx(dir.path()))
            .await
            .unwrap();
        assert!(out.contains("- pdf (project) — PDF workflows."));
    }

    #[tokio::test]
    async fn skill_read_tool_reads_within_the_boundary_contract() {
        let dir = setup();
        let skill_dir = write_skill(
            &dir.path().join(".flux/skills"),
            "pdf",
            "pdf",
            "PDF workflows.",
        );
        fs::write(skill_dir.join("refs.md"), "reference body").unwrap();

        let tool = SkillReadTool::new();
        let whole = tool
            .call(
                processed(vec![("name", json!("pdf"))]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(whole.contains("name: pdf"));

        let reference = tool
            .call(
                processed(vec![("name", json!("pdf")), ("path", json!("refs.md"))]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap();
        assert_eq!(reference, "reference body");

        // Escape attempt — a tool error the model sees.
        let err = tool
            .call(
                processed(vec![("name", json!("pdf")), ("path", json!("../../x"))]),
                boundary_ctx(dir.path()),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("escapes") || err.to_string().contains("cannot read"));
    }

    #[tokio::test]
    async fn tools_fail_closed_without_a_boundary() {
        // Empty workdir = no boundary — resolve(".") errors out (the
        // fail-closed default), never a silent fallback to the server cwd.
        let tool = SkillListTool::new();
        let err = tool
            .call(processed(vec![]), flux_core::ToolCtx::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("workdir"));
    }
}
