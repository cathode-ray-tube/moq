# Commits

PRs are squash-merged, so the PR title becomes the commit subject and the PR description becomes the body in `git log`.

- Use conventional-commit subjects (`feat(watch): ...`, `fix: ...`, `chore: ...`, `docs: ...`)
- AI commit attribution goes in a `Co-Authored-By:` trailer, not the commit body.
- Never commit binaries or build artifacts (`.a`, `.so`, `.dylib`, `.dll`, wheels).

# PRs

Keep the body short and structured, not narrated.

- **Summary**: a few bullets on what changed and why. For a bug fix, state the root cause.
- **Public API**: every new/renamed/removed/updated exported item, with breaking ones called out.
- **Wire**: any change to the on-the-wire format, and the draft under `drafts/` updated with it.

When pushing additional commits to an existing PR, update the title and description if needed.
When taking over someone else's PR, push commits on top of theirs so they keep credit.

# CI

`Check` and `Test` compile the packages a branch changed and run their unit tests.

Every test suite must run in CI, at least nightly. The Nightly workflow runs
Rust doctests, Loom, drill sensitivity, and all four fuzz targets (five minutes
each); ordinary fuzz regression replay stays in the PR test suite.

# AI

AI-assisted issues, pull requests, reviews, and comments are welcome.
GitHub issues are the public front door for brainstorming. Prefer a quest for work needing durable scope or coordination.

Add the AI marker `(Written by <model>)` to any posts on GitHub, excluding commit messages that contain `Co-Authored-By:` trailers.

# Reviews

Codex and CodeRabbit review every push on their own. Never request a review; an @codex or @coderabbitai mention is banned.
CodeRabbit may be rate-limited, treat it as optional.

Fix the findings you agree with, reply to the ones you do not, and push once.
If the next automatic review still has findings, stop and report to the user.
If a finding is out of scope, make or update a follow-up quest.
