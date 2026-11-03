---
name: coomi-custom-iteration
description: Develop Coomi in a private fork, use GitHub Actions to build an isolated CoomiDev APK, submit a PR, or diagnose the optional mobile ARM64 Build Kit.
keywords: [coomi, custom, iteration, github, github-actions, actions, git, fork, clone, pull request, apk, coomidev, prootlinux]
file_types: [rs, vue, ts, java, xml, gradle, md]
tool_requirements: [shell, local_shell, git, gh, github_actions, cargo, npm, gradle]
project_types: [coomi, rust, android, vue]
risks: [network, destructive]
---

# Coomi Custom Iteration

Use the Runtime V2 Debian ProotLinux guest and work only in `~/custom_coomi`.
Never modify or push the official repository's `main` branch directly.

## Directory architecture

- Shell commands run in the ProotLinux guest. `/workspace` is the active Android-host workspace mount and `/home/coomi` is the persistent guest home.
- Use `/home/coomi/custom_coomi` (equivalent to `~/custom_coomi`) for the source checkout. Do not clone into the engine configuration directory.
- `/opt/coomi-dev` is the persistent CoomiDev Build Kit mount. Its `current/` directory is the selected immutable toolchain, `bin/` contains Coomi-owned helpers, and `cache/`, `state/`, `logs/`, and `keys/` persist independently from the source checkout.
- The host Coomi engine home contains `config/`, `skills/`, `sessions/`, `memory/`, `cache/`, `tasks/`, and `runtime-v2/`. Skill discovery reads host `skills/`; MCP definitions read host `config/mcp_servers.json`.
- MCP stdio servers are host-engine processes unless their MCP configuration explicitly launches them through ProotLinux. Do not assume an MCP process shares the guest shell's `HOME` or `PATH`.
- File tools and APK export require Android-host absolute paths. Shell commands use guest paths; `/workspace/<path>` maps to the active host workspace and `/home/coomi/<path>` maps to the Runtime V2 persistent home.

For a Coomi source checkout, preserve these ownership boundaries:

- `apps/coomi-app`: Android shell, dashboard, service lifecycle, APK assets and Gradle packaging.
- `apps/coomi-rs`: Rust engine, tools, Providers, Skills/MCP catalogs, Runtime V2 and local Web API.
- `apps/web`: Vue conversation UI and console pages.
- `runtime-v2-dist`: pinned offline ARM64 ProotLinux artifacts and manifest.
- `assets`: shared product/developer artwork.
- `references`: pinned third-party bootstrap and reference payloads.
- `build/`, `target/`, `node_modules/` and generated APK assets are outputs, not source ownership areas.

## Before editing

1. Confirm `COOMI_RUNTIME_BACKEND=proot_linux`, `git`, and `gh` are available. GitHub Actions is the default APK build route; the phone is an editing and orchestration environment, not the first choice for Android toolchain execution.
2. Confirm GitHub authentication with `gh auth status`; never print, paste, or save a token in chat, logs, source files, or commits. The account must have permission to dispatch workflows and download artifacts in the user's fork.
3. Inspect `git status`, the current branch, project rules, and available disk space. Keep at least 6 GiB free for local tests; Actions runners provide the Android build disk space.
4. Use a feature branch such as `codex/custom-<short-description>` based on the user's request. Keep the fork's `main` and the official `TensorHub-ORG/Coomi-Android:main` untouched.
5. Only run `coomidev-build doctor` before a local build attempt or when diagnosing the optional local Build Kit. Do not delay normal GitHub Actions work because the phone does not have ARM64 Android compilers.

## ARM64 Build Kit invariants

- Use only `/opt/coomi-dev/current` for build-critical tools. A selected kit must have `buildkit.json`, pinned versions, and verified SHA-256 entries. Do not install or update critical build tools with `apt` or `dpkg`.
- Install a trusted offline kit with `coomidev-install-buildkit <archive.tar.gz> <expected-sha256>`; the expected digest must come from a trusted release channel, not from the downloaded archive itself.
- Never run Termux Android-PIE/Bionic executables from `/data/data/.../files/usr` inside the Debian/glibc guest. Do not copy them into `/usr/bin` or the Build Kit.
- Do not assume an official Android SDK or NDK Linux archive supports an ARM64 host. Most `aapt2`, `clang`, and `lld` host binaries are x86_64. Inspect every native executable with `file` and `readelf -h`, including its ELF interpreter/loader, before using it.
- The complete kit needs a Linux AArch64 JDK, Node/npm, Rust host plus `aarch64-linux-android` target, ARM64-host Clang/LLD, Android NDK sysroot, ARM64 aapt2, d8/R8 and apksigner JAR launchers, and zipalign or a verified compatible implementation.
- Signing material belongs under `/opt/coomi-dev/keys`. Never place a private key in the repository, tool output, logs, or conversation.
- Limit Gradle and Cargo concurrency to two workers and keep Gradle heap near 1536 MiB unless device resources justify a different verified limit.

The repository's supported commands and Build Kit layout are in `tools/mobile-build/README.md`. Use `coomidev-build`; do not recreate the build pipeline ad hoc.

## Linux migration rules

Before attempting a mobile build, remove Windows-only assumptions from the affected build path:

- Invoke `npm`, `cargo`, and other tools through configurable commands. Never hardcode `npm.cmd`, `.cmd` suffixes, drive letters, backslashes, or `windows-x86_64`.
- Use POSIX shell syntax and forward-slash paths. Keep Windows support through host detection and environment overrides rather than separate divergent build files.
- For Linux ARM64, require explicit `COOMI_NDK_HOME`, `COOMI_NDK_TOOLCHAIN_DIR`, `COOMI_ANDROID_CLANG`, `COOMI_ANDROID_AR`, and `COOMI_ANDROID_RANLIB` values supplied by the verified Build Kit.
- Pass the ARM64-host aapt2 path with `-Pandroid.aapt2FromMavenOverride=...`. Do not let Gradle silently download and execute an x86_64 aapt2.

