# Changelog

All notable changes to this project are documented here. This file is
maintained by [release-please](https://github.com/googleapis/release-please)
from Conventional Commit messages.

## [0.1.1](https://github.com/BazaiHassan/mizan/compare/mzn-v0.1.0...mzn-v0.1.1) (2026-09-27)


### Features

* add analyzer, filter generator and weekly report ([f2a26be](https://github.com/BazaiHassan/mizan/commit/f2a26be0cee888026c00f874bd4728be64ecca26))
* add Claude Code plugin with /coach command ([2e14e44](https://github.com/BazaiHassan/mizan/commit/2e14e449e691fb4ab8efaf323bb1532929852111))
* add doctor and reversible activate/deactivate ([4936db1](https://github.com/BazaiHassan/mizan/commit/4936db113bdfc12f72614b852bd7c47e3e0a5f61))
* add mzn command-line interface ([74e2d72](https://github.com/BazaiHassan/mizan/commit/74e2d724f2859521e3bf74cdc44c004616901b33))
* detect RTK and read its stats and filter files ([0e92ebc](https://github.com/BazaiHassan/mizan/commit/0e92ebcb36c4cdfb20a020101ba5a62cd39bcb4c))
* parse Claude Code session logs and group commands ([8165a6c](https://github.com/BazaiHassan/mizan/commit/8165a6cbc50eb5ca89a33204dfe7645953c05a57))


### Bug Fixes

* keep earlier generated filters and refuse writes in read-only mode ([5c4e220](https://github.com/BazaiHassan/mizan/commit/5c4e220067c5217dea8fbef3de7b40c80881a96d))

## 0.1.0 (unreleased)

### Features

* `mzn analyze`: per-command token cost (estimated), RTK coverage via RTK's
  hook-decision log or `rtk rewrite`, waste report, and over-compression
  detection (raw re-run shortly after an RTK run). Text and `--json`.
* `mzn doctor`: RTK binary/version/mode, RTK hooks across user, project and
  local settings, duplicate or conflicting hooks, filter trust status, Lerim
  detection. Read-only. `--install-rtk` offers RTK's official installer.
* `mzn suggest`: generates RTK TOML filters for costly uncovered commands in a
  managed block of `.rtk/filters.toml`; dry-run diff by default, `--apply`,
  `--clear`.
* `mzn report`: weekly summary in plain text or markdown.
* `mzn activate` / `mzn deactivate`: keep exactly one RTK hook, add a
  SessionEnd stats hook, with backups, a state file and byte-exact restore.
* Claude Code plugin with `/coach`.
