//! skills.rs — the chat-owned `skill_read`, the Tier-1 catalog
//! composition, and the per-chat workdir line. The first two share one
//! definition of "skill content"; all three meet at one begin-point
//! composition:
//!
//! - **Workdir line**: [`compose_system_prompt`] states the boundary's
//!   VALUE in the system prompt — the model's only always-visible source
//!   for the path (the state schema deliberately does NOT advertise it;
//!   see domain.rs). Without it the model's first absolute-path need
//!   costs a `state_get` round-trip.
//! - **Catalog (Tier-1)**: [`compose_system_prompt`] appends the skill
//!   catalog to the agent preamble at every connection begin (spawn and
//!   in-place rebuild). A begin-time snapshot — `skill_list` stays the
//!   live rescan for mid-session changes.
//! - **Activation dedup**: a committed `skill_read` result IS in the
//!   model's context (the transcript only grows), so re-reading the same
//!   content re-injects a duplicate. [`SkillActivationIndex`] is a
//!   read-side derivation from the transcript — no new persistence: the
//!   transcript is the only truth, and fork/rebuild/restart inherit the
//!   index by re-deriving at every assembly point (the same philosophy
//!   as `history::validate_history`).
//!
//! Content identity is [`flux_tools::skill_content`]: the
//! manifest counts as its frontmatter-stripped body, every other file as
//! raw bytes. The live tool and the derivation both hash that — editing
//! frontmatter alone does not invalidate an activation (the body the
//! model follows is unchanged); editing the body does.

use crate::buf::{BUF_PAGE_CHARS, INLINE_BUDGET};
use async_trait::async_trait;
use flux_core::{CoreError, Message, Role, SKILL_READ_TOOL, Tool, ToolCtx};
use flux_store::Store;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};

// ── Tier-1 catalog composition ──────────────────────────────────────────────

/// The per-chat boundary line: the workdir VALUE's only always-visible
/// surface in the model's context. The state schema deliberately does not
/// advertise the key (domain.rs) — without this line the model's first
/// absolute-path need costs a `state_get` round-trip, and before it
/// existed there was no source for the path at all.
fn workdir_section(workdir: &str) -> String {
    format!(
        "Working directory: {workdir} — the chat's sandbox boundary, fixed for \
         this conversation. Relative paths and path arguments resolve against \
         it; file, search, and shell tools default to it."
    )
}

/// Compose the connection's system prompt: the agent preamble, plus the
/// per-chat workdir line, plus the skill catalog (a begin-time snapshot —
/// the instruction text inside the section points at `skill_list` for the
/// live list). No workdir boundary leaves the preamble unchanged — a
/// boundary-less chat has no path to state and an empty line would
/// mislead.
pub(crate) fn compose_system_prompt(base: &str, workdir: &str) -> String {
    if workdir.is_empty() {
        return base.to_string();
    }
    // Stable-before-volatile ordering: the boundary never moves for a
    // chat's lifetime while the catalog refreshes at every rebuild gate,
    // so the workdir line precedes the catalog and a catalog change
    // keeps the longest shared prompt prefix.
    let base = format!("{base}\n\n{}", workdir_section(workdir));
    compose_with_catalog(&base, flux_tools::catalog_for_workdir(Path::new(workdir)))
}

/// The pure composition the glue above feeds: catalog appended after the
/// base (preamble + workdir line — prompt-prefix caches keep the preamble
/// byte-stable per connection), absent catalog → the base verbatim.
pub(crate) fn compose_with_catalog(base: &str, catalog: Option<String>) -> String {
    match catalog {
        Some(section) => format!("{base}\n\n{section}"),
        None => base.to_string(),
    }
}

// ── activation index (read-side derivation) ─────────────────────────────────

/// One activation record: what the model already received, and where.
#[derive(Debug, Clone)]
struct Activation {
    /// Hash of the content the transcript carries for this key.
    hash: u64,
    /// The tool call whose result carried it (the note's pointer, and the
    /// buf_read ref when the result overflowed).
    call_id: String,
    /// The transcript result was truncated — the full content lives in
    /// buf_entries, so the note must hand the model the paging ref.
    overflowed: bool,
}

/// Key: (skill name, lexically normalized path — `flux_tools::skills::
/// normalize_rel`, the SAME normalization the live tool applies). Lexical
/// on purpose: a derivation re-canonicalizing paths would re-resolve
/// skills that may have moved since the call committed, for a benefit
/// only in-skill symlink aliases could ever show (an extra duplicate read
/// at worst).
type Key = (String, String);

