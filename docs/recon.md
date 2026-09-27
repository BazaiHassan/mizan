# Phase 0 Recon

Date: 2026-09-27. Sources: RTK `rtk-ai/rtk` @ `45b0b4c` (v0.49.0, Apache-2.0),
Lerim `lerim-dev/lerim-cli` @ `5fed45a` (Apache-2.0), Claude Code 2.1.283, and the
official docs at code.claude.com. Real session logs were inspected locally only;
nothing from `~/.claude/` is committed.

What is **verified** here: read directly in source, docs, or a real log file.
What is **unverified**: marked `UNVERIFIED`. RTK was not executed during recon
(building it was blocked in this environment), so runtime behaviour is read from
source, not observed.

---

## TL;DR — findings that change the brief

1. **RTK already does part of phase 1.** `rtk discover` scans Claude Code JSONL for
   commands that could have been rewritten (≈ our "waste report"), `rtk session`
   shows RTK adoption per session, `rtk learn` finds fail→fix command pairs, and
   `rtk gain --recalls` reports "elisions vs agent recalls" per filter (close to our
   "over-compression detection"). mzn must not be a clone of these. Its unique value
   is **per-project tuning**: finding project-specific commands with no filter at
   all and **generating `.rtk/filters.toml` rules** for them, plus a weekly report.
2. **RTK supports custom filters without source changes.** A hard fork is not
   needed (section 2.3).
3. **We probably don't need our own hook, at least in phase 1–2.** RTK already
   logs every hook decision to SQLite **keyed by Claude's `tool_use_id`**
   (section 2.5), so "did this call go through RTK?" is answered by a join, with no
   extra hook. Replacing RTK's hook (the `activate` plan) is risky: `rtk init`
   re-adds its hook, and RTK warns about a missing/outdated hook. Proposal in
   section 7.
4. **The "`rtk rtk git status`" failure mode in the brief is not what happens.**
   Matching hooks run **in parallel** on the same original input, so two rewriting
   hooks don't chain. The real risk is that when two hooks both return
   `updatedInput`, which one wins is **undocumented**. The conclusion is the same:
   never have two rewriting hooks on Bash.
5. **`includeCoAuthoredBy` is deprecated.** The current setting is `attribution`
   (`commit`, `pr`, `sessionUrl`). `.claude/settings.json` sets both.
6. **Custom filters must be trusted.** RTK ignores project/global filter files
   until the user runs `rtk trust`, and any edit (including ours) invalidates trust.
   `suggest --apply` must say so and tell the user to re-run `rtk trust`. That's
   good for consent, but it's a UX step to design for.

---

## 1. Claude Code session logs

**Location:** `~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl`. The encoded
cwd replaces each non-alphanumeric char with `-` (`/home/user/mizan` →
`-home-user-mizan`). A sibling dir `<session-uuid>/` holds `tool-results/*.txt`
(spilled large outputs) and, for subagents, sidechain transcripts. RTK's
`discover/provider.rs` does the same encoding, including Windows paths
(`C:\Users\foo` → `C--Users-foo`).

**Format:** one JSON object per line. Observed top-level `type`s: `user`,
`assistant`, `attachment`, `ai-title`, `last-prompt`, `queue-operation`,
`atis-latch`. Parse only `assistant` and `user`, and skip everything else.

Common fields on `user`/`assistant`: `uuid`, `parentUuid`, `sessionId`,
`timestamp` (ISO-8601), `cwd`, `gitBranch`, `version` (Claude Code version),
`isSidechain` (true = subagent), `message`.

**Bash call** (`type: "assistant"`):
```json
{"type":"assistant","uuid":"…","timestamp":"…","cwd":"/proj","isSidechain":false,
 "message":{"role":"assistant","content":[
   {"type":"tool_use","id":"toolu_…","name":"Bash",
    "input":{"command":"git status","description":"…"}}]}}
```
A single assistant message may hold several `tool_use` blocks (parallel calls).

**Result** (`type: "user"`):
```json
{"type":"user","sourceToolAssistantUUID":"<assistant uuid>",
 "message":{"role":"user","content":[
   {"type":"tool_result","tool_use_id":"toolu_…","content":"…","is_error":false}]},
 "toolUseResult":{"stdout":"…","stderr":"…","interrupted":false,"isImage":false}}
```

