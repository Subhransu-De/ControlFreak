# Contributing to ControlFreak

ControlFreak targets 64-bit Windows and uses the Rust toolchain pinned in `rust-toolchain.toml`.

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

## Architecture

- `controlfreak-core` owns domain types and focused backend traits.
- `controlfreak-platform` implements Windows capture, desktop, and input behavior.
- `controlfreak-mcp` translates MCP requests and responses.
- `controlfreak-server` wires the production backend to STDIO transport.
- `controlfreak-installer` is a separate setup-only configuration utility. It cannot be invoked
  through the MCP protocol and does not perform desktop control.
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

`make release` builds both the server and the setup helper. The Windows target statically links
the MSVC runtime through `.cargo/config.toml`. Keep installer-only dependencies out of the server:
`toml_edit` and `jsonc-parser` preserve configuration formatting, `tempfile` stages new files
and unique backups, and `semver` compares installed versions, including prereleases.

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

## Publishing a release

Keep changes under `## Unreleased` in `CHANGELOG.md` during development. To release:

1. Merge the intended changes into `main`.
2. Open GitHub Actions, select **Release**, then **Run workflow** on `main`.
3. Enter a semantic version without `v`, such as `0.1.0-alpha` or `0.1.0`.

The workflow updates the workspace version and local dependency requirements in `Cargo.toml`,
fetches the complete locked dependency graph before using Cargo to update workspace versions in
`Cargo.lock`, and moves Unreleased entries into
`## [<version>] - YYYY-MM-DD` using the UTC date. Existing changelog history is preserved.
Versions cannot decrease or reuse an existing release section/tag, and empty releases are refused.
`semver` validates versions and `toml_edit` preserves the manifest layout; both are also used by
the setup helper, and the release preparation commands remain developer-only.

After the tests and package checks pass, the workflow commits those three files and pushes `main`
and the version tag atomically. If `main` advances during the build, the push fails without adding
a tag; run again from the updated `main`. Repository rules must permit the workflow token to push
this release commit. No personal access token is required. The same workflow then publishes the
installer, ZIP, checksums and the version's changelog notes. Alpha/beta/RC versions are prereleases.

Manual tag pushes remain supported for versions whose manifest and dated changelog section have
already been prepared. If publication fails after the tag was pushed, dispatch **Release** against
that existing tag with its matching version using
`gh workflow run release.yml --ref v0.1.0-alpha -f version=0.1.0-alpha` (substitute the version).
This retries the same source without bumping versions or moving tags. An already published release is never overwritten; inspect it before any retry.

The workflow does not create a release until explicitly started or a version tag is pushed.
