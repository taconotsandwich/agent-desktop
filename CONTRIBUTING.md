# Git Rules

This document defines the repository git workflow for `agent-desktop`.

## Branches

- `main` is the stable integration branch.
- Do not commit directly to `main` unless you are doing repository maintenance
  and the change is intentionally small.
- Use branch names in the form `type/short-kebab-description`.
- Use one of these branch types:
  - `feature/*` for user-visible features or new capabilities.
  - `fix/*` for bug fixes.
  - `refactor/*` for internal restructuring without behavior changes.
  - `test/*` for test-only changes.
  - `infra/*` for build, CI, tooling, or repository maintenance.
  - `spike/*` for exploratory work that is not intended to merge as-is.
- Examples:
  - `feature/application-targeting`
  - `fix/kde-screenshot-authorization`
  - `refactor/platform-drivers`
- Generated worktree or automation branch names are fine for local scratch
  work, but rename or recreate them as descriptive topic branches before
  review.
- Do not mix unrelated work in one branch.
- Do not rewrite shared branch history after others have based work on it.

## Commits

- Keep commits focused and reviewable.
- Write commit messages in this form:
  - `type(area): what you did`
- Git-generated merge commits may keep the default form:
  - `Merge branch 'type/short-kebab-description'`
- Use a short, concrete area related to the change:
  - `feat(runtime): expose persistent JavaScript computer use`
  - `fix(kde): authorize screenshots in private seats`
  - `test(farm): verify independent parallel application input`
  - `infra(qa): add desktop container test matrix`
- Avoid vague messages such as `update`, `fix stuff`, or `wip`.
- Do not include emojis in commit messages.
- Every commit must include a human sign-off trailer:
  - `Signed-off-by: Human Name <human@example.com>`
- The signer MUST be a human who has reviewed and approved the commit.
  AI tools, coding agents, LLMs, and bots must never be the signer.
- The human must add the sign-off themselves or explicitly authorize its
  use with their exact name and email. Agents must not invent a sign-off or
  infer approval from the configured Git identity.
- If an AI tool, coding agent, or LLM substantially assisted with
  implementation, review, or testing, add this trailer immediately before
  the sign-off trailer:
  - `Assisted-by: <tool>; model=<provider>/<model>`
- `Assisted-by` records AI assistance and does not replace the required
  human `Signed-off-by` trailer.
- Trailer order is `Assisted-by` first, `Signed-off-by` last.
- Do not commit generated build outputs from `target/`, compiled binaries,
  local editor files, temporary seat state, or QA artifacts such as
  screenshots, logs, and saved test documents.
- Keep reusable QA fixtures and test scripts in the repository; write their
  generated results under `target/qa/`.
- Commit `Cargo.lock` for this repository.
