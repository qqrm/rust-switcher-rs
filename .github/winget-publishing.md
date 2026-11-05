# WinGet publishing

The release workflow submits the already-published `rust-switcher.exe` to the community WinGet repository after the existing release and crate publication jobs succeed. It does not build or publish another binary.

## Required secret

Set the repository Actions secret `WINGET_TOKEN`. It must be able to:

- create a branch and commit manifest files in `qqrm/winget-pkgs`;
- open pull requests against `microsoft/winget-pkgs`.

Use the narrowest token GitHub permits for both repositories. Keep the existing `RELEASE_TOKEN` dedicated to the release version-PR workflow; release-policy checks require that boundary.

## Backfill

After the automation PR is merged, run **Backfill WinGet manifest** from Actions with an empty release tag to submit the latest stable release. The script checks that it is still latest, downloads and hashes the exact EXE, verifies its x64 PE header, validates the generated manifests with `winget validate`, and only then opens a PR. It never merges that PR.
