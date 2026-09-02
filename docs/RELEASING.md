# Releasing & update signing — maintainer runbook

How releases are cut is covered by the release skill (bump the three manifests in lockstep,
roll `[Unreleased]` in `CHANGELOG.md`, tag, push — `release.yml` builds and uploads the
bundles). This document covers the part that is **not** in the workflow: the minisign key
that signs auto-updates, and what to do about it.

## How update signing works

- Every release's updater artifacts (`*.tar.gz` / `*.zip` + `.sig`) and the `latest.json`
  manifest are signed with a **minisign private key** during `release.yml`'s build job
  (`tauri-apps/tauri-action`, which reads `TAURI_SIGNING_PRIVATE_KEY` and
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` from GitHub Actions secrets).
- The matching **public key is baked into every shipped binary** via
  `src-tauri/tauri.conf.json` → `plugins.updater.pubkey`, alongside the updater endpoint
  (`.../releases/latest/download/latest.json`). Each installed copy verifies `latest.json`
  and the downloaded artifact against *its own* baked key before installing. Changing
  either the key or the endpoint therefore requires shipping a new release — they cannot
  be rotated server-side.
- `createUpdaterArtifacts` is **off** in the base config, so a local `just build` needs no
  key; the release workflow turns it on with `--config src-tauri/updater-build.json`.

## Key generation (one-time)

```sh
cargo tauri signer generate -w ~/.tauri/ninjaone-patch-toolkit.key
```

Pick a strong password. The command prints the public key; that string goes into
`src-tauri/tauri.conf.json` → `plugins.updater.pubkey`.

## Storage

- **GitHub Actions secrets** (`TAURI_SIGNING_PRIVATE_KEY` = the private-key file's
  contents, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` = its password) are the only place CI
  ever sees the key. Never commit it, never echo it in a workflow.
- Keep an **offline backup** of the private key + password (password manager or sealed
  backup). **Losing the key permanently breaks auto-update for every installed copy** —
  users would have to notice on their own and manually download the next release.

## Succession (the one unrecoverable failure)

Everything else in this repo can be rebuilt from the source tree. The signing key cannot.
If it is lost, **every installed copy is permanently orphaned from updates** — there is no
recovery path, because each install verifies against the public key baked into the binary
it already has. This is a single-maintainer project, so write the answers down here and
keep them current:

- **Who holds the private key and password**, and where the offline backup lives (password
  manager entry, sealed backup, safe). Name the location, not just "a password manager".
- **Who else can reach it** if the maintainer is unavailable — an emergency-access contact
  on the password-manager vault is the least-effort version of this and takes minutes to
  configure.
- **When the backup was last restore-tested.** An untested backup is not a backup. Once a
  year, restore the key file to a scratch directory and confirm `cargo tauri signer sign`
  accepts the password; that is the whole drill.

| | |
|---|---|
| Key holder | *(fill in)* |
| Offline backup location | *(fill in)* |
| Emergency access | *(fill in)* |
| Last restore test | *(fill in)* |

**Known gap:** nothing verifies that the private key in `TAURI_SIGNING_PRIVATE_KEY` still
matches the public key baked into `src-tauri/tauri.conf.json`. If the two ever diverge —
a secret rotated without a transition release, a typo'd paste — the build succeeds, the
release publishes, and every install silently rejects the update; the first signal is a
user reporting that updates stopped. The check is to verify a built artifact's `.sig`
against the baked `pubkey` (Tauri wraps a minisign signature in base64) as a post-build
step in `release.yml`. Not yet implemented, and it should not be added without exercising
it against a real release — a signing check that is itself wrong fails every release.

## Rotation (compromise or precaution)

Rotation must bridge installs that verify with the *old* key. The order matters:

1. Generate a new key pair (command above).
2. Ship a **transition release**: `tauri.conf.json` carries the **new** public key, but
   the GitHub secrets still hold the **old** private key — existing installs verify this
   release with their baked old pubkey and, once updated, trust the new key.
3. After the transition release is published, swap the GitHub secrets to the **new**
   private key + password.
4. Release normally from then on.

Installs that **skip the transition release** can no longer auto-update (signature
mismatch against their old baked key) and need a one-time manual download — say so in the
release notes of the first post-rotation release.

If the old key is known-compromised, also revoke trust socially: a security advisory
naming the last good release, since binaries signed with the stolen key would verify on
any install that never picked up the transition release.

## Release-flow guardrails (already automated)

- `release.yml`'s guard job fails the run if the tag doesn't match all three manifests, or
  if `CHANGELOG.md` has no non-empty section for the tagged version (the section becomes
  the GitHub release body and the in-app update notes).
- Updates only go live when the release is **published** — a draft never ships to users.
- macOS bundles are Apple Silicon (arm64) only and not notarized; see the matrix comment
  in `release.yml`.
