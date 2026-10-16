param(
  [Parameter(Mandatory = $false)]
  [string]$Version = ''
)

. "$PSScriptRoot\common.ps1"

Assert-Tool git
Assert-Tool gh

Push-Location (Get-RepoRoot)
try {
  Assert-OnBranch 'dev'
  Assert-CleanWorktree
  $head = (Invoke-Checked git @('rev-parse', 'HEAD') -Quiet).Output.Trim()
  $repo = (Invoke-Checked gh @('repo', 'view', '--json', 'nameWithOwner', '--jq', '.nameWithOwner') -Quiet).Output.Trim()
  $remote = (Invoke-Checked gh @('api', "repos/$repo/git/ref/heads/dev", '--jq', '.object.sha') -Quiet).Output.Trim()
  if ($head -ne $remote) {
    throw 'Local dev differs from GitHub dev. Sync the reviewed source before dispatching a release.'
  }
  $arguments = @('workflow', 'run', 'release.yml', '--repo', $repo, '--ref', 'dev', '-f', "expected_sha=$head")
  if (-not [string]::IsNullOrWhiteSpace($Version)) {
    $checkedVersion = Assert-SemVer $Version
    $arguments += @('-f', "version=$checkedVersion")
  }
  Invoke-Checked gh $arguments | Out-Null
  Write-Host "Release workflow dispatched for $head. Follow its checked version PR and publication jobs in GitHub Actions."
}
finally {
  Pop-Location
}
