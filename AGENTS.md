# ControlFreak

ControlFreak is a native Windows computer-use MCP server communicating locally over STDIO. It captures screens, performs local OCR, manages existing windows and discoverable virtual desktops, and injects mouse and keyboard input.

Prioritize predictable desktop actions, truthful capability and error reporting, privacy, and responsive local operation. Keep changes focused on the requested outcome. Prefer the smallest design that fits the existing crate boundaries.

Read README.md for supported behavior, CONTRIBUTING.md for contribution requirements, and SECURITY.md before changing desktop control or privilege handling. Use rust-toolchain.toml as the toolchain source of truth. Windows x86-64 with the MSVC build tools is the supported development target.

## Architecture and vocabulary

- crates/controlfreak-core: domain types, validation, capability/permission models, and focused backend traits.
- crates/controlfreak-platform: Windows APIs, capture, OCR, input, privilege checks, desktop arbitration, and native helper implementation.
- crates/controlfreak-mcp: tool schemas, request/result translation, diagnostics, and control-session coordination.
- crates/controlfreak-server: executable options, production wiring, helper entry points, and STDIO startup.

A client is the MCP application calling this server. A backend implements the core traits. An observation reads desktop state; a mutation changes it. A control session is bounded desktop ownership across actions, not a chat conversation.

Keep platform details behind core traits and protocol details in the MCP crate. Follow existing patterns before introducing another abstraction.

## Preserve desktop-control invariants

Read SECURITY.md for the full policy. Preserve elevated-startup opt-in, unlocked Default input-desktop checks, target-integrity checks, and refusals when integrity cannot be verified. Do not make --allow-elevated bypass these checks.

Route mutations through the existing ownership, operation-lease, cancellation, and indicator lifecycle. Observations must not claim a mutating session. Keep bounded holds, end-session behavior, and cleanup after errors or disconnects. Keep input release cleanup and reject ambiguous OCR text clicks before injecting input.

Do not expand the MCP surface into shell, application launch, filesystem, clipboard, registry, network, or elevation tools incidentally. Treat such additions as an explicit product-scope decision. This tool boundary is not a sandbox: keyboard input can still operate powerful applications.

## Complete the tool contract

For a tool change, inspect core request/result types and validation; capability and permission reporting; backend behavior; MCP schema, annotations and dispatch; result/error conversion; and transport tests. Update every affected layer together.

Preserve stable tool names, serialized fields, and structured error meanings unless a contract change is intentional and explained. Report unsupported behavior honestly.

During MCP serving, reserve stdout for protocol traffic and send diagnostics to stderr. Keep standalone CLI output modes and private helper protocols separate from the MCP stream. Never add debug prints to its stdout.

## Rust and native resources

Keep Windows API calls in controlfreak-platform. Confine unsafe operations to the smallest practical scope with a nearby SAFETY explanation covering the actual preconditions and lifetime. Do not weaken lint or Hawk policy to conceal a problem.

Prefer typed errors and existing validation over panics on external input. Keep native handles, helper processes, and operation leases owned through success, cancellation, and failure. Preserve the existing blocking-worker boundary for synchronous platform operations; do not block Tokio workers with Win32/OCR work.

Use Cargo to change dependencies and Cargo.lock. Do not edit the lockfile manually. Respect deny.toml and explain new dependencies.

## Check affected scenarios

Tool coordinates use display-local physical pixels where declared by the schema. Preserve conversion to signed virtual-desktop coordinates, including negative display origins. A resized screenshot is not the input coordinate space: maintain source bounds and image-size metadata for mapping.

For relevant changes, cover multiple displays and DPI scaling, resized/cropped capture, Unicode input, missing or ambiguous OCR matches, vanished windows, unsupported capabilities, privilege refusal, concurrent clients, cancellation, timeout, and helper failure.

State which scenarios apply and which remain unverified. Avoid unbounded waits, image retention, or busy work. Preserve bounded visual baselines and responsive helper shutdown; measure performance-sensitive changes with synthetic data.

## Work safely in the active environment

Use workspace build outputs for development. Do not replace an installed controlfreak executable or change MCP client configuration unless that is part of the requested task.

Track the processes you start and stop only those processes by owned handle or captured PID. Do not kill processes by name or path pattern.

Use synthetic fixtures and mock backends first. For live mutation testing, use a disposable test window or isolated Windows environment and establish the intended target before sending input. Do not use personal applications or real desktop content as test fixtures. If the target or permission to interact with it is unclear, resolve that before injecting input.

## Verify the change

Use the root Makefile for the standard verification commands. During iteration, run the relevant target: make fmt-check, make lint, make test, or make release. For focused behavior tests, use Cargo directly, for example cargo test -p controlfreak-core --locked.

Before opening a PR, run make check from the workspace root on Windows. This runs formatting verification, workspace Clippy with warnings denied, workspace tests, and the release server build in sequence. GNU Make, the pinned Rust toolchain, and MSVC build tools must be available.

Add regression coverage for changed behavior, using existing fake backends and synthetic fixtures where possible. Test outcomes and failure paths rather than reproducing implementation details.

CI also checks documentation with warnings denied, dependency policy, workflow syntax, and Hawk against the Windows target. Consult .github/workflows/ci.yml for the current setup; run applicable specialist checks when changing those areas.

Passing mock or transport tests does not establish real desktop behavior. Report commands, outcomes, and any missing Windows, hardware, or interactive validation. Never call an unrun check passed.

## Documentation and delivery

Update CHANGELOG.md for user-visible changes as required by CONTRIBUTING.md. Update README.md when setup or user-facing behavior changes, and SECURITY.md when its documented boundaries change. Keep explanations of local implementation decisions near the relevant code; avoid duplicate inventories and task diaries.

Keep each change focused, preserve unrelated work, and follow the PR template when a PR is requested. Explain the problem, resulting behavior, verification, and remaining limitations. Check the current PR head's pipeline and review feedback; investigate failures and validate review findings against the actual change before acting.

Keep this file portable. Personal publication preferences, storage paths, agent-model choices, and account-specific integrations belong in user-level instructions. When a task conflicts with documented product or security constraints, identify the conflict before changing the boundary.
