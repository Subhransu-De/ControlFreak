# Contributing to ControlFreak

ControlFreak targets 64-bit Windows and uses the Rust toolchain pinned in `rust-toolchain.toml`.

## Local checks

Install GNU Make and the Microsoft C++ Build Tools, then run this command from the workspace root before opening a pull request:

```powershell
make check
```

The root `Makefile` runs formatting verification, workspace Clippy with warnings denied, workspace tests, and the release server build in sequence. During development, run individual gates with `make fmt-check`, `make lint`, `make test`, or `make release`. For focused tests, use Cargo directly, for example `cargo test -p controlfreak-core --locked`.

CI also validates generated documentation, dependency policy, workflow syntax, and unsafe-code boundaries.

## Architecture

- `controlfreak-core` owns domain types and focused backend traits.
- `controlfreak-platform` implements Windows capture, desktop, and input behavior.
- `controlfreak-mcp` translates MCP requests and responses.
- `controlfreak-server` wires the production backend to STDIO transport.
- `controlfreak-installer` is a separate setup-only configuration utility. It cannot be invoked
  through the MCP protocol and does not perform desktop control.

Keep Windows API calls inside the platform crate. New unsafe operations belong in the smallest practical module, require a nearby `SAFETY` explanation, and must pass the Hawk and Clippy checks.

## Windows packaging

`make release` builds both the server and the setup helper. The Windows target statically links
the MSVC runtime through `.cargo/config.toml`. Keep installer-only dependencies out of the server:
`toml_edit` and `jsonc-parser` preserve configuration formatting, `tempfile` stages atomic file
updates and unique backups, and `semver` compares installed versions, including prereleases.

Use PowerShell 7 and the checksum-pinned portable Inno Setup compiler:

```powershell
make release
$compiler = ./scripts/install-inno.ps1 -Destination "$env:TEMP/controlfreak-inno"
./scripts/package-windows.ps1 -Compiler $compiler -OutputDirectory target/package-test -TestSetup
./scripts/verify-windows-package.ps1 -PackageDirectory target/package-test `
  -TestDirectory "$env:TEMP/controlfreak-portable-check" -ExpectedVersion 0.1.0-alpha
./scripts/test-windows-installer.ps1 -PackageDirectory target/package-test `
  -TestDirectory "$env:TEMP/controlfreak-installer-check"
```

Use fresh output/test directories for every invocation. Local lifecycle tests require `-TestSetup`,
which gives the installer a separate Windows application identity. The tests create synthetic user
profiles, preserve unrelated fixture files, and restore their process environment afterward. Never
use real MCP client profiles as fixtures. Test output and configuration backups must not be uploaded
as CI artifacts. The production-installer test switch is reserved for disposable GitHub Actions
runners; it exercises the exact release executable before publication.

The release workflow validates the tag before building, runs release-mode tests, builds both binaries
with `cargo-auditable`, and generates CycloneDX SBOMs. `package-windows.ps1 -RequireSbom` requires a
generated `controlfreak.cdx.json` for every crate. Development packages can omit SBOM generation and
contain an explicitly labeled placeholder instead. Each public release contains the setup EXE,
portable ZIP, and two SHA-256 files; package contents include SBOMs. Signing is not configured.

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
