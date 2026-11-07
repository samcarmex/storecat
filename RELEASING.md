# Releasing Storecat

## One-time setup

1. **Updater signing key.** Release builds sign their update packages so installed copies
   only accept updates from this project. A key pair was generated outside the repo:

   | File | Use |
   | --- | --- |
   | `%USERPROFILE%\.tauri\storecat-updater.key` | Private key. Keep it safe; never commit it. |
   | `%USERPROFILE%\.tauri\storecat-updater.key.password` | Its password |
   | `%USERPROFILE%\.tauri\storecat-updater.key.pub` | Public key, already in `tauri.conf.json` |

   Add two repository secrets on GitHub (Settings › Secrets and variables › Actions):
   `TAURI_SIGNING_PRIVATE_KEY` with the contents of the `.key` file and
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` with the password. If the key is ever lost, existing
   installs can't auto-update to builds signed with a new one; they need one manual install.

2. **Repository URL.** `tauri.conf.json` (updater endpoint), the winget manifests and a
   few links assume `github.com/samcarmex/storecat`. Change them together if the repo
   lives elsewhere.

3. **GitHub Pages** (optional). Settings › Pages › Deploy from branch `main`, folder `/docs`.

## Each release

1. Bump the version in `Cargo.toml` (workspace), `apps/desktop/src-tauri/tauri.conf.json`,
   `package.json` and `apps/desktop/package.json`, and the `storecat-extract` path
   dependencies. Versions must be plain `MAJOR.MINOR.PATCH` (the MSI format rejects
   pre-release suffixes).
2. Add the release to `CHANGELOG.md`.
3. Commit, then tag and push: `git tag v1.0.0 && git push origin main v1.0.0`.
4. CI builds Windows (NSIS + MSI), macOS (Apple silicon + Intel DMG) and Linux (deb,
   AppImage, rpm), signs the updater packages and creates a **draft** release with
   `latest.json`. Check the draft, then publish it. Installed copies see the update within
   a day.
5. winget: run `.\packaging\winget\update-manifest.ps1 -Version 1.0.0`, check it with
   `winget validate`, then submit with `wingetcreate submit` (opens a pull request to
   `microsoft/winget-pkgs`).

## Code signing (not set up)

Builds are not code-signed. Windows SmartScreen shows "unknown publisher" on first run
until the installer builds reputation, and macOS Gatekeeper requires right-click › Open.
Signing needs a certificate (Windows: an OV/EV certificate or Azure Trusted Signing;
macOS: an Apple Developer ID) and is configured in `tauri.conf.json` and CI secrets.
