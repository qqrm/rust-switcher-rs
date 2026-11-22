# WinGet publishing

The release workflow submits the already-published `rust-switcher-setup.exe` to the community WinGet repository in a separate, read-only-permissions job after GitHub Release and crate publication succeed. It does not build another binary. Historical releases without an installer retain their portable-EXE manifests. This direct invocation is necessary because publishing a release with `GITHUB_TOKEN` does not trigger another release-event workflow. The standalone release-event workflow covers releases published by a user or another credential.

## Installer and portable migration

`scripts/build-installer.ps1` builds a per-user Inno Setup 6 installer and writes
the production manifest set to its output directory's `winget` subdirectory.
The default version comes from Cargo; `-Version` is available for isolated
acceptance fixtures without editing package versions. Release bundles require
both the installer and standalone EXE, and bind their hashes to the checked
source alongside the existing ZIP and crate archives.

The new manifest uses `InstallerType: inno`, `Scope: user` and
`UpgradeBehavior: install`. Its Apps and Features declarations accept both the
new `QQRM.RustSwitcher_is1` registration and the old official portable product
code. This permits WinGet to select the installer for an existing portable
package without attempting to uninstall the running EXE first.

The installer validates the old registration's package identifier, official
source identifier, installer type, installation directory and executable path.
It migrates in place, retaining the WinGet alias or existing per-user PATH
fallback, and retires the old portable registration only after successful
installation and any required restart. Autostart and configuration are preserved.
Uninstall removes only an adopted alias that still resolves to the installed EXE
or the exact adopted PATH segment. Machine-scope portable installations and
unrecognized source registrations are not automatically migrated.

Running-process selection uses the executable's canonical file path and current
Windows session, with retained process handles for bounded graceful shutdown
and forced fallback. A per-product setup mutex serializes installer invocations.
When setup is already elevated, it retains the running application's primary
token before stopping it and uses that token for restart, preserving its user
and privilege level. Normal non-elevated setup uses Inno's original-user launch.
Only a previously running application is restarted; a fresh silent installation
stays stopped. Failed installation restores the prior executable and running
state where the OS permits replacement and launch.

## Optional publication secret

Set the repository Actions secret `WINGET_TOKEN` to enable WinGet publication. It must be able to:

- create a branch and commit manifest files in `qqrm/winget-pkgs`;
- open pull requests against `microsoft/winget-pkgs`.

Use the narrowest token GitHub permits for both repositories. Keep the existing `RELEASE_TOKEN` dedicated to the release version-PR workflow; release-policy checks require that boundary.

Each publishing job first checks whether `WINGET_TOKEN` is configured. This check receives only a boolean; the token value is passed only to the submission step. If the secret is absent, the job reports a successful skip before checkout, validator installation, or submission. GitHub Releases do not require this secret. A configured but invalid token still fails publication normally.

Validator installation and read-only CI use the ordinary repository token. The WinGet jobs do not request repository write or OIDC permissions, and checkouts never persist credentials. The required CI validator runs regardless of whether the publishing secret exists.

## Backfill

After the automation PR is merged, run **Backfill WinGet manifest** from Actions on `dev` with an empty release tag to submit the latest stable release. The script checks that it is still latest, downloads and hashes the exact EXE, verifies its PE header, validates the generated manifests with `winget validate`, and only then opens a PR. Portable binaries must be x64; an Inno installer may use the 32-bit bootstrap for its x64 payload. An invalid installer asset never falls back to the standalone EXE. The script never merges the WinGet PR.

Backfill also skips successfully when `WINGET_TOKEN` is absent. The run summary states that publication was skipped; a successful run in this case does not mean a manifest was submitted.

The release pipeline and both standalone submission jobs share the `release-dev` concurrency group. Each submission checks the official catalog and existing package PRs before writing and again before opening its PR. Incomplete or truncated searches stop publication. A matching open PR is reused without writes; a closed/rejected PR, another pending package update, or an orphaned manifest branch requires manual inspection. This prevents silent duplicate submissions after a failed run.

New branches start from Microsoft's current `master`, so a stale fork cannot introduce unrelated changes. Manifest files are UTF-8 with CRLF line endings; the same bytes validated locally are Base64-encoded for GitHub's Contents API.

## Automated verification

The **Workflow Security** gate requires the `winget-validation` job on `windows-2025`:

- Standard-library unit tests cover generation, release/asset identity, Base64 payloads, permission failures, duplicate detection, stale releases, and publication with strict GitHub API stubs. Unexpected network requests and subprocess commands fail the tests.
- `.github/scripts/install_winget.ps1` installs the pinned official Microsoft WinGet client and bootstrap module. Every publication path uses the same setup.
- `python .github/scripts/winget_release.py --validate-only` verifies the actual latest stable EXE and runs `winget validate --manifest` against generated manifests. It cannot create branches, write files to GitHub, or open a PR.

The required application CI gate separately exercises actual WinGet install and
upgrade operations on a disposable Windows desktop, including migration from
the published portable package, running/stopped states, autostart preferences,
failure recovery and uninstall. Release builds run this acceptance before
bundling publication files. The built-in updater is tested through the actual
installer with fixture GitHub transport; the shipped helper has no test endpoint
or verification bypass. See `scripts/e2e/README.md` for the coverage boundary.

The unit tests also run on Linux in the dependency-policy job. Run them locally with:

```text
python -m unittest discover -s .github/scripts -p 'test_winget*.py' -v
```
