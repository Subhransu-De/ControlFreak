# Contributing to ControlFreak

ControlFreak targets 64-bit Windows and uses the Rust toolchain pinned in `rust-toolchain.toml`.

Everyone taking part is expected to follow the [code of conduct](CODE_OF_CONDUCT.md).

## Local checks

Install GNU Make and the Microsoft C++ Build Tools, then run this command from the workspace root before opening a pull request:

```powershell
make check
```

The root `Makefile` runs formatting verification, workspace Clippy with warnings denied, workspace tests, and the release server build in sequence. During development, run individual gates with `make fmt-check`, `make lint`, `make test`, or `make release`. For focused tests, use Cargo directly, for example `cargo test -p controlfreak-core --locked`.

CI also validates generated documentation, dependency policy, workflow syntax, and unsafe-code boundaries.

Tests that capture the live desktop are ignored by default. Run the named `live_backend_*`
capture tests and `stdio_capture_display_returns_png_image_content` with `--ignored` only
in an explicitly approved disposable desktop. The default suite uses synthetic data and hidden
fixture windows for target-policy coverage and never needs a personal application as a fixture.

The live end-to-end suites drive the real server against disposable fixture windows. They run in CI on every pull request and on every push to `main`. To run them yourself, use a disposable desktop only, set `CONTROLFREAK_LIVE_DESKTOP=1` and run `cargo xtask e2e --strict`. [docs/scene-e2e.md](docs/scene-e2e.md) describes each scene and task. After CI finishes, a bot comment on the pull request summarizes the workspace tests and the e2e run.

## Architecture

- `controlfreak-core` owns domain types and focused backend traits.
- `controlfreak-platform` implements Windows capture, desktop, and input behavior.
- `controlfreak-mcp` translates MCP requests and responses.
- `controlfreak-server` wires the production backend to STDIO transport.
- `controlfreak-installer` is a separate setup-only configuration utility. It cannot be invoked
  through the MCP protocol and does not perform desktop control.
- `controlfreak-e2e` holds the live end-to-end suites and renders the pull request test report.
  `controlfreak-live-tests` holds the disposable fixture window they drive. Neither is shipped.
- `xtask` is developer-only Rust automation for packaging and release verification. It is never
  installed with ControlFreak or called by the application.

Keep Windows API calls inside the platform crate. New unsafe operations belong in the smallest practical module, require a nearby `SAFETY` explanation, and must pass the Hawk and Clippy checks.

For a new tool, keep its domain request, result, and validation in `controlfreak-core`.
In `controlfreak-mcp`, `requests.rs` decodes arguments, `schema.rs` owns the published contract,
`handlers.rs` runs backend operations on blocking workers, and `results.rs` translates outcomes.
`lib.rs` owns transport and dispatch, `session.rs` owns control and indicator lifetimes, and
`diagnostics.rs` records content-free operation summaries. The public indicator API is re-exported
from the crate root. In the Windows backend, `actions.rs` owns pointer and keyboard execution
and shared target checks; `ocr.rs` owns the isolated OCR process protocol and cleanup.

Keep contract and handler tests in the MCP crate, native behavior tests beside their platform
implementation, and lifecycle regressions in the session and cleanup tests. Extend
`examples/recipes.json` and its synthetic transport test when a shipped contract changes a recipe.
See [examples/README.md](examples/README.md) for the runner and generated contract export.
The domain traits support test doubles; only Windows has a production desktop backend.

## Windows packaging

`make release` builds both the server and the setup helper. The Windows target statically links the MSVC runtime through `.cargo/config.toml`. Keep installer-only dependencies out of the server: `semver` compares installed versions (including prereleases) and `serde_json` parses `codex mcp get --json` output without editing third-party config files directly.

Run the Rust packaging tool through the workspace's `cargo xtask` alias. Cargo builds it on demand;
there is no separate script runner to install. Compiler downloads use Windows' built-in `curl.exe`.

```powershell
make release
cargo xtask compiler --output target/inno
cargo xtask package --compiler target/inno/compiler/ISCC.exe --output target/package-test --test-setup
cargo xtask verify --package target/package-test --output target/portable-check
cargo xtask test-installer --package target/package-test --output target/installer-check
```

Use fresh output/test directories for every invocation. Local lifecycle tests require `--test-setup`,
which gives the installer a separate Windows application identity. The tests create synthetic user
profiles and preserve unrelated fixture files. Environment overrides apply only to child processes. Never
use real MCP client profiles as fixtures. Test output and configuration backups must not be uploaded
as CI artifacts. The production-installer test switch is reserved for disposable GitHub Actions
runners; it exercises the exact release executable before publication.

The release workflow validates the version and changelog before building, runs release-mode tests, builds both binaries
with `cargo-auditable`, and generates CycloneDX SBOMs. `cargo xtask package --require-sbom` requires a
generated `controlfreak.cdx.json` for every product crate (excluding `xtask`). Development packages can omit SBOM generation and
contain an explicitly labeled placeholder instead. Each public release contains the setup EXE,
portable ZIP, and two SHA-256 files; package contents include SBOMs. Signing is not configured.

