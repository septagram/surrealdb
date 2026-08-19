# customware/003 - AGENTS workflow context

## Context

This fork carries a root `AGENTS.md` file with assistant-facing project context
for working in the SurrealDB codebase. The file is useful fork-local workflow
documentation: it records the project layout, common build/test commands,
language-test conventions, code-quality rules, and bug-investigation protocol.

Upstream `v3.1.5` does not include a root `AGENTS.md`, so this is a fork-local
addition rather than a modification to upstream documentation.

## Decision

Capture `AGENTS.md` as a first-class customware entry instead of leaving it as
an untracked workspace file. This keeps the customware invariant intact: every
fork-local change that rides on `main` has a matching numbered
`customware/NNN-*.md` and `customware/NNN-*.patch` record.

## Implementation plan

1. Add the current root `AGENTS.md` file to the fork.
2. Generate `customware/003-agents-workflow-context.patch` from the `AGENTS.md`
   add, excluding `customware/` itself.
3. Commit both the root file and the customware record before the upstream
   update snapshot branch is created.
4. During future customware updates, replay this entry after code-bearing
   entries unless upstream gains its own root `AGENTS.md`; in that case merge
   the fork-local guidance into the upstream file and regenerate the patch.

## Verification

- Confirm `git apply --check customware/003-agents-workflow-context.patch`
  succeeds on the target upstream release before applying it.
- Confirm the root `AGENTS.md` exists after the customware update finishes.

## Known risks / followups

- If upstream later adds its own root `AGENTS.md`, this patch will become a
  content merge rather than a simple file add.

## Implementation notes (what actually shipped)

- Initial capture adds the root `AGENTS.md` exactly as it existed in the
  workspace before the `v3.1.5` customware update.

## Final commit list

- No separate code commits. The root `AGENTS.md` add lands in the same capture
  commit as this customware record.

## v3.1.5 reapplication notes

- `git apply --check customware/003-agents-workflow-context.patch` succeeded on
  the `v3.1.5` customware chain.
- Reapplied the root `AGENTS.md` file unchanged.
