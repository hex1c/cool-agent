# Branch & Review Strategy

## Protected branches

| Branch   | Role                          | Default? |
|----------|-------------------------------|----------|
| `prod`   | Production release line       | no       |
| `staging`| Pre-prod integration line     | no       |
| `dev`    | Integration / default branch  | **yes**  |

`main` is removed once `dev` is the default.

## Flow

1. Every fix / feature / enhancement lands via a PR into `dev`.
2. `dev` → `staging` promotion: PR or fast-forward when `dev` is green.
3. `staging` → `prod` promotion: PR when staging is verified.
4. **Hotfix to `prod`**: PR directly into `prod`. After merge, an automated
   down-merge carries the same change into `staging` and then `dev`
   (`.github/workflows/down-merge.yml`).

## Protection (applied via `gh api`)

All three branches:

- Require PR review (min 1) before merge.
- Require status checks (CI) to pass before merge.
- Require branches to be up to date before merge.
- No direct pushes; no force-push; no deletion.
- `prod` additionally: require linear history + code-owner review recommended.

## Grouping of existing commits into reviewable PRs

The 77 commits are already linear on the old `main`. To turn them into
*real* reviewable PRs the history must be rewritten (Option B below); to
keep history intact the grouping is a retrospective record (Option A).

### Proposed PR groups (by phase / task)

| # | PR title | Task(s) | Commit range (oldest→newest) |
| --- | ---------- | --------- | ------------------------------ |
| 1 | `feat: project foundation and Phase 0 feasibility` | init, 1–8 | `9c40ce2`, `fe08311` |
| 2 | `feat: audited workflow state machine and authorization` | 14–16 | `f365ad9` `4770cdf` `9ff4540` `b997479` `958cccd` |
| 3 | `feat: topic binding, membership cache, corrections auth` | 14, 16 | `2b646a9` `84efde6` `cbbd0ce` `be7a6ab` `809deae` |
| 4 | `feat: idempotency and external-operation retry policy` | 17 | `46da9cc` `649900b` `b323efa` `d118897` |
| 5 | `feat: parametric budget decision policy with reserve` | 18 | `9f4d62e` `9bf1038` `56a94ef` |
| 6 | `feat: persistence ports and DynamoDB workflow storage` | 19–20 | `ea9e8ac` `28dd4cf` `a8e66b2` `66e2df6` `f641f28` `1f16226` `f8fcfaf` `f3bde7c` |
| 7 | `feat: object, history, and secret storage with S3` | 21 | `1231a5d` `b67448b` `6a17e5d` `d6efa66` `7099808` `6c22e88` `b05d3a7` `21a2357` `d0d0019` `094b326` `3c6248f` `bfe2f06` `e6d9d7c` |
| 8 | `feat: producer-owned sanitized history with secret-provenance hardening` | 21 hardening | `5f48f3e` `cd68dfe` `55b4c29` `1d6cd3c` `194b34d` `6e87622` `4624ef4` `7a09ced` `42186d3` `e0ca5a8` `8f5f7bb` `c54b919` `6813b17` `d9a65ec` |
| 9 | `feat(telegram,oauth): webhook, topic commands, OAuth onboarding, Lambda wiring` | 22–25 | `3514675` `67acaac` `c9a33d2` `f9a195f` `4517b9c` |
| 10 | `feat(application,agent-harness): secure intake, AI extraction, resumable confirmation` | 26–31 | `36d536a` `2157bb0` `c3a6bc6` `0caef92` `a1f0de0` `9618683` `36ac365` `622ce41` |
| 11 | `feat(google,pdf): Drive/Sheets/Docs creation, quotation renderer, existing-file mutation` | 32, 33, 35 | `f7f1aaa` `564b8a2` `3715638` `3ee2fa7` `9b9bb3f` `f5ae1ac` `c5a2d77` `c21cca9` `d66c543` `2e7eff2` |

11 PRs total. Each maps to one checkpoint in `tasks/plan.md`.

## Options for landing these PRs

**Option A — non-destructive (recommended).** Keep the existing linear
history. `prod`/`staging`/`dev` all start at current `main` HEAD. The
table above is recorded here as the retrospective review record. New work
follows the PR flow going forward.

**Option B — history rewrite.** Reset `dev` to the initial commit, create
11 feature branches by cherry-picking each group, open 11 PRs against
`dev`, merge in order. This force-pushes and rewrites all 77 commits.
Destructive and irreversible; only worth it if you want the git history
itself to be PR-shaped.
