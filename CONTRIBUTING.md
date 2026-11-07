# Contributing to Storecat

Thanks for your interest. The best way to start is to pick an
[open issue](https://github.com/samcarmex/storecat/issues), or open one describing what you
want to change, and say you are on it before writing a lot of code.

## Setup

1. Install [Rust](https://rustup.rs) (stable), Node 20+, and on Windows the
   Visual Studio 2022 C++ build tools. Linux needs the
   [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) (webkit2gtk etc.).
2. `npm install`
3. `npm run tauri dev` starts the app with hot reload. `npm run dev` alone serves the UI
   in a normal browser for design work (Tauri APIs are stubbed there).

## Before you push

```bash
npm run check
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs the same commands on Windows, macOS and Linux and then builds the installers.

**Changing npm dependencies.** Recent npm versions write only the current platform's
native packages (Rollup, esbuild, the Tauri CLI) into `package-lock.json` when
`node_modules` already exists, and `npm ci` then fails on the other platforms. After
changing dependencies, regenerate the lockfile in a folder with no `node_modules`
(copy `package.json` and `apps/desktop/package.json` there and run
`npm install --package-lock-only`), and check it lists `@rollup/rollup-linux-x64-gnu`
and `@rollup/rollup-darwin-arm64`.

## Ground rules

- **Core stays portable.** `crates/core` must compile on Windows, macOS and Linux with no
  OS-specific code. Anything that touches volumes, drive hardware or change journals goes
  in `crates/platform` behind a trait.
- **Nothing phones home.** No telemetry, no network calls except explicit, user-initiated
  downloads (FFmpeg, updates).
- **Licence hygiene.** Dependencies must be GPL-3.0 compatible (MIT, Apache-2.0, BSD,
  MPL-2.0, LGPL, public domain). The unrar licence is not; RAR support is an optional
  external helper only.
- **Speed is a feature.** Cold start under 400 ms and name search under 50 ms over 20 M
  files are budgets, not aspirations. A change that regresses them needs a very good reason.
- **Design tokens live in one place**: `apps/desktop/src/app.css`. Use the variables,
  don't hard-code colours in components.

## Commit messages

Short imperative subject line, optional body explaining why. Reference issues with
`Fixes #123` where applicable.
