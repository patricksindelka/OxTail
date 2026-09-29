@AGENTS.md

## Claude Code notes

- Subagents live in `.claude/agents/`; run development subagents on Sonnet (their
  frontmatter sets `model: sonnet`). Give each one a self-contained brief: the
  crate, the PLAN.md sections, the public API it must expose, and the check commands.
- Before handing work back, run the `verify` skill.
- For large features, first have `reviewer` check the diff, then fix what it finds.