The automation uses `zip` for archives, `sha2` for checksums, and `clap` for typed command arguments.
Its `packaging-tests` platform feature exposes native version-resource, registry and fixture-permission
checks only to developer tooling. These dependencies and checks are not part of the shipped server.
Run `cargo test -p xtask --locked` for its focused tests. CI uses the same commands shown above;
`cargo xtask publish` is restricted to the GitHub Actions release workflow and verifies that the
remote tag points to the built commit. Release notes come from the matching changelog section.

## Releasing

Keep changes under `## Unreleased` in `CHANGELOG.md` during development. A ruleset protects `main`: every change, including a release, reaches it through a pull request with passing CI checks. The release workflow never pushes to `main`.

1. In the Actions tab, open the **Release** workflow and use **Run workflow** on `main`. Enter a version such as `0.3.0` or `0.3.0-rc.1`, or leave it empty for the next patch version (or the release of a prerelease, for example `0.3.0-rc.1` to `0.3.0`). CI must have passed on that `main` commit. The workflow updates the version, moves the Unreleased entries into `## [<version>] - YYYY-MM-DD` (UTC date), pushes the `release/vX.Y.Z` branch and opens the `chore: release vX.Y.Z` pull request with the release notes.
2. Close and reopen that pull request to start CI. Pull requests opened by the workflow token do not start workflows, and the ruleset blocks the merge until CI passes.
3. When CI passes, squash merge the pull request and keep its title.
4. When CI passes on `main` for the merged commit, tag that commit and push the tag: `git fetch origin`, `git tag vX.Y.Z <merged commit>`, `git push origin vX.Y.Z`.
5. The tag starts the **Release** workflow. The build job requires a passing CI run on `main` for the tagged commit and a tag that matches the workspace version. It runs release-mode tests, builds, verifies and attests the package, and writes the release notes to the run summary. The publish job then waits on the `release` environment. Read the summary and use **Review deployments** to approve or reject it. Approval publishes the GitHub release; stable releases are then submitted to winget. Rejecting it publishes nothing and keeps the tag, so you can publish later by running the workflow on that tag.

To stop a release before step 4, close its pull request and delete its branch. Do the same and start again if the pull request conflicts with `main`. Tick **Dry run** to build and verify on any branch without opening a pull request, tagging or publishing.

If publication fails, use **Re-run failed jobs**, or run the workflow on the existing tag: `gh workflow run release.yml --ref vX.Y.Z`. This retries the same source without changing versions or tags. A published release is never overwritten; inspect it before any retry. Release commits that the earlier workflow pushed to `main`, up to v0.2.3, have no CI run, so the workflow refuses to retry their tags.

`cargo xtask prepare-release` updates the workspace version and local dependency requirements in `Cargo.toml`, fetches the complete locked dependency graph before using Cargo to update workspace versions in `Cargo.lock`, and preserves the changelog history. Versions cannot decrease or reuse an existing release section or tag, and empty releases are refused. `semver` validates versions and `toml_edit` preserves the manifest layout for release preparation; the release preparation commands remain developer-only. Alpha, beta and RC versions are prereleases.

The `release` environment must exist with the maintainer as a required reviewer and deployments limited to `v*` tags. Publication only runs on tags, so `main` does not need to be allowed. Without the environment, GitHub creates an unprotected one and the release publishes without approval. The repository setting **Allow GitHub Actions to create and approve pull requests** (Settings, Actions, General) must be on, or the workflow pushes the release branch but cannot open its pull request. The ruleset requires no approvals, so the setting lets Actions skip no review. Publishing needs no personal access token.

After a stable release is published, a separate job uses a pinned, checksum-verified Komac to open a pull request in [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) for `SubhransuDe.ControlFreak`. It needs a `WINGET_TOKEN` repository secret: a classic personal access token with the `public_repo` and `workflow` scopes, whose owner has a fork of winget-pkgs. Without the secret the job only records a notice, and prereleases are never submitted. Komac can only add a version to a package that winget-pkgs already lists, so until the first submission is merged there the job also skips with a notice instead of failing; it submits by itself once the package is listed. If only this job fails, use **Re-run failed jobs** on the same workflow run.

Automated package checks make only version/capability and MCP initialize, tools/list and server-status
requests. They do not capture desktop content or inject input. An elevated runner additionally checks
default startup refusal before explicit opt-in for these harmless requests. Test a non-elevated clean
Windows 10/11 environment and the interactive wizard before declaring release compatibility; CI alone
does not establish OCR language-pack, desktop-helper, multi-monitor, or physical input behavior.

Client configuration references:
[Codex](https://developers.openai.com/codex/mcp/),
[Claude Code](https://code.claude.com/docs/en/mcp),
[Claude Desktop](https://modelcontextprotocol.io/docs/develop/connect-local-servers),
[Pi adapter](https://github.com/nicobailon/pi-mcp-adapter), and
[OpenCode](https://opencode.ai/docs/config/).

## Pull requests

Keep each pull request focused, update tests for behavior changes, and update `CHANGELOG.md` for user-visible changes. Do not edit `Cargo.lock` manually; use Cargo commands and commit the resulting lockfile changes.

## License

ControlFreak uses the [MIT License](LICENSE). Contributions are accepted under the same license.
