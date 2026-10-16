# Release process

The primary branch is `dev`. From a clean, synchronized checkout, `just release`
dispatches the Release workflow for that exact GitHub commit. `just release 1.2.3`
requests an explicit numeric version. Dispatch does not mean publication succeeded;
follow the workflow and its version PR.

## Trust boundaries

1. **Prepare (read-only):** determine the version and generate a data-only plan.
   Only the two workspace versions, their internal dependency, and corresponding
   lock entries may change. No dependency resolution or repository hooks run.
2. **Version PR:** a fresh runner checks the plan against the exact source before
   a step-scoped `RELEASE_TOKEN` creates/reuses its version PR. Native auto-merge
   and both required gates must succeed for the exact PR head. The writer runs no
   Cargo build, test, package, or dependency code and stores no Git credentials.
3. **Build (read-only):** a fresh Windows runner checks out the resulting merge,
   runs formatting, Clippy, tests, a release build, and verified workspace
   packaging. It receives no release environment, registry token, or OIDC
   permission. An immutable artifact ID and SHA-256 manifest bind the executable,
   ZIP, optional PDB, and both crate archives to source/merge/version/toolchain.
4. **Publish:** another fresh Windows runner downloads only that run's artifact
   by ID and validates all identities and hashes. Before authentication it
   repackages with `cargo package --no-verify` from an empty directory and fresh
   `CARGO_HOME`, requiring byte-for-byte equality with the verified archives.
   Project Cargo config, credential providers, environment wrappers, build
   outputs, and caches are excluded. GitHub credentials are available only to
   the asset/tag step. The registry token is available only to
   `cargo publish --locked --no-verify` in the same sterile environment; no
   dependency compilation or build scripts run with it.

The publisher's ordinary GitHub token creates the tag/release and uploads assets.
Trusted publishing is preferred for crates.io; the existing
`CARGO_REGISTRY_TOKEN` remains the fallback. No new approval requirement is added.
Actions are pinned to commits. Publication artifacts expire after one day.

## Checks and retries

The version PR is a normal protected PR, never a direct push or an admin bypass.
The release workflow waits for `ci-gate` and `workflow-security-gate` on its exact
head before using its merge SHA. Its squash body records `release-source: SHA`.
Retries reuse the same version PR or matching source tag. If branch push succeeded
before PR creation failed, its parent, release identity and tree must match before
the branch is reused; retries never force-replace it. Multiple mappings,
a closed version PR, altered artifact data, or conflicting tags fail closed.

A release tag never moves. Same-tag asset upload retains the existing
`--clobber` retry behavior. Crates publish core first, then app; index propagation
is checked between packages. An already published version counts as success
only when the registry checksum matches the verified archive. An ambiguous upload
is reconciled against that checksum, not accepted from an error-message string.
Before every upload attempt, individual package regeneration runs without a token
and must match the verified archive, including after core becomes available in
the registry. Cargo regenerates again during upload; source, toolchain and lockfile
are fixed, and the resulting registry checksum is reconciled afterward.

Rerun the **original** Release workflow to retry its original source. A new
dispatch on the version commit is a new source and may request another patch
version. A nightly toolchain change between build and publication causes a safe
failure; rerun from the build stage with one toolchain rather than bypassing the
version check.

## Local commands and validation

`just bump [version]` remains a local version preparation command. It does not
publish. Direct `just github-release` and `just crates-release` commands fail
closed: compilation and publication must use the isolated workflow.

CI tests configuration write failures, release identity/retry rules, and secret
boundaries. Windows CI also verifies real workspace packaging and tokenless
publisher preflight; it never dispatches Release or publishes anything. A stale
lockfile causes a failing diagnostic that prints the candidate workspace-only
lock delta. Review and commit that delta; CI never silently accepts it.

Settings keep the historical `%APPDATA%/RustSwitcher/config.json` path and TOML
format. Saves validate and serialize before writing a same-directory temporary
file, sync its contents, then atomically replace the destination. Missing defaults
are created without overwriting a racing writer. Invalid existing files are
reported unchanged. This prevents truncation on interrupted/failed writes; it is
not a guarantee against power loss, failing storage, or concurrent last-writer
updates. A replacement inherits directory permissions rather than preserving a
custom per-file ACL.
