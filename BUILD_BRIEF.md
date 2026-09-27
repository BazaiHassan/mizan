# Build Brief: mzn

> Fill in every `{{PLACEHOLDER}}` below before starting. Do not guess them.

| Placeholder | Value |
|---|---|
| `{{GITHUB_USER}}` | My GitHub username |
| `{{GIT_NAME}}` | Name for commits |
| `{{GIT_EMAIL}}` | Email for commits (must match my GitHub account) |
| `{{REPO_URL}}` | The repo I created for this project |
| Tool name | `mzn` (short for *mizan*, Persian "to tune/calibrate") |
| `{{LICENSE}}` | Default: Apache-2.0 |

---

## 0. Identity & Git rules (read first, apply always)

All work must appear as mine. Before any commit:

1. Verify GitHub CLI auth is my account:
   ```bash
   gh auth status   # must show {{GITHUB_USER}}
   ```
   If not logged in as me, STOP and ask me to run `gh auth login`. Never store tokens in the repo or in files.
2. Set identity locally in every repo you touch:
   ```bash
   git config user.name  "{{GIT_NAME}}"
   git config user.email "{{GIT_EMAIL}}"
   ```
3. Disable Claude attribution. Create/update `.claude/settings.json` in the project:
   ```json
   { "includeCoAuthoredBy": false }
   ```
   (If this setting name has changed in the installed Claude Code version, find the current equivalent and use it.)
   Do NOT add `Co-Authored-By`, "Generated with Claude Code", or similar lines to commits, PR titles, PR bodies, or README.
4. Commit style: Conventional Commits (`feat:`, `fix:`, `docs:`, `test:`, `chore:`), small and focused. One logical change per commit.
5. Never push to, or open PRs/issues against, any upstream repo without asking me first.
6. Never commit real session logs, API keys, `.env` files, or anything from `~/.claude/`. Test fixtures must be synthetic.

### Licensing note (required, not optional)
My own code is copyright me. Any code copied or adapted from RTK or Lerim must keep its original license and copyright notice (add a `NOTICE` / `THIRD_PARTY_LICENSES` file). Do not remove upstream attribution.

---

## 1. Repos to set up

1. Clone my repo `{{REPO_URL}}`. This is the main product repo; all product code lives here.
2. Fork RTK to my account for reference and for filter/patch work that may go upstream later:
   ```bash
   gh repo view rtk-ai/rtk   # verify this is the canonical upstream (most stars, active). Many forks exist; if unsure, ask me.
   gh repo fork <canonical-upstream> --clone=false
   ```
3. Do NOT fork Lerim yet. It is phase 3 and optional.

Architecture decision: **RTK is a runtime dependency, not vendored code.** The product calls the installed `rtk` binary and reads/writes RTK's config. Keep a hard fork only if phase 0 proves RTK cannot support custom filters without source changes.

---

## 2. What we're building

A lightweight, free, open-source companion to RTK for Claude Code that **tunes RTK to each project automatically.**

One-line pitch: *"RTK saves tokens. mzn makes RTK fit your project."*

Principles:
- Single binary, fast install. Rust preferred to match RTK (propose an alternative in phase 0 only with a strong reason).
- No LLM calls, no network, no telemetry. Everything is local; reads logs, never uploads.
- Useful on the first run: a report within seconds of install.

---

## 2.5 Coexistence with existing RTK / Lerim installs (hard requirement)

Many users will already have RTK and/or Lerim installed. The tool must **reuse** them, never install a second copy, and never cause double processing.

### Detection (`mzn doctor`)
Report, without changing anything:
- RTK: binary on PATH? version? is it in our supported version range?
- RTK hooks: scan `~/.claude/settings.json`, project `.claude/settings.json`, and `.claude/settings.local.json` for RTK hook entries.
- RTK config: location of existing user filters.
- Lerim (phase 3): installed? running (`lerim status`)? MCP entries in Claude Code config? location of its context store.
- Conflicts found: e.g. two hooks rewriting the same Bash calls (would produce `rtk rtk git status`), duplicate MCP entries, unsupported versions.

### Reuse rules
- If RTK is installed and in range → use that binary and its existing stats/config. Never bundle or install our own RTK.
- If RTK is missing → print install instructions (or offer to run the official installer with `--install-rtk`). Never install silently.
- If version is out of range → warn and run in read-only analysis mode.
- Lerim (phase 3): reuse the existing store and daemon. Never start a second ingest process or a second store.

