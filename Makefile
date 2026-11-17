.PHONY: check
check:
	nix fmt -- --ci
	cargo clippy --workspace --all-targets -- -D warnings
	cargo clippy --all-targets --manifest-path bench/Cargo.toml -- -D warnings
	lake -d lean build --wfail
	lake -d audit build --wfail
	git diff --exit-code -- .leanfran lean
	test -z "$$(git ls-files --others --exclude-standard -- .leanfran lean)"
	tools/with-postgres.sh tests/fixtures/schema.sql tests/fixtures/seed.sql -- cargo test --workspace

.PHONY: bench
bench:
	tools/with-postgres.sh tests/fixtures/schema.sql bench/seed.sql -- cargo run --release --manifest-path bench/Cargo.toml

.PHONY: survey
survey:
	lake -d audit build Survey

.PHONY: format
format:
	nix fmt
