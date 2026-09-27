# Changelog

All notable changes to this project are documented here. This file is
maintained by [release-please](https://github.com/googleapis/release-please)
from Conventional Commit messages.

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
