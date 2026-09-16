//! Skill tools — lazy-loading access to Agent Skills packages.
//!
//! A skill is a self-contained capability package: a directory carrying a
//! `SKILL.md` (frontmatter `name` + `description`, body = instructions)
//! plus optional references/scripts/assets. This module owns the skill
//! FORMAT and its three consumption surfaces:
//!
//! - discovery + listing (`skill_list`, the always-cheap tool surface);
//! - the Tier-1 catalog for the system prompt (`catalog_for_workdir` —
//!   composed by flux-chat at every connection begin; the tools' own
//!   descriptions remain the no-catalog fallback);
//! - activation reads (`read_skill_file` → `format_skill_activation`):
//!   the manifest returns as `<skill_content>` wrapping the
//!   FRONTMATTER-STRIPPED body (metadata was already consumed at
//!   discovery), other files return raw with a bundled-file listing on
//!   manifest reads. The same normalization (`skill_content`) is what
//!   flux-chat hashes for activation dedup — one function, one meaning
//!   of "content".
//!
//! Locations (both follow the same `<root>/.flux/skills` layout):
//! - project: `<workdir>/.flux/skills/` — inside the chat boundary
//! - global: `$HOME/.flux/skills/` (`USERPROFILE` fallback) —
//!   user-installed content, OUTSIDE the workdir boundary by design (the
//!   same trust tier as MCP servers launched from the DB: the user opted
//!   in by installing it)
//!
//! The boundary exception is structural, not a hole: `skill_read` is
//! NAME-keyed — the model never passes a path to the skill root. The
//! requested file resolves relative to the discovered skill's directory
//! under a strict containment check (canonicalize + prefix, symlink-
//! safe), so a read cannot leave the skill directory regardless of the
//! arguments.

use flux_core::{CoreError, ToolCtx};
use std::path::{Path, PathBuf};

/// Cap for one skill file read (scan and read share it) — SKILL.md files
/// are instructions, not data dumps; anything larger is not a skill file.
const MAX_SKILL_FILE_BYTES: u64 = 256 * 1024;
/// Depth cap for the discovery walk below a skills root — skill packages
/// are shallow; a deep tree is content, not a skill location.
const MAX_SCAN_DEPTH: usize = 8;

/// The skill manifest's path relative to the skill directory.
const MANIFEST: &str = "SKILL.md";

/// Cap on the per-skill description carried into the Tier-1 catalog
/// (the spec's own `description` ceiling — the catalog never carries
/// more than the format allows).
const MAX_CATALOG_DESC_CHARS: usize = 1024;

/// Total char budget for the catalog section appended to the system
/// prompt. Fixed (not a window fraction): flux does not track per-model
/// window sizes, and a fixed cap is honest about the worst case. Codex
/// uses the same number as its "window unknown" floor.
const CATALOG_MAX_CHARS: usize = 8000;

/// Cap on the bundled-file listing attached to a manifest activation —
/// a large tree is listed truncated with an explicit note, never
/// silently.
const MAX_RESOURCE_ENTRIES: usize = 20;

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

