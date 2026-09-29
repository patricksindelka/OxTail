---
name: verify
description: Run OxTail's full check suite (format, clippy, tests, dependency allowlist) and report results. Use before committing or handing work back.
---

Run from the repository root, in this order, and stop to fix problems at each step:

1. `cargo fmt --all` then `cargo fmt --all --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace`
4. `cargo run -q -p xtask -- check-deps` (skip if xtask does not exist yet)

Report each command with pass/fail and, for failures, the relevant error lines.
Never report a step as passing without having run it. Flaky tests are real
failures: investigate them, don't re-run until green.
