# AGENTS.md

## Project Context

QsoRipper is a high-performance ham radio logging system focused on speed,
clean workflows, and keyboard-first operation.

Primary goals:

- Fast TUI experience for operators during active radio operation.
- Clean graphical interface for richer workflows.
- Rich operator and station enrichment through QRZ lookups.

## Style Rules

- Never use emojis in code, documentation, comments, or generated copy unless
  the user explicitly asks.
- Never use em dashes. Use regular hyphens, commas, parentheses, or separate
  sentences.
- Keep comments focused on why something exists, not what each line does.

## Development Environment

- This repo is normally worked on from Windows.
- Use PowerShell (`pwsh`) for repository scripts and Windows command examples.
- Use `rg` for text search.
- Do not assume Bash, sh, Python, or a Node toolchain unless the task
  explicitly needs it.
- All code must work on both Windows and Linux. The engine is developed on
  Windows but runs in Linux Docker containers in production.

## Commands

- Build default artifacts:
  `.\build.ps1`
- Build debug artifacts:
  `.\build.ps1 -Configuration Debug`
- Run full local quality checks:
  `.\build.ps1 check`
- Run Rust-only quality checks:
  `.\build.ps1 check-rust`
- Run .NET-only quality checks:
  `.\build.ps1 check-dotnet`
- Run tests without the heavier quality and coverage gates:
  `.\test.ps1`
- Build and then test:
  `.\build-and-test.ps1`
- Run cross-engine conformance:
  `.\tests\Run-EngineConformance.ps1`
- Start local Rust engine:
  `.\start-qsoripper.ps1 -Engine local-rust`
- Start local .NET engine:
  `.\start-qsoripper.ps1 -Engine local-dotnet`
- Stop local engines:
  `.\stop-qsoripper.ps1 -Engine local-rust`
  `.\stop-qsoripper.ps1 -Engine local-dotnet`
- Run Rust workspace commands directly:
  `cd src\rust`
  `cargo build`
  `cargo test`
- Run .NET workspace commands directly:
  `cd src\dotnet`
  `dotnet build QsoRipper.slnx`
  `dotnet test QsoRipper.slnx`
- Restore root Node tooling for UX automation:
  `npm install`

Local Win32 CMake validation uses Visual Studio Build Tools 2026
(`Visual Studio 18 2026`). Do not assume Visual Studio 17 2022 is available.

## Repository Structure

