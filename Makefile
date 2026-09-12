.PHONY: check fmt-check lint test release

.NOTPARALLEL:

check: fmt-check lint test release

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets --locked -- -D warnings

test:
	cargo test --workspace --locked

release:
	cargo build -p controlfreak-server --release --locked
