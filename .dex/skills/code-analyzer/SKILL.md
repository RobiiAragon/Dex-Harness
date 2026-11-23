---
name: code-analyzer
description: Performs static analysis, clippy lint checks, and security audits across the codebase
---

# Code Analyzer Skill

## Objective
Analyze Rust source code to detect anti-patterns, missing error handlers, or unwrap usages.

## Guidelines
1. Execute `cargo clippy --workspace --all-targets -- -D warnings`.
2. Search for any `.unwrap()` or `.expect()` calls in `crates/` using `grep_search`.
3. Provide a structured report with remediation suggestions.
