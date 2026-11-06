# WinGet publishing

The release workflow submits the already-published `rust-switcher.exe` to the community WinGet repository in a separate, read-only-permissions job after GitHub Release and crate publication succeed. It does not build another binary. This direct invocation is necessary because publishing a release with `GITHUB_TOKEN` does not trigger another release-event workflow. The standalone release-event workflow covers releases published by a user or another credential.

## Optional publication secret

Set the repository Actions secret `WINGET_TOKEN` to enable WinGet publication. It must be able to:

- create a branch and commit manifest files in `qqrm/winget-pkgs`;
- open pull requests against `microsoft/winget-pkgs`.

Use the narrowest token GitHub permits for both repositories. Keep the existing `RELEASE_TOKEN` dedicated to the release version-PR workflow; release-policy checks require that boundary.

Each publishing job first checks whether `WINGET_TOKEN` is configured. This check receives only a boolean; the token value is passed only to the submission step. If the secret is absent, the job reports a successful skip before checkout, validator installation, or submission. GitHub Releases do not require this secret. A configured but invalid token still fails publication normally.

Validator installation and read-only CI use the ordinary repository token. The WinGet jobs do not request repository write or OIDC permissions, and checkouts never persist credentials. The required CI validator runs regardless of whether the publishing secret exists.

## Backfill

After the automation PR is merged, run **Backfill WinGet manifest** from Actions on `dev` with an empty release tag to submit the latest stable release. The script checks that it is still latest, downloads and hashes the exact EXE, verifies its x64 PE header, validates the generated manifests with `winget validate`, and only then opens a PR. It never merges that PR.

Backfill also skips successfully when `WINGET_TOKEN` is absent. The run summary states that publication was skipped; a successful run in this case does not mean a manifest was submitted.

The release pipeline and both standalone submission jobs share the `release-dev` concurrency group. Each submission checks the official catalog and existing package PRs before writing and again before opening its PR. Incomplete or truncated searches stop publication. A matching open PR is reused without writes; a closed/rejected PR, another pending package update, or an orphaned manifest branch requires manual inspection. This prevents silent duplicate submissions after a failed run.

New branches start from Microsoft's current `master`, so a stale fork cannot introduce unrelated changes. Manifest files are UTF-8 with CRLF line endings; the same bytes validated locally are Base64-encoded for GitHub's Contents API.

## Automated verification

The **Workflow Security** gate requires the `winget-validation` job on `windows-2025`:

- Standard-library unit tests cover generation, release/asset identity, Base64 payloads, permission failures, duplicate detection, stale releases, and publication with strict GitHub API stubs. Unexpected network requests and subprocess commands fail the tests.
- `.github/scripts/install_winget.ps1` installs the pinned official Microsoft WinGet client and bootstrap module. Every publication path uses the same setup.
- `python .github/scripts/winget_release.py --validate-only` verifies the actual latest stable EXE and runs `winget validate --manifest` against generated manifests. It cannot create branches, write files to GitHub, or open a PR.

The unit tests also run on Linux in the dependency-policy job. Run them locally with:

```text
python -m unittest discover -s .github/scripts -p 'test_winget*.py' -v
```
