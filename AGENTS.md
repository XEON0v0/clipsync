# ClipSync Agent Instructions

## External development storage

The machine-wide policy in `~/.codex/AGENTS.md` applies first. This workspace
keeps every dependency store, emulator/simulator data set, build, test result,
temporary file and generated intermediate on the external development volume.
The exFAT `UH100` host contains the APFS sparse bundle at
`/Volumes/UH100/ClipSyncDev.sparsebundle`; development data must never be
written directly to exFAT.

Before installing dependencies, building, testing, running Android emulators,
or using Colima, source the environment helper:

```zsh
source scripts/external-dev-env.zsh
[[ "$EXTERNAL_DEV_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_LINKS_READY" == 1 ]]
```

The helper mounts `/Volumes/ClipSyncDev` when possible and configures:

- Android SDK, NDK, and AVD storage
- Gradle user home
- Cargo home, Rustup home, and Cargo target directory
- SwiftPM build and cache directories
- Android project build output directory
- Node, Python, Go, Maven, Kotlin/Native, browser-test and compiler caches
- project-specific test results, temporary data and staged artifacts

`CLIPSYNC_EXTERNAL_READY=1` means the APFS development volume is mounted and
writable, so missing dependencies may be installed there. The link readiness
check verifies that the generated macOS framework also targets the external
volume. If either readiness check is not `1`, stop and report that external
storage setup is unavailable. Do not download replacement SDKs, toolchains,
or dependencies into the internal disk.

The compatibility paths `~/Library/Android/sdk`, `~/.gradle`, `~/.cargo`,
`~/.rustup`, and `~/.colima` point into the mounted APFS volume. Do not replace
those links with local directories. Stop emulators and Colima before ejecting
the drive. Do not detach or compact the sparse bundle while builds, emulators,
or containers are running.

Release output defaults to `$CLIPSYNC_RELEASE_OUTPUT_ROOT` on the external APFS
volume. Copy a finished artifact into the repository `dist/` directory or any
other internal location only when the user explicitly requests that final
destination. Xcode, Homebrew and system SDK stubs may remain in macOS-managed
locations, but their writable output and caches may not.

The user shell intentionally loads `~/.config/external-dev/env.zsh` globally.
The legacy `~/.config/clipsync/external-dev-env.zsh` path is a compatibility
wrapper. Run `scripts/audit-external-dev-env.zsh` after storage changes; the
repository helper adds workspace-specific paths and link checks.

## Environment notes added by the F3 clean-room run (2026-08-06)

- Emulator AVDs live on the external volume (`$ANDROID_AVD_HOME`) and are NOT
  migrated automatically. `scripts/test-android-core.sh` and
  `scripts/spike-android.sh` assume AVDs named `clipsync-spike-api29/34/35`
  (pixel_2, google_apis, arm64-v8a); `spike-android.sh` creates them on demand,
  `test-android-core.sh` does not. After a fresh migration, create them once
  with `avdmanager create avd --name clipsync-spike-api<N> --package
  "system-images;android-<N>;google_apis;arm64-v8a" --device pixel_2`.
- Colima's daemon is configured on the external volume with Docker Hub
  registry mirrors (daocloud/1ms/xuanyuan/dockerproxy) and mounts
  `/Volumes/ClipSyncDev` writable so compose bind-mounts (e.g. the local
  Caddyfile in `live-smoke.sh`) work from external-volume checkouts.
- When Docker builds crawl in China, export
  `CARGO_REGISTRY_CONFIG=deploy/cargo-config.rsproxy.toml` so the pinned
  Dockerfile compiles crates through rsproxy instead of crates.io directly.