**Linking:** `tool_result.tool_use_id` == `tool_use.id`. That's the only reliable
key. `sourceToolAssistantUUID` also points back to the assistant line.

**Gotchas that affect token accounting:**
- `tool_result.content` is either a string or an array of content blocks
  (`[{type:"text",text:…}]`). Handle both.
- **What the model saw ≠ what the command printed.** For large outputs,
  `content` holds a `<persisted-output>` stub with a ~2 KB preview and a path
  under `tool-results/`, while `toolUseResult.stdout` holds the full output.
  Token cost must be measured from `message.content`. Command verbosity comes from
  `toolUseResult`.
- `toolUseResult` is sometimes a plain string (e.g. `"Error: …"` for denied
  calls). `is_error: true` marks failures and denials.
- Assistant lines may also carry `wireToolInputs[tool_use_id].command`, which can
  differ from `input.command`. Observed: the wire form kept a leading
  `cd /path &&` that `input` lacked. **UNVERIFIED:** which field records the
  post-hook (RTK-rewritten) command. Must test on a machine with RTK installed. The
  RTK SQLite join (2.5) avoids depending on it.
- Unknown fields appear between versions (`advisorModel`, `wireIngestContext`,
  …). Deserialize leniently (`serde(default)`, no `deny_unknown_fields`) and skip
  and count bad lines.
- Sidechain (subagent) Bash calls exist too. Decide whether they count (proposal:
  yes, flagged).

## 2. RTK

### 2.1 Hook mechanism
- Current install (`rtk init -g`) writes to **`~/.claude/settings.json`**:
  ```json
  {"hooks":{"PreToolUse":[{"matcher":"Bash",
    "hooks":[{"type":"command","command":"rtk hook claude"}]}]}}
  ```
  (`src/hooks/init/mod.rs::insert_hook_entry`; the command may be an absolute path,
  e.g. `/opt/homebrew/bin/rtk hook claude`.) `rtk init` (non-global) doesn't add a
  hook. It patches `CLAUDE.md` and creates a `.rtk/filters.toml` template.
- Legacy install: `~/.claude/hooks/rtk-rewrite.sh` (bash + jq, calls `rtk rewrite`)
  with a SHA-256 baseline in `~/.claude/hooks/.rtk-hook.sha256`. RTK migrates it
  away on re-init.
- RTK's own detection (`hook_already_present`) matches a command equal to
  `rtk hook claude`, any command RTK recognizes as its Claude hook, or any
  containing `rtk-rewrite.sh`. mzn doctor should use the same rules.
- `rtk init` backs up settings to `.bak` and writes atomically.

### 2.2 Rewrite logic (callable from another hook: **yes**, two ways)
- `rtk rewrite "<cmd>"`: prints the rewritten command. Exit codes: `0` rewrite +
  allow rule matched, `1` no RTK equivalent, `2` deny rule matched, `3` rewrite but
  let Claude Code ask. Scrub `RTK_REWRITE_HOST` before calling.
- `rtk hook claude`: reads the full PreToolUse JSON on stdin and prints the
  hook JSON response. A delegating hook can pipe stdin through it unchanged. That
  is the cleanest delegation if we ever own the hook.
- The rewrite is compound-aware (`&&`, `;`, pipes, env prefixes, `timeout`
  wrappers) and idempotent on already-`rtk` commands.
- `hooks.exclude_commands` in RTK's `config.toml` disables rewriting per command.

