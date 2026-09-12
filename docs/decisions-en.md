# Flux Accepted Tradeoffs

> Existing compromises that are explicitly accepted — do not fix as defects.
> A new entry must state "why accepted" and "when to re-evaluate".

## Accepted tradeoffs

### T-01 VACUUM write-lock window

Under WAL, VACUUM rewrites the whole database while holding the write lock (triggered only when the freelist crosses a threshold; failure surfaces as a transient write error + log). Accepted; if hardening is ever needed: run it once before the listener binds, or switch to incremental cleanup.

### T-02 Inherent risk of runner-class bash commands

Under the defense that classifies commands by their first word, `eval`/`sudo`/interpreter-direct runners can execute arbitrary commands without any smuggling — that is the inherent semantics of "allow by first word", not a bypass (the notice shows the full command). Rejected alternatives: a runner blocklist (false-positives on legitimate use), disabling first-word allow (sacrifices convenience). The approval layer is gone; this entry stays in case approvals ever return.

### T-03 The state tool's generic KV behavior

state writes any key / reads of unknown keys return an empty string — **by design** (a generic KV channel for the agent across rounds; the schema enum is guidance, not a whitelist; read/write are symmetric). Do not fix as a defect.

### T-04 Performance exclusions

Per-viewer clones in the streaming fan-out (no benefit with a single viewer), unbounded-queue hardening (defensive), splitting the global lock's cross-await hold into two phases (~0 benefit for a single user — re-evaluate with new evidence if the multi-window premise changes). Breaking protocol changes (binary frames/compression) were rejected after measurement showed the wire format is not the bottleneck.

### T-05 Explorer git status via the git CLI, not git2/gix

`fsbrowse` shells out to `git` for directory status collection (two calls per listed directory: `rev-parse --show-toplevel` + `status --porcelain -z`), already bounded by a `-- .` pathspec on the listed subtree, `-unormal` folding of untracked directories, and execution on the blocking thread pool. `git2` (C toolchain dependency + unsafe + a worktree/config compatibility surface) and `gix` (pure Rust, but the high-level status API is still in flux and the dependency tree is heavy) are not introduced — for the UI load of re-listing loaded directories every 15s, CLI compatibility and zero new heavy dependencies win. Re-evaluate when: status calls become a measured hotspot (large-repo cold scan > several hundred ms), or the gix status API reaches stability.

### T-06 No startup-time validation of the web build

The web server (on by default) does not validate the dist layout at startup: an incomplete build surfaces **at request time** — index read failure → 500 + warn log; missing assets → 404 (visible in the browser console). Why: refusing to start would misread the perfectly usable state of "agent available, UI assets pending rebuild" as fatal, and the failure chain is already attributable (the warn names the path / the console names the resource). Re-evaluate when: request-time exposure proves hard to attribute (e.g. users cannot tell a misconfigured assets dir from an incomplete build), or multi-entry pages complicate the failure shapes.

### T-07 Streaming render pipeline keeps marked — no renderer swap, no streaming library

The streaming markdown pipeline stays on marked + splitAndFold/ParagraphSplitter + renderStableSlice + FenceCache (researched and decided 2026-08-28; do not re-propose a swap): ① marked has no incremental continuation API — the trailing `text` token absorbs unclosed inline constructs, so caching at token boundaries loses inline context; ② micromark's true streaming entry is Node-only, and the browser fallback degrades to a full re-parse (the O(n²) remains); ③ the ecosystem's "streaming markdown renderers" (streamdown, Vercel AI SDK, etc.) are actually remend self-healing of incomplete blocks + full re-parse + DOM diff, all carrying a React peer dep. The current pipeline's per-block caching / stable-prefix scheme is finer-grained than those implementations; the residual O(n²) lives only in pathological wall-of-text paragraphs (measured: 64KB ≈ 12ms, clamped to the current paragraph). Re-evaluate when: a framework-free incremental renderer with a stable prefix-cache contract appears, or real-world inputs measurably exceed the frame budget.