/// Discover skills for one chat workdir: global + project locations (the
/// shared location composition behind `skill_list`, the activation
/// reads, and the catalog — one place, so all three surfaces always see
/// the same set).
pub fn discover_for_workdir(workdir: &Path) -> Vec<SkillEntry> {
    discover_skills(
        global_skills_dir().as_deref(),
        Some(&workdir.join(".flux").join("skills")),
    )
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

/// Strip a leading frontmatter block: the body after the closing `---`
/// when the file opens with `---` (the same first-line rule
/// [`parse_frontmatter`] applies — one source for "what counts as
/// frontmatter"), else the content unchanged. Unterminated blocks return
/// the content unchanged — nothing safely strippable.
pub fn strip_frontmatter(content: &str) -> &str {
    let Some(first_end) = content.find('\n') else {
        return content;
    };
    if content[..first_end].trim_end() != "---" {
        return content;
    }
    let mut start = first_end + 1;
    while let Some(end) = content[start..].find('\n') {
        let line_end = start + end;
        if content[start..line_end].trim_end() == "---" {
            // Body starts after the closing line, without its leading
            // blank line — the metadata block is discovery-only.
            return content[line_end + 1..].trim_start();
        }
        start = line_end + 1;
    }
    content
}

/// Lexically normalize a `skill_read` `path` argument to the per-skill
/// key form: missing/empty → the manifest (`SKILL.md`), `.` components
/// dropped, `name/..` pairs cancelled. THE shared key derivation for
/// activation dedup — the live tool and the transcript-derived index
/// (flux-chat) must key identically. Note this is deliberately lexical,
/// not canonical: a path that fails containment never reaches a key, and
/// an in-skill symlink alias costs at worst a duplicate full read.
pub fn normalize_rel(rel: Option<&str>) -> String {
    use std::path::Component;
    let raw = rel.map(str::trim).unwrap_or("");
    if raw.is_empty() {
        return MANIFEST.to_string();
    }
    let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
    for comp in Path::new(raw).components() {
        match comp {
            Component::Normal(p) => parts.push(p),
            Component::ParentDir => {
                parts.pop();
            }
            _ => {} // CurDir dropped; RootDir/Prefix cannot survive containment
        }
    }
    if parts.is_empty() {
        return MANIFEST.to_string();
    }
    parts
        .iter()
        .collect::<PathBuf>()
        .to_string_lossy()
        .into_owned()
}

/// Char-boundary truncation for catalog descriptions.
fn truncate_chars(s: &str, max: usize) -> &str {
    if s.chars().count() <= max {
        return s;
    }
    let end = s.char_indices().nth(max).map_or(s.len(), |(i, _)| i);
    &s[..end]
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

/// List available skills (name + source + description). The live-scan
/// complement to the snapshot catalog: the catalog rides the system
/// prompt (a begin-time snapshot, budget-truncated), this rescans on
/// every call — the freshness surface for mid-session installs and the
/// self-correction path after an unknown-name rejection.
#[derive(Default, flux_macros::Tool, ::serde::Deserialize)]
#[tool(
    name = "skill_list",
    description = "List the available skills (name, source, description) — a live rescan, unlike the snapshot listing in your instructions. Call it when a skill you expect is missing from that listing, when a skill_read name was rejected, or when you suspect skills changed since the conversation started."
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
        Ok(format_skill_list(&discover_for_workdir(&workdir)))
    }
}

// ── catalog (Tier-1, system-prompt injection) ───────────────────────────────

/// The Tier-1 catalog section for the system prompt: every skill's name +
/// source + description under one instruction block. `None` when no
/// skills are discovered — an empty catalog block would mislead the
/// model, so the section simply never appears. A snapshot by design: the
/// text says so and points at `skill_list` for the live rescan.
pub fn catalog_section(global_dir: Option<&Path>, project_dir: Option<&Path>) -> Option<String> {
    const HEADER: &str = "## Skills\n\n\
        Specialized instruction packages are available. When a task matches a skill's \
        description, call skill_read with its name to load the full instructions before \
        proceeding. This listing is a snapshot taken when the conversation started — call \
        skill_list for the live list.\n\n<available_skills>\n";
    let entries = discover_skills(global_dir, project_dir);
    if entries.is_empty() {
        return None;
    }
    // Fill entries in the stable discovery order until the budget; the
    // remainder collapses into one explicit tail note (never a silent
    // truncation).
    let mut out = String::from(HEADER);
    let mut shown = 0usize;
    for e in &entries {
        let source = if e.project { "project" } else { "global" };
        let line = format!(
            "- {} ({}): {}\n",
            e.name,
            source,
            truncate_chars(&e.description, MAX_CATALOG_DESC_CHARS)
        );
        if out.len() + line.len() > CATALOG_MAX_CHARS {
            break;
        }
        out.push_str(&line);
        shown += 1;
    }
    if shown < entries.len() {
        out.push_str(&format!(
            "… {} more — skill_list has the full list.\n",
            entries.len() - shown
        ));
    }
    out.push_str("</available_skills>");
    Some(out)
}

