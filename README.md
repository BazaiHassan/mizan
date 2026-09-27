# mzn

**RTK saves tokens. mzn makes RTK fit your project.**

[RTK](https://github.com/rtk-ai/rtk) cuts the tokens Claude Code spends on
command output by filtering common tools like `git`, `cargo` and `npm`. Every
project also has its own commands, such as build scripts, test runners and
deploy tools, and RTK has no filter for those.

`mzn` (from *mizan*, Persian for "to tune, to balance") reads your local Claude
Code session logs and shows:

- which commands cost the most tokens in **this** project,
- which of them go through RTK, and which slip past it,
- where RTK's output was too terse, so the agent re-ran the command raw.

It can then **generate RTK filters** for the uncovered commands. It never
touches your own filters.

- Single static binary, starts instantly.
- **Local only:** no network, no LLM calls, no telemetry. It only reads logs.
- Reuses the RTK you already have. It never installs a second copy.

## Install (30 seconds)

**Linux (Fedora, Ubuntu, Debian, Arch, openSUSE, …) and macOS:**

```bash
curl -fsSL https://raw.githubusercontent.com/BazaiHassan/mizan/main/install.sh | sh
```

This installs a static binary to `~/.local/bin` and verifies its SHA-256.
`.deb` and `.rpm` packages, and Windows `.zip` builds, are attached to each
[release](https://github.com/BazaiHassan/mizan/releases).

**From source** (any platform with Rust ≥ 1.85):

```bash
cargo install --git https://github.com/BazaiHassan/mizan
```

mzn needs RTK to be useful. If RTK is missing, `mzn doctor` tells you how to
install it, and `mzn doctor --install-rtk` offers to run RTK's official
installer after asking you.

## Use

```bash
cd your-project
mzn doctor              # is RTK installed, hooked, and are your filters trusted?
mzn analyze             # where do tokens go? (last 30 days)
mzn suggest             # proposed RTK filters, as a diff (dry run)
mzn suggest --apply     # write them to .rtk/filters.toml, then run: rtk trust
mzn report --markdown   # weekly shareable summary
```

Every command takes `--project PATH` and `--days N`. Most take `--json`.

### Example

```text
$ mzn analyze
10 Bash calls in 2 sessions (0 in subagents), ~1.8k tokens of output (estimate)
Through RTK: 2 calls (20%), ~9 tokens.  Not covered: ~1.8k tokens (97%).

Waste: costly commands RTK is not filtering
command             runs  ~tokens  ~per run        rtk
------------------  ----  -------  --------  ---------
./scripts/build.sh     3     1.7k       564  uncovered
ls                     2       42        21     missed
git status             2       17         8     missed
  `missed` = RTK has a filter but the call bypassed it (is the hook installed? see `mzn doctor`)
  `uncovered` = no RTK filter exists; `mzn suggest` can generate one

Possible over-compression: re-run without RTK shortly after an RTK run
command     re-runs  ~tokens
----------  -------  -------
cargo test        1       47

$ mzn suggest
  mzn-scripts-build-sh  (3 runs, ~1.7k → ~18 tokens, -99% estimated)
      download/install chatter: 59% of lines
      compile progress: 39% of lines
+[filters.mzn-scripts-build-sh]
+match_command = '^\./scripts/build\.sh(\s|$)'
+strip_lines_matching = [ ... ]
```

Token numbers are **estimates**: characters ÷ 4 of the output the model
actually received. For outputs Claude Code spilled to disk, only the preview
the model saw is counted.

### How generated filters stay safe

- Filters only **remove lines**. Patterns come from a fixed catalogue (progress
  bars, download chatter, compile progress, passing tests, docker layers, …).
  A pattern is used only if it matched real output of that command and never
  matched a line mentioning an error, failure, warning or similar.
- mzn writes only between `# >>> managed by mzn >>>` and
  `# <<< managed by mzn <<<` in `.rtk/filters.toml`. Your filters outside the
  block are never edited. Commands your own filters already match are skipped,
  so your filters always win.
- RTK ignores changed filter files until you approve them with `rtk trust`. mzn
  never trusts files on your behalf. `mzn suggest --clear` removes the block.

## Optional: `activate`, the SessionEnd hook, and the `/coach` plugin

`mzn activate` makes sure **exactly one** RTK hook handles Bash. Claude Code
runs matching hooks in parallel, and which rewrite wins is undefined. It also
adds a quiet `SessionEnd` hook (`mzn hook session-end`) that appends a
one-line summary per session to `~/.local/share/mzn/sessions.jsonl`. RTK's own
hook keeps doing the compression.

It shows the exact diff and asks first (`--yes` skips the prompt). It backs up
every file, records each change in `~/.config/mzn/state.json`, and is
idempotent. `mzn deactivate` restores the previous files byte for byte. If you
edited them in between, it merges the reversal into your current content and
shows you the diff first.

**Claude Code plugin:** adds `/coach`, which runs doctor, analyze and suggest,
then summarizes the results. It requires the `mzn` binary on `PATH`.

```text
/plugin marketplace add BazaiHassan/mizan
/plugin install mzn@mizan
```

### Manual restore (if the mzn binary is gone)

`mzn activate` only ever changes `hooks` entries in:
`~/.claude/settings.json`, `<project>/.claude/settings.json`,
`<project>/.claude/settings.local.json`.

1. Originals are in `~/.config/mzn/backups/<timestamp>/`, named after the full
   path of each file. `~/.config/mzn/state.json` lists each file, its backup,
   and the hooks removed and added.
2. If you haven't edited the settings since, copy each backup over its file.
   Otherwise, open the file and remove the `SessionEnd` entry whose command is
   `mzn hook session-end`. Then re-add any RTK hook listed under `removed` in
   `state.json`.
3. Delete `~/.config/mzn/state.json`.

(On macOS, `~/.config` above is `~/Library/Application Support`.)

## Privacy

mzn reads `~/.claude/projects/**.jsonl` (Claude Code's local session logs),
RTK's local stats database (read-only), and settings files. It writes only:

- `.rtk/filters.toml`, with `suggest --apply`,
- settings files, with `activate`/`deactivate`,
- its own state under `~/.config/mzn` and `~/.local/share/mzn`.

The binary makes no network connections; CI checks that no HTTP/TLS crate is
linked. The only network use anywhere is `install.sh` and
`mzn doctor --install-rtk`, and only when you run them.

## Relation to RTK and Lerim

mzn is an independent companion to **[RTK (Rust Token Killer)](https://github.com/rtk-ai/rtk)**
and would be pointless without it. All the compression is RTK's work. mzn adds:

- per-project measurement: `rtk discover` and `rtk gain` look across projects,
- over-compression detection,
- filter generation in RTK's own TOML format.

mzn calls the installed `rtk` for classification (`rtk rewrite`). It joins
RTK's hook-decision log to Claude Code sessions by `tool_use_id`, which needs
RTK ≥ 0.48. With RTK 0.23–0.47 it runs a read-only degraded analysis.

**[Lerim](https://github.com/lerim-dev/lerim-cli)** users: `mzn doctor` detects
Lerim and flags duplicate MCP registrations. mzn never starts Lerim or a second
store.

## Development

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
```

Tests use synthetic fixtures only (`tests/fixtures/`). Design notes and
format research are in [`docs/recon.md`](docs/recon.md). Commits follow
Conventional Commits; releases and `CHANGELOG.md` are managed by release-please.

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