## GitHub setup

Guide the user through `gh auth login` using the device-code flow. Generate an ed25519 SSH key only when needed, show the public key for the user to add to GitHub, and verify with `ssh -T git@github.com`. Prefer SSH remotes. A Personal Access Token (classic) is a fallback for HTTPS/API operations; explain minimum permissions and never expose it.

## Verification

For repository verification, check the fork, `main` branch, remotes, and clean/expected worktree. Star and fork actions require explicit user confirmation before any write operation. Clone the official source or the user's fork into `~/custom_coomi` only after confirming the destination.

## PR delivery

Before pushing, summarize changed files and run the smallest relevant Rust tests, frontend build/type checks, and Android checks. Ask for confirmation immediately before commit, push, or PR creation. Use a PR body with Summary, Changes, Testing, Compatibility, Screenshots, and Risks. The target is `TensorHub-ORG/Coomi-Android:main`, and the head should be the user's fork feature branch.

## GitHub Actions CoomiDev delivery (default)

Use this route first because Android's official Linux host tools are generally x86_64 while the phone's guest is ARM64. The local ProotLinux environment can edit and test the checkout, but it must not pretend to be a complete Android build host.

1. Ensure the user's fork contains the current feature branch and a reviewed workflow under `.github/workflows/`. The workflow must use a pinned Ubuntu runner/tool versions, checkout the requested branch, set `COOMI_DEV_BUILD=1`, and build `CoomiDev` with package `com.coomidev.android`, port `18765`, and `assets/coomi-agent-dev.png`.
2. Keep build and release concerns separate: one job may run `doctor`, frontend/Rust checks, and APK packaging; a later release job may upload only the verified APK artifact. Do not put secrets, signing passwords, or private keys in workflow YAML or repository files.
3. Before dispatch, inspect the workflow inputs and permissions. Prefer `contents: read` for build-only jobs; request `contents: write` or release permissions only when the user explicitly asks to publish a release.
4. Trigger and observe the run from the guest:
   ```sh
   gh workflow list
   gh workflow run <workflow.yml> --ref codex/custom-<short-description>
   gh run list --workflow <workflow.yml> --branch codex/custom-<short-description> --limit 5
   gh run watch <run-id> --exit-status
   ```
5. On failure, retrieve only the failed-step logs with `gh run view <run-id> --log-failed`. Classify the failure as source, dependency, architecture, signing, cache, or workflow permission; fix the smallest cause and dispatch a new run. Never loop-retry an unchanged failing run.
6. Download artifacts only after `gh run watch` exits successfully: `gh run download <run-id> --name <artifact> --dir /home/coomi/CoomiDev-output`. Verify APK package, version, ABI, application label, default port, icon and signature before showing the path to the user.
7. If a PR is requested, keep the successful run URL and test summary in the PR body. Ask for explicit confirmation immediately before `git push`, PR creation, release publication, or any fork write operation.

### Workflow implementation checklist

- Use `workflow_dispatch` inputs for the source ref, whether to upload an APK, and an optional release tag. Keep the default ref as the current feature branch so a stale `main` build cannot be mistaken for the user's iteration.
- Use a fixed `ubuntu-24.04` runner with pinned setup actions. Set up JDK 17, Node/npm, Rust 1.95.0 plus `aarch64-linux-android`, and the Android SDK/NDK versions declared by the repository. Cache Gradle, Cargo and npm by lockfile; never cache signing keys or provider credentials.
- Run `npm run type-check` / `npm run build`, the smallest relevant Rust tests, then `COOMI_DEV_BUILD=1 ./gradlew --no-daemon --max-workers=2 :app:assembleRelease`. Keep the checks before packaging so a failed test cannot produce a seemingly valid artifact.
- Inject release signing through GitHub encrypted secrets or a protected fork environment. The workflow must fail when signing values are missing; never generate or print a private key in a log. For a preview build, use the repository's explicitly documented debug key and label the artifact as a preview.
- After packaging, use `aapt2 dump badging` or `apkanalyzer` to check `com.coomidev.android`, `CoomiDev`, `arm64-v8a`, and the expected version. Run `apksigner verify --verbose`, then upload a single named artifact such as `coomidev-apk` together with checksums and the run URL.
- Keep an optional release job behind an explicit input and environment approval. It may create a GitHub Release in the user's fork, but it must never push to or publish from `TensorHub-ORG/Coomi-Android` without an explicitly confirmed release operation.

## CoomiDev delivery (local fallback)

Use this route only when the user explicitly requests a local APK or GitHub Actions is unavailable. Validate in this order: `coomidev-build doctor`, `coomidev-build android-smoke`, `coomidev-build rust-smoke`, then `coomidev-build full`. Do not claim on-device build support is complete until all four stages pass.

The final build must set `COOMI_DEV_BUILD=1` and use application name `CoomiDev`, package `com.coomidev.android`, isolated app storage/runtime, default engine port `18765`, and bundled icon `assets/coomi-agent-dev.png`. Verify package identity and signature before exporting to `~/CoomiDev-output`; never replace or update `com.coomi.android`.

If a required ARM64 artifact cannot be sourced and checksum-verified, stop the local build and return to the GitHub Actions route. Do not bypass architecture, loader, checksum, package identity, or signature checks.

## Conflict and safety rules

Project rules and direct user requirements take precedence. If this Skill conflicts with either, report the conflict and pause the conflicting operation. Do not run destructive cleanup, force push, reset, or delete a worktree without explicit confirmation.