/// [`catalog_section`] resolved for one chat workdir (the same global +
/// project composition discovery uses). An empty workdir (no boundary)
/// yields no section — the project location would otherwise resolve
/// against the server process's cwd.
pub fn catalog_for_workdir(workdir: &Path) -> Option<String> {
    if workdir.as_os_str().is_empty() {
        return None;
    }
    catalog_section(
        global_skills_dir().as_deref(),
        Some(&workdir.join(".flux").join("skills")),
    )
}

// ── skill_read ──────────────────────────────────────────────────────────────

/// One successfully resolved skill file: everything the caller (the
/// chat-owned activation tool) needs — identity for the dedup key, the
/// manifest flag that decides normalization, and the raw bytes.
#[derive(Debug)]
pub struct SkillFile {
    /// Skill name as listed by skill_list (the key's first half).
    pub name: String,
    /// Canonical path of the file inside the skill directory.
    pub path: PathBuf,
    /// `true` when the requested path IS the skill manifest — the only
    /// file whose frontmatter was consumed at discovery, and thus the
    /// only one whose frontmatter is stripped on return.
    pub is_manifest: bool,
    /// Raw file content.
    pub raw: String,
}

/// The normalized content of a skill file — the ONE definition of
/// "content" shared by the tool's return, the transcript, and the
/// activation-dedup hash: the manifest returns as its frontmatter-
/// stripped body (metadata was consumed at discovery), every other file
/// returns raw.
pub fn skill_content(file: &SkillFile) -> &str {
    if file.is_manifest {
        strip_frontmatter(&file.raw)
    } else {
        &file.raw
    }
}

/// Read one file from a discovered skill, strictly contained in the
/// skill's directory. Name-keyed: unknown names are tool errors the
/// model self-corrects from (re-list via `skill_list`).
pub fn read_skill_file(
    entries: &[SkillEntry],
    name: &str,
    rel: Option<&str>,
) -> Result<SkillFile, CoreError> {
    let entry = entries.iter().find(|e| e.name == name).ok_or_else(|| {
        CoreError::Tool(format!(
            "unknown skill `{name}` — call skill_list for the available names"
        ))
    })?;
    let shown = rel.unwrap_or(MANIFEST);
    if shown.trim().is_empty() {
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
        .join(shown)
        .canonicalize()
        .map_err(|e| CoreError::Tool(format!("cannot read `{shown}` in skill `{name}`: {e}")))?;
    if !target_real.starts_with(&root_real) {
        return Err(CoreError::Tool(format!(
            "path `{shown}` escapes the skill directory"
        )));
    }
    if !target_real.is_file() {
        return Err(CoreError::Tool(format!(
            "`{shown}` in skill `{name}` is not a file"
        )));
    }
    let meta = std::fs::metadata(&target_real)
        .map_err(|e| CoreError::Tool(format!("stat failed: {e}")))?;
    if meta.len() > MAX_SKILL_FILE_BYTES {
        return Err(CoreError::Tool(format!(
            "`{shown}` is {} bytes — over the {MAX_SKILL_FILE_BYTES}-byte skill-read cap",
            meta.len()
        )));
    }
    let raw = std::fs::read_to_string(&target_real)
        .map_err(|e| CoreError::Tool(format!("read failed: {e}")))?;
    // The manifest is identified by the REQUESTED path (what the model
    // activated), not by the canonical target — a symlinked `SKILL.md`
    // is still the manifest discovery parsed, so stripping stays
    // consistent with the frontmatter that was consumed.
    let is_manifest = normalize_rel(rel) == MANIFEST;
    Ok(SkillFile {
        name: name.to_string(),
        path: target_real,
        is_manifest,
        raw,
    })
}

/// Bundled files of a skill directory (relative paths), for the
/// manifest-activation listing: shallow, symlink-safe walk (same posture
/// as [`scan_skills_dir`]), sorted, capped — a large tree is listed with
/// an explicit incompleteness note by the formatter.
pub fn skill_resources(root: &Path) -> Vec<String> {
    const MAX_RESOURCE_DEPTH: usize = 3;
    let Ok(root_real) = root.canonicalize() else {
        return Vec::new();
    };
    let mut files: Vec<String> = Vec::new();
    let mut complete = true;
    let mut stack: Vec<(PathBuf, usize)> = vec![(root_real.clone(), 0)];
    'walk: while let Some((dir, depth)) = stack.pop() {
        let Ok(children) = std::fs::read_dir(&dir) else {
            continue; // unreadable — skip (same posture as walk_dir)
        };
        for child in children.filter_map(|e| e.ok()) {
            let Ok(real) = child.path().canonicalize() else {
                continue; // dangling symlink — skip
            };
            if !real.starts_with(&root_real) {
                continue; // symlink escape — never leave the skill directory
            }
            if real.is_dir() {
                if depth < MAX_RESOURCE_DEPTH {
                    stack.push((real, depth + 1));
                }
                continue;
            }
            if real == root_real.join(MANIFEST) {
                continue; // the manifest itself is not a "bundled" resource
            }
            if files.len() >= MAX_RESOURCE_ENTRIES {
                complete = false;
                break 'walk;
            }
            if let Ok(rel) = real.strip_prefix(&root_real) {
                files.push(rel.to_string_lossy().into_owned());
            }
        }
    }
    files.sort();
    if !complete {
        files.push(format!("… list capped at {MAX_RESOURCE_ENTRIES} files"));
    }
    files
}

