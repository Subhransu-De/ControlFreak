# Contributing to ControlFreak

ControlFreak targets 64-bit Windows and uses the Rust toolchain pinned in `rust-toolchain.toml`.

## Local checks

Install the Microsoft C++ Build Tools, then run these commands from the workspace root before opening a pull request:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build -p controlfreak-server --release --locked
```

CI also validates generated documentation, dependency policy, workflow syntax, and unsafe-code boundaries.

## Architecture

- `controlfreak-core` owns domain types and focused backend traits.
- `controlfreak-platform` implements Windows capture, desktop, and input behavior.
- `controlfreak-mcp` translates MCP requests and responses.
- `controlfreak-server` wires the production backend to STDIO transport.

Keep Windows API calls inside the platform crate. New unsafe operations belong in the smallest practical module, require a nearby `SAFETY` explanation, and must pass the Hawk and Clippy checks.

## Pull requests

Keep each pull request focused, update tests for behavior changes, and update `CHANGELOG.md` for user-visible changes. Do not edit `Cargo.lock` manually; use Cargo commands and commit the resulting lockfile changes.

## License

ControlFreak uses the [MIT License](LICENSE). Contributions are accepted under the same license.
