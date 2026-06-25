---
name: release
description: Prepare and publish a new release — bump version across the manifests, verify, tag, and push (the release workflow builds the installers). Use when the user says "release", "bump version", or asks to publish a new release.
argument-hint: "[version bump type: patch, minor, major]"
---

# Release — version bump → verify → tag → push

Prepare a released build and publish the tag. Pushing a `v*` tag triggers
`.github/workflows/release.yml`, which builds the platform bundles and uploads them to a **draft**
GitHub release. If arguments were passed, treat them as the bump type (`major` / `minor` / `patch`;
default `patch`).

## 0. Determine the bump type

- If argument given: use it (`patch`, `minor`, or `major`).
- If no argument, look at commits since the last release tag:
  - `feat:` → minor bump.
  - Breaking changes (`!` or `BREAKING CHANGE`) → major bump.
  - Only `fix:` / `chore:` → patch bump.

## 1. Bump the version — keep all three manifests in lockstep

The release workflow's guard step **fails the build** if these drift, so update every one:
- `src-tauri/Cargo.toml` → `[package] version`.
- `src-tauri/tauri.conf.json` → top-level `version`.
- `web-rs/Cargo.toml` → `[package] version`.

After editing, refresh both lockfiles so the version change is recorded:
`cargo update -p ninjaone-patch-toolkit --manifest-path src-tauri/Cargo.toml --precise <X.Y.Z>` (or
just run `just verify`, which touches both).

## 1.5 Roll the changelog

`release.yml` publishes the `CHANGELOG.md` section for the tagged version as the GitHub release
body **and** the updater manifest's `notes` (which the in-app "Update available" window shows), so
the changelog must have a section for this version before tagging:

- In `CHANGELOG.md`, rename the `## [Unreleased]` heading to `## [<X.Y.Z>] - <YYYY-MM-DD>` (today's
  date), and add a fresh empty `## [Unreleased]` above it for the next cycle.
- Make sure the section's bullets describe what shipped (the entries should already be there if they
  were added per-PR; otherwise summarize the `feat`/`fix` commits since the last tag).
- The extractor matches `## [<version>]` exactly, so the heading version must equal the tag (minus
  the `v`). A missing section doesn't fail the build — it falls back to a generic "see GitHub" line —
  but then the in-app update notes are empty, so don't skip this.

## 2. Verify the release build

- Run `just verify` (fmt-check → clippy → test → web-check → web-clippy). Stop on any failure.
- Optionally `just build` to confirm `cargo tauri build` produces bundles locally before tagging.

## 3. Tag and push

- `git checkout main` && `git pull origin main` (ensure up-to-date).
- Commit the bump **and the rolled `CHANGELOG.md`**: `git commit -m "chore(release): v<X.Y.Z>"`.
- Tag: `git tag -a v<X.Y.Z> -m "release v<X.Y.Z>"`.
- Push: `git push origin main --tags`.

The tag push starts `release.yml`. The version-guard step rejects the tag if it doesn't match the
three manifests, so a mismatch fails fast before the long build.

## 4. Finish the GitHub release

`release.yml` creates the release as a **draft** whose notes are this version's `CHANGELOG.md`
section (the same text the in-app update window shows), with the platform bundles attached. Review
the notes, then publish it from the GitHub UI (or
`gh release edit v<X.Y.Z> --draft=false`). Drafts are never "latest", so nothing ships until you
publish.

## Output format

```
release: bumping to v0.1.1 (patch)

✅ verify passed (fmt-check, clippy, test, web-check, web-clippy)
✅ bumped src-tauri/Cargo.toml, src-tauri/tauri.conf.json, web-rs/Cargo.toml → 0.1.1
✅ tagged v0.1.1 and pushed to origin/main — release.yml building bundles

🔗 https://github.com/tiredithumans/ninjaone-patch-toolkit/releases (draft)
```

## Failure handling

- `just verify` fails → stop, report the failing gate's output.
- Conflict on pull → resolve before tagging.
- Tag already exists → report and ask whether to overwrite (never force-push silently).
- Release workflow's version-guard fails → a manifest is out of sync; fix and re-tag.
