---
name: milestone
description: How to take a PLAN.md milestone (M0–M6) from spec to merged code using the OxTail subagents. Use when asked to build or continue a milestone.
---

1. Read the milestone in PLAN.md §12 and every section it references. List its
   items as tasks.
2. Define the cross-crate APIs first (types and function signatures, with doc
   comments) so subagents can work in parallel against a stable contract.
3. Delegate each item to the matching subagent (see AGENTS.md "Claude Code
   specifics"), on Sonnet, with a self-contained brief: crate and files it owns,
   PLAN.md sections, the API to implement or consume, tests expected, and the
   check commands. Run independent briefs in parallel; never give two agents the
   same files at the same time.
4. When they return, read the diffs yourself. Then run the `reviewer` agent on
   the milestone diff and fix every verified blocker and should-fix.
5. Run the `verify` skill. Check the milestone's exit criteria in PLAN.md.
6. Update the milestone status table in AGENTS.md and commit with a message
   naming the milestone (e.g. `M2: search and filter views`).
