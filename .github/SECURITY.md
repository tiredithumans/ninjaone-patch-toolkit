# Security Policy

## Reporting a vulnerability

**Please do not open a public issue for security vulnerabilities.**

Report privately through GitHub's
[security advisory flow](https://github.com/tiredithumans/ninjaone-patch-toolkit/security/advisories/new),
so a fix can be prepared and released before the issue is disclosed. Please
include reproduction steps and the affected version (the installer filename, or
the version shown in the app) where you can.

We aim to acknowledge a report within a few days and to keep you updated as a
fix is developed and shipped.

## Supported versions

This project is pre-1.0; only the latest released version receives security
fixes. Update to the newest release before reporting.

## Scope

In scope: the desktop app and the two crates in this repository — the OAuth 2.0
+ PKCE authentication / token handling, the NinjaOne API client, OS-keyring
storage of the refresh token and optional client secret, and the local data
handling / Excel export.

Out of scope: vulnerabilities in the NinjaOne platform or API itself (report
those to NinjaOne); and issues that require an already-compromised workstation
or OS keyring.

## How this app handles your credentials

The security model is described in the
[Security section of the README](../README.md#security): access tokens are kept
in memory only; the refresh token and the optional client secret live in the OS
keyring (Keychain / Credential Manager / Secret Service); nothing sensitive is
written to `settings.json`; and the app requests read-only (`monitoring`) scope.