### 2.3 User-defined filters (supported, no fork needed)
- Files: project **`.rtk/filters.toml`** (a **relative path**, resolved against the
  rtk process's cwd) and global **`<config_dir>/rtk/filters.toml`**
  (`~/.config/rtk/filters.toml` on Linux, `~/Library/Application Support/rtk/` on
  macOS).
- Format: `schema_version = 1`, then `[filters.<name>]` with `match_command`
  (fully-anchored regex), `strip_ansi`, `filter_stderr`, `strip_lines_matching`,
  `keep_lines_matching`, `replace`, `match_output`, `truncate_lines_at`,
  `max_lines`, `tail_lines`, `on_empty`, plus inline `[[tests.<name>]]`.
- Load order: project → global → built-in. First match wins. Within a file,
  filters are held in a `BTreeMap`, so **order is alphabetical by name**, not file
  order.
- `rtk rewrite` routes a command to `rtk <cmd>` when a (trusted) TOML filter
  matches it, so a generated filter takes effect through the existing hook with
  no extra wiring.
- **Trust gate:** untrusted or changed files are silently skipped. Trust is a
  SHA-256 of the file's content in `~/.local/share/rtk/trusted_filters.json`, and
  users grant it with `rtk trust`. `RTK_TRUST_PROJECT_FILTERS=1` works only in CI.
- Implication for "managed block": RTK only reads these two files, so a separate
  mzn-only file isn't possible. We write a marked `# >>> managed by mzn >>>` …
  `# <<< managed by mzn <<<` block inside `.rtk/filters.toml`, name our filters
  `mzn-*`, and never generate a `match_command` that overlaps a user filter. We
  must check overlap ourselves because name order, not authorship, decides
  precedence.

### 2.4 Version detection
`rtk --version` → `rtk 0.49.0`. **Name collision:** `reachingforthejack/rtk`
(Rust Type Kit) uses the same binary name. Confirm by checking that
`rtk gain --help` succeeds, as RTK's own docs suggest. Proposed supported range:
`>=0.40, <0.50`, to be decided after testing against real binaries.

### 2.5 Stats storage (`rtk gain`)
SQLite at `RTK_DB_PATH` → `tracking.database_path` in config → default
`<data_local_dir>/rtk/history.db` (`~/.local/share/rtk/history.db` on Linux).
Tables:
- `commands(timestamp, original_cmd, rtk_cmd, input_tokens, output_tokens,
  saved_tokens, savings_pct)`, tokens estimated as bytes/4.
- `hook_decisions(timestamp, session_id, tool_use_id, project_path, raw_cmd,
  decision, rewritten_cmd, rtk_version)`. **Joins directly to our JSONL on
  `tool_use_id`.** Written only by the native `rtk hook claude` path
  (`hook_cmd.rs`), not by `rtk rewrite`. Users still on the legacy
  `rtk-rewrite.sh` hook, or on older RTK, have no rows. We fall back to
  command-prefix heuristics for them.
- `parse_failures(...)`.
- A recall store (`rtk recall`) keeps elided raw output. `rtk gain --recalls`
  reports how often agents recalled it.

Machine-readable output exists: `rtk gain --format json`,
`rtk discover --format json`, `rtk learn --format json`. Prefer shelling out to
these, with read-only SQLite as a fallback, over depending on the DB schema.

Config: `<config_dir>/rtk/config.toml`.

**Telemetry:** RTK has opt-in telemetry (off by default). mzn never touches it.

## 3. Claude Code plugins
- Layout: `.claude-plugin/plugin.json` (optional manifest), `skills/<name>/SKILL.md`
  (preferred for new plugins), `commands/*.md` (legacy slash commands),
  `hooks/hooks.json`, `bin/` (on the Bash tool's PATH while enabled; note that
  claude.ai/Cowork refuse to install plugins that have `bin/`).
- Hook commands can use `${CLAUDE_PLUGIN_ROOT}`, `${CLAUDE_PLUGIN_DATA}`
  (`~/.claude/plugins/data/<id>/`, survives updates), and `${CLAUDE_PROJECT_DIR}`.
- Distribution: a git repo with a marketplace manifest, added with
  `/plugin marketplace add owner/repo` and installed with
  `/plugin install <name>@<marketplace>`.
- Proposal: the plugin does **not** ship the binary. It expects `mzn` on PATH
  (installed via cargo/brew/release). `/coach` becomes a skill that runs
  `mzn analyze`. The optional `SessionEnd` hook runs `mzn report --quiet`.
  SessionEnd input: common fields (`session_id`, `transcript_path`, `cwd`) plus a
  reason (`clear`, `resume`, `logout`, `prompt_input_exit`, `other`).

## 4. Hook ordering / conflicts
- Docs: "All matching hooks run in parallel." No ordering, and no documented
  result when several PreToolUse hooks return `updatedInput`.
- Hooks **merge** across user, project, local, and managed settings and enabled
  plugins. None override each other. A plugin hook plus RTK's settings hook =
  two hooks.
- No per-hook disable exists. Only `disableAllHooks` or deleting the entry.
- Cloud sessions ignore `~/.claude/settings.json`, so in claude.ai/code, RTK
  installed globally is inactive (relevant to `doctor` messaging).

## 5. Lerim (phase 3 only)
- Python ≥3.11 plus Node ≥20. Store: `~/.lerim/context.sqlite3`. Config:
  `~/.lerim/config.toml`. Logs and workspace: `~/.lerim/logs/`, `~/.lerim/workspace/`.
- Claude Code MCP: `lerim connect` writes `mcpServers.lerim = {type:"stdio",
  command:<python>, args:["-m","lerim.mcp_server"]}` into **`~/.claude.json`**.
- Canonical repo is unclear: the README badges point at `kargarisaac/lerim`, and
  the clone at `lerim-dev/lerim-cli` works. Ask before forking.

## 6. Settings for this repo
`.claude/settings.json` sets `attribution.commit = ""`, `attribution.pr = ""`,
`attribution.sessionUrl = false`, and the legacy `includeCoAuthoredBy = false` for
older clients. **UNVERIFIED:** the value type of `sessionUrl`. The docs describe it
as "omit the session link"; boolean assumed.

## 7. Proposed architecture

Rust, single binary, edition 2024. No network crates in the dependency tree
(enforced in CI with `cargo deny` or a `cargo tree` check).

```
src/
  main.rs            clap CLI: analyze | doctor | suggest | report | activate | deactivate
  paths.rs           claude/rtk/mzn dirs, cwd→project encoding
  session/           JSONL reader: lenient serde, tool_use↔tool_result pairing,
                     malformed-line counter, content-string-or-blocks, persisted-output
  rtk/
    detect.rs        binary, version, name-collision check
    stats.rs         rtk gain/discover --format json; read-only SQLite fallback
    hooks.rs         find RTK hook entries in the 3 settings files
    filters.rs       read .rtk/filters.toml, managed-block edit, overlap check
  analyze/           per-command aggregation, coverage, waste, re-run detection
  suggest/           command family → TOML filter template
  report/            text + markdown weekly summary
  settings/          settings.json patching, backups, state.json (activate/deactivate)
plugin/              .claude-plugin/plugin.json, skills/coach/SKILL.md, hooks/hooks.json
tests/fixtures/      synthetic JSONL + settings.json fixtures
```

Command classification: "went through RTK" = `hook_decisions` row with a rewrite
for that `tool_use_id`, or the command starts with `rtk `. Otherwise, call
`rtk rewrite` in dry-run form to ask whether RTK *would* cover it (exit 1 =
uncovered). This reuses RTK's classifier instead of duplicating 60+ regexes.
In source, `rtk rewrite` doesn't write `hook_decisions` (only `rtk hook claude`
does), so calling it won't skew RTK's stats. **UNVERIFIED** at runtime.

**Proposed change to 2.5 (activate/deactivate):** ship `doctor` as specified, but
make `activate` optional and **not replace RTK's hook** by default. With
`hook_decisions` available, mzn needs no hook to measure anything. Replacing RTK's
hook fights `rtk init` and RTK's hook-health warnings. Keep the full
activate/deactivate machinery (backups, state file, round-trip tests) only if you
still want mzn-side logging. I'd rather cut it and add a `doctor` conflict check
for duplicate Bash rewriting hooks.

## 8. Open questions for you
1. Accept the repositioning (mzn = per-project filter generator + reporting on top
   of `rtk discover/gain`, not a re-implementation)?
2. Drop or defer the own-hook `activate` step (section 7)?
3. Minimum RTK version: require one with `hook_decisions` (to be pinned after
   testing), or support older versions in a degraded mode?
4. Fork RTK now? Canonical upstream is clearly `rtk-ai/rtk`. I didn't fork it,
   because nothing in phases 1–2 needs a fork.
5. Count subagent (sidechain) Bash calls in reports?
6. Supported OS for phase 1: Linux + macOS first, with Windows builds but lighter
   testing?