/// Format one skill activation — the model-facing return of a full read:
/// the manifest arrives as `<skill_content>` around the normalized body
/// (structured wrapping — the model can tell skill instructions from
/// conversation content, and a future compactor can recognize the block)
/// plus the bundled-file listing (paths only — never eagerly loaded);
/// a non-manifest file is returned raw.
pub fn format_skill_activation(file: &SkillFile) -> String {
    let body = skill_content(file);
    if !file.is_manifest {
        return body.to_string();
    }
    let mut out = format!(
        "<skill_content name=\"{}\">\n{body}\n</skill_content>",
        file.name
    );
    // The skill root is the manifest's parent (a canonical path).
    let resources = skill_resources(file.path.parent().unwrap_or(Path::new("/")));
    if !resources.is_empty() {
        out.push_str(
            "\n\nBundled files (relative to the skill root — read via skill_read `path`): ",
        );
        out.push_str(&resources.join(", "));
    }
    out
}

#[cfg(test)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{boundary_ctx, processed, setup};
    use flux_core::Tool;
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
        assert_eq!(whole.name, "pdf");
        assert!(whole.is_manifest);
        // The manifest's normalized content is the stripped body — the
        // frontmatter was already consumed at discovery.
        let body = skill_content(&whole);
        assert_eq!(body, "# pdf\n".to_string());
        assert!(!body.contains("name: pdf"));
        // The path form reaches files the skill text points to, raw.
        fs::create_dir_all(skill_dir.join("references")).unwrap();
        fs::write(skill_dir.join("references/api.md"), "API reference body").unwrap();
        let reference = read_skill_file(&entries, "pdf", Some("references/api.md")).unwrap();
        assert!(!reference.is_manifest);
        assert_eq!(skill_content(&reference), "API reference body");
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

    // ── normalization + formatting ──

    #[test]
    fn strip_removes_only_a_leading_frontmatter_block() {
        // Plain strip: body after the closing `---`, leading blank line gone.
        assert_eq!(
            strip_frontmatter("---\nname: x\n---\n\n# Body\n"),
            "# Body\n"
        );
        // No frontmatter — unchanged (including files that merely CONTAIN `---`).
        assert_eq!(
            strip_frontmatter("no frontmatter\n---\nstill body"),
            "no frontmatter\n---\nstill body"
        );
        // Unterminated block — nothing safely strippable.
        assert_eq!(strip_frontmatter("---\nname: x\n"), "---\nname: x\n");
        // CRLF frontmatter.
        assert_eq!(
            strip_frontmatter("---\r\nname: x\r\n---\r\nBody\r\n"),
            "Body\r\n"
        );
    }

    #[test]
    fn normalize_rel_converges_the_manifest_spellings() {
        assert_eq!(normalize_rel(None), "SKILL.md");
        assert_eq!(normalize_rel(Some("")), "SKILL.md");
        assert_eq!(normalize_rel(Some("SKILL.md")), "SKILL.md");
        assert_eq!(normalize_rel(Some("./SKILL.md")), "SKILL.md");
        assert_eq!(normalize_rel(Some("sub/../SKILL.md")), "SKILL.md");
        assert_eq!(
            normalize_rel(Some("references/api.md")),
            "references/api.md"
        );
    }

    #[test]
    fn manifest_activation_wraps_the_body_and_lists_resources() {
        let dir = setup();
        let skill_dir = write_skill(dir.path(), "pdf", "pdf", "PDF workflows.");
        fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        fs::write(skill_dir.join("scripts/run.py"), "#!/usr/bin/env python3").unwrap();
        let entries = discover_skills(None, Some(dir.path()));
        let file = read_skill_file(&entries, "pdf", None).unwrap();
        let out = format_skill_activation(&file);
        assert!(out.starts_with("<skill_content name=\"pdf\">\n"));
        assert!(out.contains("\n</skill_content>"));
        assert!(!out.contains("name: pdf")); // frontmatter stripped
        assert!(out.contains("scripts/run.py")); // bundled listing, path only
        assert!(out.contains("read via skill_read"));
        // The absolute skill root is deliberately NOT surfaced (global
        // skills live outside the workdir boundary).
        assert!(!out.contains(dir.path().to_str().unwrap()));
    }

    #[test]
    fn non_manifest_reads_return_raw_without_wrapping() {
        let dir = setup();
        let skill_dir = write_skill(dir.path(), "pdf", "pdf", "PDF workflows.");
        fs::write(skill_dir.join("refs.md"), "reference body").unwrap();
        let entries = discover_skills(None, Some(dir.path()));
        let file = read_skill_file(&entries, "pdf", Some("refs.md")).unwrap();
        assert_eq!(format_skill_activation(&file), "reference body");
    }

    // ── catalog (Tier-1) ──

    #[test]
    fn catalog_is_none_without_skills_and_carries_entries_otherwise() {
        let dir = setup();
        // No skills — the section never appears (empty blocks mislead).
        assert!(catalog_section(None, Some(&dir.path().join("skills"))).is_none());

        write_skill(&dir.path().join("skills"), "pdf", "pdf", "PDF workflows.");
        let section = catalog_section(None, Some(&dir.path().join("skills"))).unwrap();
        assert!(section.starts_with("## Skills"));
        assert!(section.contains("snapshot"));
        assert!(section.contains("skill_list for the live list"));
        assert!(section.contains("- pdf (project): PDF workflows."));
        assert!(section.trim_end().ends_with("</available_skills>"));
    }

    #[test]
    fn catalog_respects_its_budget_with_an_explicit_tail_note() {
        let dir = setup();
        let skills = dir.path().join("skills");
        for i in 0..40 {
            write_skill(
                &skills,
                &format!("s{i}"),
                &format!("s{i}"),
                &"x".repeat(300),
            );
        }
        let section = catalog_section(None, Some(&skills)).unwrap();
        assert!(section.len() <= 8000 + 200); // budget + tail-note slack
        assert!(section.contains("more — skill_list has the full list"));
        assert!(section.contains("- s0 (project):"));
    }

    #[test]
    fn catalog_for_workdir_needs_a_boundary() {
        assert!(catalog_for_workdir(Path::new("")).is_none());
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
