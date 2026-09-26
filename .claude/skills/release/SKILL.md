---
name: release
description: Prepare and publish a new release — bump version across the manifests, verify, tag, and push (the release workflow builds the installers). Use when the user says "release", "bump version", or asks to publish a new release.
argument-hint: "[version bump type: patch, minor, major]"
---

# Release — version bump → verify → tag → push

Pushing a `v*` tag triggers `.github/workflows/release.yml`: a guard job checks the tag against
the three manifests and the changelog, a verify job runs `just verify` + `just screenshot-test`
on the tagged commit, then the bundles are built and uploaded to a **draft** GitHub release.
The signing-key runbook is `docs/RELEASING.md`; the gate rationale is `docs/design/ci.md`.

## 0. Determine the bump type

- Argument given → use it.
- Otherwise from the commits since the last tag: breaking (`!` / `BREAKING CHANGE`) → major;
  any `feat:` → minor; only `fix:` / `chore:` / `deps:` → patch.

## 1. Bump the three manifests in lockstep

The `versions` CI job and the release guard both fail on drift:
- `src-tauri/Cargo.toml` → `[package] version`
- `src-tauri/tauri.conf.json` → top-level `version`
- `web-rs/Cargo.toml` → `[package] version`

Then refresh both lockfiles so they record the crate's own new version. Any build does it
(`just verify` below is enough); there is no `cargo update --precise` step for a path crate.
Confirm with `git diff --stat` that both `Cargo.lock` files changed.

Regenerate the license notice: `just licenses` (needs `cargo-about`). The CI
`Third-party licenses` job fails a PR whose `THIRD-PARTY-LICENSES.md` is stale, and the
lockfile refresh above can change it.

## 2. Roll the changelog

`release.yml` publishes the `CHANGELOG.md` section for the tagged version as the release body
**and** as the updater manifest's `notes` (what the in-app update window shows):

- Rename `## [Unreleased]` to `## [<X.Y.Z>] - <YYYY-MM-DD>` and add a fresh empty
  `## [Unreleased]` above it.
- The section must be non-empty and its heading must equal the tag minus the `v`; the guard
  job fails the run otherwise.

## 3. Verify

- `just verify`. Stop on any failure.
- `just screenshot-test` (release.yml runs it too; catches a broken capture tool before the
  release exists).
- Optional: `just build` to confirm bundles build locally.

## 4. Land the bump, then tag the merged commit

`main` is protected, so the bump lands through a PR like any other change, and the tag goes on
the commit that CI verified on `main` — not on a local commit that may never merge as-is.

```bash
git checkout main && git pull origin main
git checkout -b release/v<X.Y.Z>
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/tauri.conf.json web-rs/Cargo.toml web-rs/Cargo.lock CHANGELOG.md THIRD-PARTY-LICENSES.md
git commit -m "chore(release): v<X.Y.Z>"
git push -u origin release/v<X.Y.Z>
gh pr create --base main --title "chore(release): v<X.Y.Z>" --body "Version bump and changelog roll for v<X.Y.Z>."
gh pr checks <num> --watch --required
gh pr merge <num> --merge --delete-branch

git checkout main && git pull origin main
git tag -a v<X.Y.Z> -m "release v<X.Y.Z>"
git push origin v<X.Y.Z>
```

Push the one tag by name. `--tags` pushes every local tag, including stale or experimental
ones, and each `v*` tag starts a release run.

## 5. Publish

The release is created as a draft with the bundles and `latest.json` attached. Review the notes,
then publish (`gh release edit v<X.Y.Z> --draft=false`). Drafts are never "latest", so the
auto-updater ships nothing until you do.

## Output format

```
release: bumping to v0.14.0 (minor)

✅ just verify + just screenshot-test passed
✅ bumped src-tauri/Cargo.toml, src-tauri/tauri.conf.json, web-rs/Cargo.toml → 0.14.0 (both lockfiles refreshed)
✅ THIRD-PARTY-LICENSES.md regenerated
✅ CHANGELOG rolled: [Unreleased] → [0.14.0] - 2026-09-01
✅ release PR #<num> merged after required checks
✅ tagged v0.14.0 on main and pushed the tag — release.yml building bundles

🔗 https://github.com/tiredithumans/ninjaone-patch-toolkit/releases (draft)
```

## Failure handling

- Gate fails → stop, report the output.
- Tag already exists (locally or on the remote) → report; never force-move or re-push a tag.
- Guard or verify job fails → the tag is spent. Delete the draft release if one was created,
  land the fix through a PR, and release it as the **next patch version** with a new tag. Never
  move or delete-and-recreate a pushed `v*` tag: the updater and anyone who fetched it have
  already seen it.
