param(
  [Parameter(Mandatory = $false)]
  [string]$Version = ''
)

. "$PSScriptRoot\common.ps1"

Assert-Tool git
Assert-Tool cargo
Assert-Tool gh
Assert-Tool rustc

$repoRoot = Get-RepoRoot
Push-Location $repoRoot
try {
  Assert-OnBranch 'dev'
  Assert-CleanWorktree

  # Ensure we see remote tags for immutability checks.
  $null = Invoke-Checked git @('fetch', 'origin', '--tags', '--prune') -Quiet

  $sourceHead = (Invoke-Checked git @('rev-parse', 'HEAD') -Quiet).Output.Trim()
  $existingTag = Find-ReleaseTagForSourceCommit $sourceHead

  # Determine target version (for early tag immutability validation).
  $appCur  = Get-CargoPackageVersion 'rust-switcher'
  $coreCur = Get-CargoPackageVersion 'rust-switcher-core'

  $target = if ($existingTag) {
    Get-VersionFromTag $existingTag
  } elseif ([string]::IsNullOrWhiteSpace($Version)) {
    $base = Max-SemVer $appCur $coreCur
    $latestTag = Get-LatestVersionTag
    if ($latestTag) {
      $base = Max-SemVer $base (Get-VersionFromTag $latestTag)
    }
    Bump-Patch $base
  } else {
    Assert-SemVer $Version
  }

  $tag = "v$target"
  $tagSha = Get-TagSha $tag
  if ($existingTag -and $existingTag -ne $tag) {
    throw "Source commit $sourceHead already maps to tag '$existingTag', but current target is '$tag'. Refusing to continue."
  }
  if (-not $existingTag -and $tagSha) {
    throw "Tag '$tag' already exists but is not associated with source commit $sourceHead. Refusing to continue."
  }

  # Checks (must pass before version bump / release).
  Write-Host "`n>> cargo fmt --all -- --check"
  Invoke-Checked cargo @('fmt', '--all', '--', '--check')

  Write-Host "`n>> cargo clippy --workspace --all-targets --all-features --locked -- -D warnings"
  Invoke-Checked cargo @('clippy', '--workspace', '--all-targets', '--all-features', '--locked', '--', '-D', 'warnings')

  Write-Host "`n>> cargo test --workspace --all-features --all-targets --locked"
  Invoke-Checked cargo @('test', '--workspace', '--all-features', '--all-targets', '--locked')

  $didBump = $false
  $created = $false
  if ($existingTag) {
    Write-Host "`n>> reusing existing release tag $tag for source $sourceHead"
    Invoke-Checked git @('checkout', '--detach', $tag) | Out-Null
  } else {
    Write-Host "`n>> bumping version to $target"
    $newV = & pwsh -ExecutionPolicy Bypass -NoLogo -NoProfile -File "$PSScriptRoot\bump.ps1" $target $sourceHead
    $newV = ($newV |
      ForEach-Object { "$_".Trim() } |
      Where-Object { $_ } |
      Select-Object -Last 1)
    if ($newV -ne $target) {
      throw "bump.ps1 returned '$newV' but expected '$target'"
    }
    $didBump = $true

    Assert-CleanWorktree

    $head = (Invoke-Checked git @('rev-parse', 'HEAD') -Quiet).Output.Trim()

    # The version commit must pass the same native gates as every other change.
    # Never use an admin bypass or push the bump directly to the protected branch.
    $releaseBranch = "automation/release-$sourceHead"
    $repo = $env:GITHUB_REPOSITORY
    if (-not $repo) {
      $repo = (Invoke-Checked gh @('repo', 'view', '--json', 'nameWithOwner', '--jq', '.nameWithOwner') -Quiet).Output.Trim()
    }
    $prs = @((Invoke-Checked gh @('pr', 'list', '--repo', $repo, '--base', 'dev', '--head', $releaseBranch,
      '--state', 'all', '--json', 'number,headRefOid,state') -Quiet).Output | ConvertFrom-Json)
    if ($prs.Count -gt 1) { throw 'Multiple version PRs for the same source commit; reconcile before release.' }
    if ($prs.Count -eq 1) {
      $pr = $prs[0]
      if ($pr.state -eq 'CLOSED') { throw 'Version PR was closed; publication refused.' }
      Invoke-Checked git @('fetch', 'origin', $pr.headRefOid) | Out-Null
      # A retry may have a different commit timestamp, but must have the identical tree.
      Invoke-Checked git @('diff', '--exit-code', $head, $pr.headRefOid) | Out-Null
      $head = $pr.headRefOid
    } else {
      Invoke-Checked git @('push', 'origin', "HEAD:refs/heads/$releaseBranch") | Out-Null
      $bodyFile = New-TemporaryFile
      try {
        Set-Content -LiteralPath $bodyFile -Value "Release version $target for tested source $sourceHead. Native CI and security gates must pass before publication." -Encoding UTF8
        Invoke-Checked gh @('pr', 'create', '--repo', $repo, '--base', 'dev', '--head', $releaseBranch,
          '--title', "chore: release $target", '--body-file', $bodyFile) | Out-Null
      } finally { Remove-Item -LiteralPath $bodyFile }
      $pr = (Invoke-Checked gh @('pr', 'view', $releaseBranch, '--repo', $repo,
        '--json', 'number,headRefOid,state') -Quiet).Output | ConvertFrom-Json
      if ($pr.headRefOid -ne $head) { throw 'Version PR head changed before merge.' }
    }
    if ($pr.state -ne 'MERGED') {
      Invoke-Checked gh @('pr', 'merge', "$($pr.number)", '--repo', $repo, '--auto', '--squash',
        '--match-head-commit', $head, '--subject', "chore: release $target", '--body', "release-source: $sourceHead") | Out-Null
    }
    $mergedHead = (Invoke-Checked python @('.github/scripts/release_gate.py', '--repo', $repo,
      '--pr', "$($pr.number)", '--head', $head, '--base', 'dev') -Quiet).Output.Trim()
    if ($mergedHead -notmatch '^[a-f0-9]{40}$') { throw 'Missing checked version merge SHA.' }
    Invoke-Checked git @('fetch', 'origin', $mergedHead) | Out-Null
    Invoke-Checked git @('checkout', '--detach', $mergedHead) | Out-Null
    $head = $mergedHead

    $created = Ensure-Tag-Immutable -Tag $tag -ExpectedSha $head

    Write-Host "`n>> git push origin $tag"
    Invoke-Checked git @('push', 'origin', $tag)
  }

  $head = (Invoke-Checked git @('rev-parse', 'HEAD') -Quiet).Output.Trim()
  $appNow  = Get-CargoPackageVersion 'rust-switcher'
  $coreNow = Get-CargoPackageVersion 'rust-switcher-core'
  if ($appNow -ne $target -or $coreNow -ne $target) {
    throw "Release tree versions are app=$appNow core=$coreNow (expected $target). Refusing to proceed."
  }

  # Build artifacts.
  Write-Host "`n>> cargo build --release"
  Invoke-Checked cargo @('build', '--release', '--locked')

  $exe = Join-Path $repoRoot 'target\release\rust-switcher.exe'
  if (-not (Test-Path $exe)) {
    throw "Expected build artifact not found: $exe"
  }

  $rustcVersion = (Invoke-Checked rustc @('-vV') -Quiet).Output
  $hostLine = $rustcVersion.Split("`n") |
    ForEach-Object { $_.Trim() } |
    Where-Object { $_ -match '^host:\s+' } |
    Select-Object -First 1
  if (-not $hostLine) {
    throw "Could not determine rustc host target from rustc -vV output."
  }
  $releaseTarget = ($hostLine -replace '^host:\s+', '').Trim()
  $zip = Join-Path $repoRoot ("target\release\rust-switcher-{0}-{1}.zip" -f $target, $releaseTarget)
  Remove-Item -Force $zip -ErrorAction SilentlyContinue
  Compress-Archive -Path $exe -DestinationPath $zip -CompressionLevel Optimal

  $assets = @($exe)
  $assets += $zip
  $pdb = Join-Path $repoRoot 'target\release\rust-switcher.pdb'
  if (Test-Path $pdb) { $assets += $pdb }

  # Create/update GitHub Release and upload assets.
  Write-Host "`n>> GitHub Release $tag (create/edit + upload assets)"
  $null = Invoke-Checked gh @('auth', 'status') -Quiet

  $notesFile = New-TemporaryFile
  try {
    $prev = (Invoke-Checked git @('tag', '--list', 'v*', '--sort=-v:refname') -Quiet).Output.Split("`n") |
      ForEach-Object { $_.Trim() } |
      Where-Object { $_ -and $_ -ne $tag } |
      Select-Object -First 1

    $changes = if ($prev) {
      (Invoke-Checked git @('log', '--oneline', "$prev..$tag") -Quiet).Output.Trim()
    } else {
      (Invoke-Checked git @('log', '--oneline', '-n', '20') -Quiet).Output.Trim()
    }

    $notes = @(
      "Release $tag",
      "",
      "Commit: $head",
      "",
      "Changes:",
      ($changes ?? '(no commits found)')
    ) -join "`n"
    Set-Content -Path $notesFile -Value $notes -Encoding UTF8

    $view = Invoke-Checked gh @('release', 'view', $tag) -AllowFailure -Quiet
    if ($view.ExitCode -ne 0) {
      Invoke-Checked gh @('release', 'create', $tag, '--title', $tag, '--notes-file', $notesFile, '--verify-tag')
    } else {
      Invoke-Checked gh @('release', 'edit', $tag, '--title', $tag, '--notes-file', $notesFile)
    }

    $uploadArgs = @('release', 'upload', $tag) + $assets + @('--clobber')
    Invoke-Checked gh $uploadArgs
  }
  finally {
    Remove-Item -Force $notesFile -ErrorAction SilentlyContinue
  }

  # Publish to crates.io in dependency order.
  Write-Host "`n>> crates.io publish"
  & pwsh -ExecutionPolicy Bypass -NoLogo -NoProfile -File "$PSScriptRoot\cargo_publish.ps1"

  $releaseUrl = (Invoke-Checked gh @('release', 'view', $tag, '--json', 'url', '-q', '.url') -Quiet).Output.Trim()
  $sha = (Invoke-Checked git @('rev-parse', 'HEAD') -Quiet).Output.Trim()

  Write-Host "`nOK"
  Write-Host ("  version: {0}" -f $target)
  Write-Host ("  tag:     {0}" -f $tag)
  Write-Host ("  head:    {0}" -f $sha)
  if ($releaseUrl) { Write-Host ("  release: {0}" -f $releaseUrl) }
  if ($didBump) { Write-Host "  bump:    committed locally + tagged" }
  if ($existingTag) { Write-Host "  retry:   reused existing release tag" }
  if ($created) { Write-Host "  tag:     created" } else { Write-Host "  tag:     already existed" }
}
finally {
  Pop-Location
}
