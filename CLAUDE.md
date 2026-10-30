# Arca: notes for working in this repository

## Build environment (do not probe for toolchains)

- The host is Windows with `git.exe`. It has **no** Rust, Cargo, Android SDK/NDK, and **no** `gh` CLI.
- All compiling, linting, testing and APK packaging happens inside the running Docker container `arca-dev`
  (image `arca-build3`, repository bind-mounted at `/src`, `CARGO_TARGET_DIR=/target`, SDK in `/opt/android-sdk`,
  build-tools dir `android-16` so pass `ARCA_BUILD_TOOLS=android-16`, android.jars in `/opt/jars` via `ARCA_ANDROID_JARS=/opt/jars`).
- From Git Bash:

  ```bash
  MSYS_NO_PATHCONV=1 docker exec arca-dev bash -lc 'cd /src && cargo check --workspace'
  ```

- `cargo fmt` cannot rewrite files on the bind mount (os error 22): copy `crates/` to `/tmp/fmt` in the container, run
  `cargo fmt --all` there, copy changed files back. `cargo fmt --all --check` works in place.
- Heredocs with quotes in the Bash tool sometimes mis-parse; write scripts with the Write tool and run them.
- Fast loop: `cargo check --workspace`. Run the full chain (`tools/build-guest.sh`, `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --locked`, `tools/check-unsafe.sh`,
  `tools/check-malware-lab.sh`, `tools/android/check-android.sh`) once, before the release commit.

## Line endings

`.gitattributes` sets `* text=auto eol=lf` (plus explicit `eol=lf` for `*.sh`, `*.rs`, `*.toml`); `core.autocrlf` is `false`.
The working copy is LF, so `tools/check-unsafe.sh` passes on it. The script also strips `\r` when it reads manifests.

## Releases

- Release secrets (`ARCA_KEYSTORE_B64`, `ARCA_KEYSTORE_PASS`, `ARCA_KEY_ALIAS`) live in the GitHub `release` environment.
  No local keystore password is needed.
- Pushing `main` and then, as a separate command, the annotated tag `vX.Y.Z` starts `.github/workflows/android.yml`; its
  `package-release` job signs with the permanent key and publishes the GitHub release.
- Release notes live in `docs/releases/vX.Y.Z.md`.
