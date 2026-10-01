# Release process

Releases are automated with [release-please](https://github.com/googleapis/release-please) and the
[`release.yml`](.github/workflows/release.yml) workflow. Nobody tags or pushes images by hand.

## Flow

1. **Use conventional commits.** [`conventional_commits.yml`](.github/workflows/conventional_commits.yml)
   runs `cz check` over every commit in a PR, so each commit message needs a prefix such as `feat:`,
   `fix:` or `feat!:`. The prefix decides the version bump.
2. **Merge to `main`.** The `release-please` job opens or updates a `chore: release main` PR. That PR
   bumps the `Cargo.toml` version, `CHANGELOG.md` and [`.release-please-manifest.json`](.release-please-manifest.json)
   entry of each package that changed.
3. **Merge the release PR.** release-please creates the git tags and GitHub releases, for example
   `graph_tally_escrow_manager-2.2.1`.
4. **Images are built in the same workflow run.** Both images (`graph_tally_aggregator` and
   `graph_tally_escrow_manager`) are built from `docker/Dockerfile.<target>` for `linux/amd64` and
   `linux/arm64`, then merged into one multi-arch manifest at `ghcr.io/graphprotocol/<target>`.

To ship: merge conventional-commit PRs, then merge the release-please PR. The versioned images appear
on GHCR a few minutes later.

## Image tags

| Trigger | Tags |
|---|---|
| Push to `main` that releases the component | `X.Y.Z`, `X.Y`, `X`, `vX.Y.Z`, `vX.Y`, `vX`, `main`, `sha-<short>` |
| Any other push to `main` | `main`, `sha-<short>` |
| PR from a branch in this repo | `pr-<N>`, `sha-<short>` |
| PR from a fork | Built, not pushed |
| `workflow_dispatch` | `<branch>`, `sha-<short>` (never a version tag) |

Both images are built on every run, whether or not their component was released. Only the version
tags depend on the release. `:main` always tracks the head of `main`.
