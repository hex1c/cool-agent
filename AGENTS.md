# Agent Instructions

Optimize for minimum token usage.

Do not narrate your work.

When performing coding tasks:

* Use tools directly.
* Do not restate the user's request.
* Do not explain obvious steps.
* Do not repeat code already written to files.
* Do not print full file contents unless explicitly requested.
* Prefer targeted edits and diffs over full-file rewrites.
* Do not print raw command output unless required to diagnose a failure.
* Keep successful command summaries extremely short.
* Avoid repeating information already present in tool results.

Only communicate during execution when:

* user input is required,
* a blocking problem occurs,
* an important decision cannot safely be inferred.

When the task is complete, respond only with:

CHANGED:

* files or components changed

CHECKS:

* concise test/build results

ISSUES:

* unresolved issues, or "none"

Keep the final response under 150 words unless more detail is explicitly requested.

For Node.js and TypeScript projects:

* Use pnpm exclusively; do not use npm or Yarn.
* Install dependencies from the repository root with
  `pnpm install --frozen-lockfile`.
* Run package-specific scripts with `pnpm --filter <package-name> <script>`.

For Rust projects:

* Prefer the compact Rust/Cargo skill over raw `cargo check`, `cargo test`,
  `cargo build`, and `cargo clippy`.
* Do not inspect full Cargo logs unless the compact diagnostics are insufficient.
* Run the narrowest relevant test first instead of repeatedly running the full
  workspace test suite.