### Hook ownership (`activate` / `deactivate`)
When the user runs `mzn activate`:
1. Run `doctor`, show exactly what will change, and **ask for confirmation** (`--yes` to skip). Never disable anything silently.
2. Back up every file it will modify (timestamped copy) and record every change in a state file (`~/.config/mzn/state.json`).
3. Disable RTK's own Claude Code hook entry and install a single `mzn` hook that **delegates to the installed RTK** for rewriting (verify in phase 0 how to invoke RTK's rewrite logic from our hook) and adds our lightweight logging. Result: exactly one hook, RTK still does the compression.
4. Must be idempotent: running `activate` twice changes nothing the second time.

`mzn deactivate` restores the exact previous state from the state file (re-enables RTK's original hook, removes ours). If the user edited those files in between, merge carefully and show a diff rather than overwriting.

README must include manual restore steps in case our binary is deleted before `deactivate`.

### Filter config safety
- Never overwrite or edit the user's existing RTK filters. Generated filters go in a separate, clearly namespaced file or marked block (`# managed by mzn`).
- User-written filters always take precedence over generated ones.
- `suggest --apply` only touches our namespaced section.

### Tests
Fixtures/tests for: no RTK; RTK installed without hook; RTK with hook in global settings; RTK hook in project settings; activate → deactivate round trip restores byte-identical files; activate twice; user edits settings between activate and deactivate.

---

## 3. Phases

### Phase 0: Recon (NO product code yet). Stop and report to me.
Investigate and write findings to `docs/recon.md`:
- Claude Code session log location and format (typically `~/.claude/projects/<encoded-path>/*.jsonl`; this format is undocumented, so confirm by inspecting real files locally, but do not commit them). Document: how Bash tool calls, their commands, and tool results are represented; how to link a result to its call.
- How RTK works: hook mechanism, how commands get rewritten, whether it supports user-defined filters (config file format?), and where `rtk gain` stores its stats.
- Claude Code plugin format (commands, hooks, e.g. `SessionEnd`) and the current way to publish/install a plugin.
- How RTK registers its hook (exact JSON it writes, which settings file), whether its rewrite logic can be called from another hook (subcommand, flag, or library), and how to detect its version reliably.
- Hook execution order when multiple `PreToolUse` hooks match the same Bash call, and whether a plugin hook and a settings.json hook can conflict.
- How Lerim registers MCP entries and where its store lives (for phase 3 reuse).
- Proposed architecture and module layout.

**Wait for my approval before phase 1.**

### Phase 1: MVP analyzer
CLI: `mzn analyze [--project PATH] [--days N]`
- Parse session JSONL for the current project.
- Per command: run count, total output size, estimated tokens (chars/4 is acceptable, clearly labeled "estimate"), whether it went through RTK.
- **Waste report:** top commands by token cost that RTK does not cover.
- **Over-compression detection:** cases where the same command ran via RTK and then ran again raw within a few turns (signal that RTK output was insufficient).
- Output: readable terminal table + `--json` flag.

### Phase 2: Coexistence + filter generator + packaging
- `doctor`, `activate`, `deactivate` as specified in section 2.5. Build these first in this phase, before `suggest --apply`.
- `mzn suggest`: for uncovered project-specific commands (build scripts, test runners, docker, etc.), generate RTK filter rules using RTK's supported config mechanism. Show a diff; write only with `--apply`.
- `mzn report`: weekly shareable summary ("this week you saved ~X tokens; top wins: …"). Plain text + markdown.
- Package as a Claude Code plugin: a `/coach` slash command and an optional `SessionEnd` hook that updates stats quietly.
- Install: one command (`cargo install`, Homebrew tap, prebuilt release binaries via GitHub Actions).

### Phase 3 (later, optional; ask before starting)
- Optional Lerim integration module (cross-session memory).
- Cursor session support.
- Persian/Finglish prompt handling.

---

## 4. Quality bar
- Unit tests with synthetic JSONL fixtures under `tests/fixtures/` (cover: normal run, RTK-wrapped run, raw re-run after RTK, malformed lines).
- Parser must tolerate unknown/new fields and malformed lines (skip + count, never crash). The log format can change between Claude Code versions.
- CI on GitHub Actions: fmt, clippy/lint, tests, release builds for Linux/macOS/Windows.
- README in English: pitch, 30-second install, example output, privacy statement (local-only), relation to RTK (credit it clearly).
- `CHANGELOG.md`, `LICENSE` ({{LICENSE}}), `NOTICE`.

---

## 5. Working agreement
- Start every session by reading this file and `docs/recon.md`.
- At the end of each phase: summarize what was done, what's untested, and open questions. Then stop.
- If something in this brief turns out to be wrong (e.g. a tool/setting doesn't exist), say so and propose a fix instead of working around it silently.
