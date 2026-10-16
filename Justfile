set dotenv-load := true
set shell := ["pwsh", "-NoLogo", "-NoProfile", "-Command"]

default: help

help:
	@just --list

fmt:
	cargo fmt --all -- --check

clippy:
	cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

test:
	cargo test --workspace --all-features --all-targets --locked

check: fmt clippy test

# bump VERSION?
# - If VERSION omitted: bump patch automatically
# - If VERSION provided: set exact semver
bump VERSION='':
	@pwsh -ExecutionPolicy Bypass -NoLogo -NoProfile -File scripts\release\bump.ps1 {{VERSION}}

# github-release VERSION?
# Deprecated: fails closed; retry the original isolated Release workflow instead.
github-release VERSION='':
	@pwsh -ExecutionPolicy Bypass -NoLogo -NoProfile -File scripts\release\github_release.ps1 {{VERSION}}

# crates-release
# Deprecated: fails closed; use the isolated Release workflow.
crates-release:
	@pwsh -ExecutionPolicy Bypass -NoLogo -NoProfile -File scripts\release\cargo_publish.ps1

# release VERSION?
# Dispatch the isolated Release workflow for the exact clean dev commit.
# checked version PR -> unprivileged build/package -> fresh publication job
release VERSION='':
	@pwsh -ExecutionPolicy Bypass -NoLogo -NoProfile -File scripts\release\release.ps1 {{VERSION}}
