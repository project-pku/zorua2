# Zorua Workflow

This repository uses a protected `main` branch and pull requests for all normal
changes. Repository bootstrap may require temporary admin setup; after this
workflow lands, normal changes follow the rules below.

## Branches

- `main` is the canonical branch. Do not push feature work directly to it.
- Work branches use lowercase kebab-case names:
  - `feat/<topic>` for new API or behavior.
  - `fix/<topic>` for bug fixes.
  - `ref/<topic>` for refactors that should not change behavior.
  - `docs/<topic>` for documentation-only changes.
  - `test/<topic>` for test-only changes.
  - `meta/<topic>` for release, version, or repository-maintenance changes.
- Delete work branches after their pull request is merged.

## Pull Requests

- Every normal change lands through a pull request into `main`.
- Keep pull requests focused on one coherent change.
- Prefer small follow-up pull requests over mixing unrelated cleanup into a
  feature or fix.
- PR titles use the same prefix vocabulary as commit subjects:
  - `[feat] Add endian-aware storage helper`
  - `[fix] Reject invalid bitfield layout`
  - `[ref] Simplify bit codec bounds`
  - `[docs] Document layout assertions`
  - `[test] Cover signed endian codecs`
  - `[meta] Bump version to v0.12.3`
- PR bodies should explain the behavioral/API impact, important tradeoffs, and
  the verification that was run.

## Merging

- Only `prof64` merges pull requests.
- Automated agents may create branches, push commits, open PRs, and respond to
  review feedback, but must not merge PRs or push directly to `main`.
- Use squash merge for normal pull requests.
- The squash commit subject should match the final PR title.
- Include co-author trailers for substantive AI-agent work, for example:

  ```text
  Co-authored-by: OpenAI Codex <codex@openai.com>
  ```

## Verification

Before a PR is ready to merge, run the checks relevant to its scope. For normal
code changes, that means:

```text
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo check --no-default-features
```

If a check is skipped, explain why in the PR.