/// The per-chat set of skill contents already in the model's context.
/// Derived at the two assembly points (spawn, apply_rebuild) and held by
/// the chat-owned [`SkillReadTool`]; updates from in-round reads stay in
/// memory — a rebuild re-derives from the round-atomically-committed
/// transcript, which by then includes them.
#[derive(Debug, Default)]
pub(crate) struct SkillActivationIndex {
    entries: StdMutex<HashMap<Key, Activation>>,
}

impl SkillActivationIndex {
    fn get(&self, key: &Key) -> Option<Activation> {
        self.entries.lock().ok()?.get(key).cloned()
    }

    fn record(&self, key: Key, activation: Activation) {
        // A lock-poisoned index degrades to "no dedup" — a duplicate full
        // read, never a wrong answer.
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(key, activation);
        }
    }
}

/// Content hash for change detection: length + SipHash. Local files, one
/// chat, non-adversarial — a stable-in-practice hash beats pulling a
/// digest crate into the dependency tree.
fn content_hash(content: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.len().hash(&mut hasher);
    content.hash(&mut hasher);
    hasher.finish()
}

/// Derive the activation index from the transcript. Only committed skill
/// reads count — at the assembly points the machine gate guarantees the
/// history is round-atomic, so the derivation matches exactly what the
/// model's context carries.
pub(crate) async fn derive_activation_index(
    store: &Store,
    chat_id: &str,
    history: &[Message],
) -> Arc<SkillActivationIndex> {
    let index = Arc::new(SkillActivationIndex::default());
    // Result lookup by call id — a skill_read without its result (should
    // be structurally impossible; validate_history guards the invariant)
    // is simply not deduplicated.
    let results: HashMap<&str, &str> = history
        .iter()
        .filter(|m| m.role == Role::Tool)
        .filter_map(|m| m.tool_call_id.as_deref().map(|id| (id, m.content.as_str())))
        .collect();
    for message in history {
        if message.role != Role::Assistant {
            continue;
        }
        for call in &message.tool_calls {
            if call.name != SKILL_READ_TOOL {
                continue;
            }
            // Unparseable arguments (only possible for foreign rows) —
            // skip; the worst case is a duplicate full read.
            let Ok(args) = serde_json::from_str::<Value>(&call.arguments) else {
                continue;
            };
            let Some(name) = args.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let rel = args.get("path").and_then(|v| v.as_str());
            let Some(result) = results.get(call.id.as_str()) else {
                continue;
            };
            // The transcript stores the tool's return verbatim within the
            // inline budget; above it, head + ref — the full content only
            // in buf_entries. Result length > budget ⟺ overflowed (the
            // bounded head alone is budget-sized).
            let (content, overflowed) = if result.chars().count() > INLINE_BUDGET {
                match store.load_buf_entry(chat_id, &call.id).await {
                    Ok(Some(full)) => (full, true),
                    // Missing entry (fork keep-set violation should never
                    // happen): no dedup for this call.
                    _ => continue,
                }
            } else {
                (result.to_string(), false)
            };
            index.record(
                (name.to_string(), flux_tools::normalize_rel(rel)),
                Activation {
                    hash: content_hash(&content),
                    call_id: call.id.clone(),
                    overflowed,
                },
            );
        }
    }
    index
}

// ── the chat-owned skill_read tool ──────────────────────────────────────────

/// `skill_read` — the activation surface. Chat-owned (like `buf_read`)
/// because it carries the chat's activation index: a hit returns a short
/// note instead of re-injecting unchanged content; a miss returns the
/// full formatted content and records itself. Successes only are recorded
/// — error texts stay out of the index (repeated errors are cheap; a
/// poisoned key would be worse). Reading goes through flux-tools' shared
/// discovery + containment + normalization.
#[derive(Debug, Clone)]
pub struct SkillReadTool {
    index: Arc<SkillActivationIndex>,
}

impl SkillReadTool {
    pub(crate) fn new(index: Arc<SkillActivationIndex>) -> Self {
        Self { index }
    }
}

#[async_trait]
impl Tool for SkillReadTool {
    fn name(&self) -> &str {
        SKILL_READ_TOOL
    }

