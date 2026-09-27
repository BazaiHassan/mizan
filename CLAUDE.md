# CLAUDE.md

Start every session by reading `BUILD_BRIEF.md` and `docs/recon.md` (§9 lists the
decisions made so far).

## Commands

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
```

## Rules

- Commits: Conventional Commits, small and focused, authored as the repo owner.
  No Claude attribution lines in commits, PRs or docs.
- The binary must stay network-free (CI enforces it) and must never call an LLM.
- Never commit real session logs or anything from `~/.claude/`; fixtures are
  synthetic (`tests/fixtures/`, `__PROJECT__` is replaced at test time).
- The session log parser must tolerate unknown fields and malformed lines.
- Never edit user RTK filters; only the `# >>> managed by mzn >>>` block.