- `proto\`: shared protobuf and gRPC contracts.
- `src\rust\`: Rust workspace, including engine, TUI, launcher, and stress
  tooling.
- `src\dotnet\`: .NET workspace, including engine, GUI, DebugHost, and CLI.
- `src\c\`: native C and Win32 support libraries.
- `shared\`: shared project assets and code where applicable.
- `tests\`: cross-cutting test harnesses and fixtures.
- `scripts\`: PowerShell and TypeScript automation helpers.
- `tools\`: experiments and helper utilities.
- `docs\architecture\`: architecture and engine contract documentation.
- `config\`: checked-in example and default configuration material.
- `data\`: local runtime data and selected checked-in test/reference data.
- `artifacts\`: generated build, run, coverage, and UX outputs. Treat as
  derived.
- `.github\`: GitHub Actions and Copilot-specific adapters.
- `.codex\`: Codex project runtime configuration.
- `.agents\skills\`: reusable agent workflows shared across agent surfaces.

## Engineering Principles

- Prefer Rust or C# for core runtime and performance-critical paths.
- Avoid Python for hot paths and primary services.
- Keep startup and interaction latency low.
- Favor small, composable modules over monoliths.
- In C#, avoid `string.Create(CultureInfo.InvariantCulture, ...)` for
  interpolated strings unless the interpolation includes culture-sensitive
  formatting. Prefer ordinary interpolation for literal separators, string-only
  values, integer zero-padding, and hex identifiers.

## Architecture Direction

- Keep the log engine independent from any specific UI.
- The engine exposes a gRPC API; UX implementations are independent consumers.
- No specific UI technology is required or privileged.
- Keep third-party integrations isolated behind interfaces.
- Make offline logging resilient, even when network integrations fail.
- The architecture is explicitly multi-engine: both Rust and .NET are
  fully-featured, production-grade engine implementations. Any conformant
  implementation in any language can serve as the engine.
- Components communicate via gRPC with Protocol Buffer messages.
- See `docs\architecture\engine-specification.md` for the authoritative engine
  contract.
- When adding new engine features, RPCs, integrations, or behavioral changes,
  update the engine specification in the same change so it stays current.

## Data Model Conventions

- All shared domain types are defined in `proto\` and generated for both Rust
  and C#.
- Proto files are the single source of truth. Never hand-write types that
  should come from proto generation.
- Follow protobuf 1-1-1 by default: one top-level message, enum, or service per
  `.proto` file.
- Every RPC must use unique `XxxRequest` and `XxxResponse` envelopes. Streaming
  RPCs also get unique streamed response envelopes.
- Keep transport-only RPC messages in `proto\services\`, not `proto\domain\`.
- If multiple RPCs need the same payload, extract a separate reusable message
  and wrap it from each response instead of reusing one RPC response envelope as
  another RPC payload.
- Exceptions to 1-1-1 are rare, must be explicit and documented, and never
  justify skipping per-RPC envelopes.
- Use `buf lint` to validate proto files. Use `buf breaking` to guard against
  incompatible schema changes.
- ADIF is for external interchange, such as QRZ API and file I/O, only.
  Internal IPC uses protobuf.
- Keep shared proto messages discoverable in the Debug Host Protobuf Lab; prefer
  auto-discovered message catalogs over hand-maintained UI enums or lists.
- In .NET UI and DebugHost surfaces, do not hand-format generated proto enum
  names with `ToString()` and string replacement. Use shared display helpers
  such as `src\dotnet\QsoRipper.DebugHost\Utilities\ProtoEnumDisplay.cs` so
  labels stay aligned with protobuf original names.
- See `docs\architecture\data-model.md` for full conventions.

## Domain Guidance

- The core entity is the QSO record.
- Standardize canonical fields early: callsign, UTC timestamp, band, mode, RST
  sent/received, operator, locator, and notes.
- Preserve edit history and traceability for log corrections.

## Integration Guidance

- QRZ integration should be isolated from UI code.
- Never hardcode credentials or API keys.
- Use environment variables or secure configuration providers for secrets.
- Integration failures must degrade gracefully and never block local logging.

## UX Rules

- Keyboard-first by default for all high-frequency actions.
- Keep TUI and GUI behavior aligned where practical.
- Prioritize uninterrupted operator flow during contest and pileup scenarios.
- UI work that changes visuals must be verified with the available capture or
  automation workflow for the affected surface when practical.

## Quality and Coverage Gates

- Treat existing CI quality and coverage thresholds as local pre-push
  requirements, not something to discover only after opening a PR.
- When implementing a new feature, behavior change, or new error path, add or
  expand automated tests in the same change so the new code is directly covered.
- Do not rely on existing coverage headroom to carry new code. Meaningful new
  logic should add meaningful test coverage.
- For Rust changes, keep `cargo fmt`, `cargo clippy`, `cargo test`,
  `cargo llvm-cov`, `buf lint`, and `cargo deny` green when those gates apply.
- For .NET changes, keep `dotnet format`, `dotnet build`, and `dotnet test`
  with coverage green when those gates apply.
- Do not push code that you already know will fail an existing quality or
  coverage gate.

## Security and Secrets

- Never commit, print, or log API keys, tokens, connection strings, passwords,
  private keys, QRZ credentials, storage keys, deployment credentials, or local
  `.env` values.
- Runtime secrets belong in environment variables or secure configuration
  providers.
- Local secrets belong in `.env`, which is ignored.
- Error handling must not leak credentials, sensitive request payloads, or
  unnecessary internal details.
- Require explicit confirmation before shutdown, reboot, process termination,
  or destructive file/system actions.

## Cross-Platform Rules

- Use `std::path::Path` and `PathBuf` in Rust for filesystem operations. Never
  hardcode path separators.
- In C code, use only portable POSIX/C standard headers such as `<stdint.h>`,
  `<stddef.h>`, and `<string.h>`. Avoid Windows-specific headers like
  `<windows.h>` unless behind a platform guard.
- Use `#[cfg(target_os = "...")]` in Rust or `#ifdef _WIN32` / `#ifdef
  __linux__` in C only when platform-specific behavior is genuinely unavoidable.
- Prefer portable abstractions.
- Do not assume a specific shell. Build and test commands should work with
  `cargo build` and `cargo test` on any platform where the relevant toolchain is
  installed.
- Test on both Windows and Linux before merging platform-sensitive changes.

## Markdown Code Fences

When writing markdown that will be rendered on GitHub, such as PR descriptions,
issue bodies, review comments, or other repository comments:

- Never use `bash` as the code fence language for Windows commands.
- Backslash path separators like `src\dotnet\QsoRipper.slnx` can render
  incorrectly in GitHub-flavored markdown when labeled as `bash`.
- Use a plain fenced code block with no language tag, or use `powershell` or
  `cmd` instead.
- Prefer Windows-style paths in examples when the command is intended to run on
  Windows.

## Pull Requests

- Before pushing commits to a branch that has an associated PR, check the PR
  status with `gh pr view` first. The PR may have already been merged or closed.
  If so, create a new PR.
- When creating PRs, use a plain text description. Avoid heavy markdown with
  lists, headings, and bullet points.
- When creating feature branches, use `u/<alias>/BranchName` as the convention.
- After creating a PR, do not arm auto-merge by default. The PR requires at
  least one approval before it can merge.
- To opt in to autocomplete behavior, explicitly arm it after creating the PR:
  `gh pr merge --auto --squash`.
- The helper function `New-PR` in `scripts\profile-helpers.ps1`, created via
  `git push -u origin HEAD; gh pr create --base main --fill`, is the default
  one-shot flow. Use `New-AutoPR` only when the user explicitly wants
  autocomplete behavior.
- Squash is the only allowed merge method on `main`. Always pass `--squash` to
  `gh pr merge`.
- Do not click or invoke "Update branch" on PRs. Branch protection no longer
  requires PRs to be up to date with `main` before merging; the merge queue
  handles speculative-merge testing automatically.
- When a PR implements only part of a GitHub issue, do not reference the parent
  issue with a closing keyword such as `Closes`, `Fixes`, or `Resolves`.
  Instead, create a sub-issue scoped to the work in the PR, reference that
  sub-issue with a closing keyword in the PR, and add a comment on the parent
  issue linking to the PR and sub-issue.

## Agent Setup

- This file is the canonical shared instruction file for Codex and GitHub
  Copilot CLI.
- Codex-only runtime settings, hooks, and custom agents belong in `.codex\`.
- Reusable workflows belong in `.agents\skills\`.
- Copilot-specific adapters belong in `.github\`.
- Keep personal MCP auth, credentials, and private defaults outside the repo.
- Do not duplicate durable repository rules across provider-specific files.
  Update this file instead.