    fn description(&self) -> &str {
        "Read a skill's full instructions (its SKILL.md — returned as the frontmatter-stripped \
         body wrapped in <skill_content>, with the skill's bundled files listed), or one file \
         inside that skill's directory (references, scripts, assets — paths the skill text points \
         to, returned as-is). Pass the skill `name` from skill_list; `path` is relative to the \
         skill directory and cannot leave it. Follow the returned instructions for the current \
         task; re-reading unchanged content returns a short note instead of the content."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Skill name as listed by skill_list." },
                "path": { "type": "string", "description": "File to read, relative to the skill's directory. Defaults to SKILL.md." },
            },
            "required": ["name"],
            "additionalProperties": false
        })
    }

    async fn call(&self, args: HashMap<String, Value>, ctx: ToolCtx) -> Result<String, CoreError> {
        let Some(name) = args.get("name").and_then(|v| v.as_str()) else {
            return Err(CoreError::InvalidArguments(
                "skill_read requires a 'name' argument".into(),
            ));
        };
        let rel = args.get("path").and_then(|v| v.as_str());
        let key = (name.to_string(), flux_tools::normalize_rel(rel));

        // Read through the shared discovery + containment + normalization
        // (the same set skill_list sees — new installs appear without any
        // restart semantics).
        let workdir = ctx.resolve(".")?;
        let file =
            flux_tools::read_skill_file(&flux_tools::discover_for_workdir(&workdir), name, rel)?;
        let full = flux_tools::format_skill_activation(&file);

        // Hit: the unchanged content is already in context — a short note
        // replaces the duplicate. The comparison is against what the
        // transcript actually carries (the full formatted return), so a
        // bundled-file change also misses (and re-injects) correctly.
        if let Some(previous) = self.index.get(&key)
            && previous.hash == content_hash(&full)
        {
            let mut note = format!(
                "result unchanged since your earlier read of this skill file (call \"{}\") — already in context.",
                previous.call_id
            );
            if previous.overflowed {
                note.push_str(&format!(
                    "\nfull content: buf_read {{\"ref\": \"{}\", \"offset\": {INLINE_BUDGET}, \"limit\": {BUF_PAGE_CHARS}}}",
                    previous.call_id
                ));
            }
            return Ok(note);
        }

        // Miss: the full content, recorded for the next call. The
        // overflow prediction mirrors `Chat::bounded_output`'s exact rule
        // (char count above the inline budget) — this tool's return is
        // that function's input, so the flag is always right. A
        // concurrent flight with the same key may record twice — benign
        // (idempotent insert, one duplicated full read at worst).
        self.index.record(
            key,
            Activation {
                hash: content_hash(&full),
                call_id: ctx.call_id.clone(),
                overflowed: full.chars().count() > INLINE_BUDGET,
            },
        );
        Ok(full)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::ToolCall;
    use std::fs;
    use std::path::PathBuf;

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

    fn tool_ctx(workdir: &Path, call_id: &str) -> ToolCtx {
        let mut ctx = ToolCtx::new();
        ctx.workdir = workdir.to_path_buf();
        ctx.call_id = call_id.to_string();
        ctx
    }

    fn args(name: &str, path: Option<&str>) -> HashMap<String, Value> {
        let mut map = HashMap::new();
        map.insert("name".to_string(), json!(name));
        if let Some(p) = path {
            map.insert("path".to_string(), json!(p));
        }
        map
    }

    fn assistant_call(call_id: &str, name: &str, path: Option<&str>) -> Message {
        let mut args_json = json!({ "name": name });
        if let Some(p) = path {
            args_json["path"] = json!(p);
        }
        Message {
            role: Role::Assistant,
            content: String::new(),
            reasoning_content: None,
            tool_calls: vec![ToolCall {
                id: call_id.to_string(),
                name: SKILL_READ_TOOL.to_string(),
                arguments: args_json.to_string(),
            }],
            tool_call_id: None,
        }
    }

    fn tool_result(call_id: &str, content: &str) -> Message {
        Message {
            role: Role::Tool,
            content: content.to_string(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.to_string()),
        }
    }

    // ── catalog composition ──

    #[test]
    fn compose_without_a_boundary_is_the_bare_preamble() {
        // Empty workdir = no boundary — never a server-cwd scan.
        assert_eq!(compose_system_prompt("You are flux.", ""), "You are flux.");
    }

    #[test]
    fn compose_with_catalog_appends_after_the_preamble_or_leaves_it_bare() {
        // No catalog (no skills) → preamble byte-identical, no empty block.
        assert_eq!(compose_with_catalog("You are flux.", None), "You are flux.");
        // A catalog rides AFTER the preamble — prompt-prefix caches keep
        // the preamble byte-stable across catalog changes.
        assert_eq!(
            compose_with_catalog("You are flux.", Some("## Skills\n...".into())),
            "You are flux.\n\n## Skills\n..."
        );
    }

    #[test]
    fn compose_appends_the_live_catalog_for_a_workdir() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(
            &dir.path().join(".flux/skills"),
            "pdf",
            "pdf",
            "PDF workflows.",
        );
        let composed = compose_system_prompt("You are flux.", dir.path().to_str().unwrap());
        assert!(composed.starts_with("You are flux.\n\n"), "{composed}");
        assert!(composed.contains("- pdf (project): PDF workflows."));
    }

    #[test]
    fn compose_states_the_boundary_value_before_the_catalog() {
        // The workdir VALUE rides the prompt — the model's only
        // always-visible source for the path (the state enum no longer
        // advertises the key). Stable-before-volatile: the boundary line
        // precedes the catalog, which refreshes at rebuild gates.
        let dir = tempfile::tempdir().unwrap();
        write_skill(
            &dir.path().join(".flux/skills"),
            "pdf",
            "pdf",
            "PDF workflows.",
        );
        let workdir = dir.path().to_str().unwrap();
        let composed = compose_system_prompt("You are flux.", workdir);
        let line = format!("Working directory: {workdir}");
        let wd = composed
            .find(&line)
            .unwrap_or_else(|| panic!("workdir line absent: {composed}"));
        assert!(composed[wd..].contains("sandbox boundary"), "{composed}");
        let cat = composed
            .find("- pdf (project)")
            .unwrap_or_else(|| panic!("catalog absent: {composed}"));
        assert!(
            wd < cat,
            "workdir line must precede the catalog: {composed}"
        );
    }

    // ── derivation ──

    #[tokio::test]
    async fn derivation_indexes_inline_results_from_the_transcript() {
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let dir = tempfile::tempdir().unwrap();
        write_skill(
            &dir.path().join(".flux/skills"),
            "pdf",
            "pdf",
            "PDF workflows.",
        );
        let full = flux_tools::format_skill_activation(
            &flux_tools::read_skill_file(
                &flux_tools::discover_for_workdir(dir.path()),
                "pdf",
                None,
            )
            .unwrap(),
        );
        let history = vec![assistant_call("c1", "pdf", None), tool_result("c1", &full)];
        let index = derive_activation_index(&store, "chat", &history).await;
        // Same path, same spelling → the tool's next read hits.
        let tool = SkillReadTool::new(index);
        let note = tool
            .call(args("pdf", None), tool_ctx(dir.path(), "c2"))
            .await
            .unwrap();
        assert!(note.contains("already in context"), "{note}");
        assert!(note.contains("\"c1\""));
        assert!(!note.contains("buf_read"));
    }

    #[tokio::test]
    async fn derivation_resolves_overflowed_results_through_the_buffer() {
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let dir = tempfile::tempdir().unwrap();
        // A skill whose formatted activation exceeds the inline budget.
        let skill_dir = write_skill(&dir.path().join(".flux/skills"), "big", "big", "Big skill.");
        fs::write(
            skill_dir.join("SKILL.md"),
            format!(
                "---\nname: big\ndescription: Big skill.\n---\n\n# Big\n{}",
                "x".repeat(INLINE_BUDGET + 500)
            ),
        )
        .unwrap();
        let full = flux_tools::format_skill_activation(
            &flux_tools::read_skill_file(
                &flux_tools::discover_for_workdir(dir.path()),
                "big",
                None,
            )
            .unwrap(),
        );
        assert!(full.chars().count() > INLINE_BUDGET);
        // buf_entries FK to the chats table — seed the chat row first.
        store.insert_chat("chat", "t").await.unwrap();
        // The transcript carries head + ref; the store carries the full text.
        let head_end = full
            .char_indices()
            .nth(INLINE_BUDGET)
            .map_or(full.len(), |(i, _)| i);
        let truncated = format!(
            "{}\n\n--- output truncated ({} chars total) ---",
            &full[..head_end],
            full.chars().count()
        );
        store.save_buf_entry("chat", "c1", &full).await.unwrap();
        let history = vec![
            assistant_call("c1", "big", None),
            tool_result("c1", &truncated),
        ];
        let index = derive_activation_index(&store, "chat", &history).await;
        let tool = SkillReadTool::new(index);
        let note = tool
            .call(args("big", None), tool_ctx(dir.path(), "c2"))
            .await
            .unwrap();
        assert!(note.contains("already in context"), "{note}");
        // The note hands over the paging ref — the transcript copy is a head.
        assert!(note.contains("buf_read {\"ref\": \"c1\""), "{note}");
    }

    #[tokio::test]
    async fn unparseable_or_resultless_calls_are_skipped() {
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let history = vec![
            // Foreign/garbage arguments — skipped, no key.
            Message {
                role: Role::Assistant,
                content: String::new(),
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: SKILL_READ_TOOL.into(),
                    arguments: "not json".into(),
                }],
                tool_call_id: None,
            },
            // Resultless call — skipped.
            assistant_call("c2", "pdf", None),
        ];
        let index = derive_activation_index(&store, "chat", &history).await;
        assert!(index.entries.lock().unwrap().is_empty());
    }

    // ── the tool ──

    #[tokio::test]
    async fn first_read_returns_full_wrapped_content_then_a_dedup_note() {
        // The tool reads from the boundary directly; no store is wired in this test.
        let _store = Arc::new(Store::open_in_memory().await.unwrap());
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = write_skill(
            &dir.path().join(".flux/skills"),
            "pdf",
            "pdf",
            "PDF workflows.",
        );
        fs::write(skill_dir.join("refs.md"), "reference body").unwrap();
        let index = Arc::new(SkillActivationIndex::default());
        let tool = SkillReadTool::new(index);

        let first = tool
            .call(args("pdf", None), tool_ctx(dir.path(), "c1"))
            .await
            .unwrap();
        assert!(first.starts_with("<skill_content name=\"pdf\">"), "{first}");
        assert!(first.contains("refs.md")); // bundled listing
        assert!(!first.contains("description: PDF")); // frontmatter stripped

        let second = tool
            .call(args("pdf", None), tool_ctx(dir.path(), "c2"))
            .await
            .unwrap();
        assert!(second.contains("already in context"), "{second}");
        assert!(second.contains("\"c1\""));

        // A different spelling of the same file converges on the key.
        let aliased = tool
            .call(args("pdf", Some("./SKILL.md")), tool_ctx(dir.path(), "c3"))
            .await
            .unwrap();
        assert!(aliased.contains("already in context"), "{aliased}");

        // A bundled file is a different key — full raw content, unwrapped.
        let reference = tool
            .call(args("pdf", Some("refs.md")), tool_ctx(dir.path(), "c4"))
            .await
            .unwrap();
        assert_eq!(reference, "reference body");
    }

    #[tokio::test]
    async fn body_edits_miss_but_frontmatter_edits_still_hit() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = write_skill(
            &dir.path().join(".flux/skills"),
            "pdf",
            "pdf",
            "PDF workflows.",
        );
        let index = Arc::new(SkillActivationIndex::default());
        let tool = SkillReadTool::new(index);

        tool.call(args("pdf", None), tool_ctx(dir.path(), "c1"))
            .await
            .unwrap();

        // Frontmatter-only edit: the body the model follows is unchanged → hit.
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: pdf\ndescription: Reworked description.\n---\n\n# pdf\n",
        )
        .unwrap();
        let hit = tool
            .call(args("pdf", None), tool_ctx(dir.path(), "c2"))
            .await
            .unwrap();
        assert!(hit.contains("already in context"), "{hit}");

        // Body edit: the content the model follows changed → full re-injection.
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: pdf\ndescription: Reworked description.\n---\n\n# pdf v2\n",
        )
        .unwrap();
        let miss = tool
            .call(args("pdf", None), tool_ctx(dir.path(), "c3"))
            .await
            .unwrap();
        assert!(miss.contains("# pdf v2"), "{miss}");
    }

    #[tokio::test]
    async fn errors_are_self_correcting_and_never_recorded() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(
            &dir.path().join(".flux/skills"),
            "pdf",
            "pdf",
            "PDF workflows.",
        );
        let index = Arc::new(SkillActivationIndex::default());
        let tool = SkillReadTool::new(index);

        let err = tool
            .call(args("nope", None), tool_ctx(dir.path(), "c1"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown skill"));

        let escape = tool
            .call(args("pdf", Some("../../x")), tool_ctx(dir.path(), "c2"))
            .await
            .unwrap_err();
        assert!(
            escape.to_string().contains("escapes") || escape.to_string().contains("cannot read")
        );

        // Nothing recorded — the index only carries successful reads.
        assert!(tool.index.entries.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn tool_surface_matches_the_pinned_contract() {
        let tool = SkillReadTool::new(Arc::default());
        assert_eq!(tool.name(), "skill_read");
        assert_eq!(
            tool.schema(),
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Skill name as listed by skill_list." },
                    "path": { "type": "string", "description": "File to read, relative to the skill's directory. Defaults to SKILL.md." },
                },
                "required": ["name"],
                "additionalProperties": false
            })
        );
        let description = tool.description();
        assert!(description.contains("frontmatter-stripped"));
        assert!(description.contains("<skill_content>"));
    }
}
