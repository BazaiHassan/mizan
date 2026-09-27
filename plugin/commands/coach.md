---
description: Show where this project's command output wastes tokens and how to tune RTK for it
allowed-tools: Bash(mzn analyze:*), Bash(mzn suggest:*), Bash(mzn doctor:*), Bash(mzn report:*), Bash(command -v mzn)
argument-hint: "[days]"
---

You are coaching the user on reducing the tokens their Bash commands cost in this
project, using the `mzn` CLI (a companion to RTK). mzn only reads local logs.

1. Check that mzn is installed with `command -v mzn`. If it is missing, tell the
   user to install it (`cargo install --git https://github.com/BazaiHassan/mizan`
   or the install script in the mizan README) and stop.
2. Run `mzn doctor`. If it reports errors (RTK missing, RTK not hooked, untrusted
   filters), summarize them first, with the exact fix commands it prints.
3. Run `mzn analyze --days ${ARGUMENTS:-30}` and summarize in a few lines:
   total estimated tokens, share going through RTK, the top 3 "waste" commands,
   and any over-compression re-runs (commands re-run without RTK right after an
   RTK run, which suggests RTK's output was too terse for them).
4. Run `mzn suggest --days ${ARGUMENTS:-30}` (a dry run). If it proposes filters,
   show the filter names and estimated savings, and ask whether to apply them.
   Only if the user agrees, run `mzn suggest --days ${ARGUMENTS:-30} --apply` and
   remind them to run `rtk trust` themselves (RTK ignores changed filter files
   until the user approves them; never run `rtk trust` on their behalf).

Keep the answer short. Token numbers are estimates (characters / 4); say so once.
