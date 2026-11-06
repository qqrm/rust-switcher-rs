"""Keep WinGet publication credentials and Windows validation behind CI gates."""

import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


def workflow(name):
    return (ROOT / ".github/workflows" / name).read_text(encoding="utf-8")


def jobs(text):
    """Read the repository's block-style job sections without a YAML dependency."""
    body = text.split("\njobs:\n", 1)[1]
    starts = list(re.finditer(r"(?m)^  ([a-zA-Z0-9_-]+):[ \t]*\n", body))
    return {
        match.group(1): body[match.end():starts[index + 1].start()
                            if index + 1 < len(starts) else len(body)]
        for index, match in enumerate(starts)
    }


def steps(job):
    return re.split(r"(?m)^      - name: ", job.split("    steps:\n", 1)[1])[1:]


def block(text, key, indent):
    """Return one direct block; policy changes to YAML shape must be reviewed."""
    pattern = rf"(?m)^{' ' * indent}{re.escape(key)}:[ \t]*\n"
    match = re.search(pattern, text)
    if match is None:
        raise AssertionError(f"Missing {key} block at indentation {indent}")
    lines = []
    for line in text[match.end():].splitlines():
        if line.strip() and len(line) - len(line.lstrip()) <= indent:
            break
        if line.strip() and not line.lstrip().startswith("#"):
            lines.append(line.strip())
    return lines


class WinGetWorkflowTests(unittest.TestCase):
    def assert_read_only(self, text, indent):
        self.assertEqual(block(text, "permissions", indent), ["contents: read"])

    def assert_release_lock(self, text, indent):
        self.assertEqual(block(text, "concurrency", indent),
                         ["group: release-dev", "cancel-in-progress: false"])

    def assert_checkout(self, job, ref=None):
        checkouts = [step for step in steps(job) if "uses: actions/checkout@" in step]
        self.assertEqual(len(checkouts), 1)
        self.assertIn("persist-credentials: false", checkouts[0])
        self.assertNotIn("token:", checkouts[0])
        if ref is not None:
            self.assertIn(f"ref: ${{{{ {ref} }}}}", checkouts[0])

    def assert_installer_before(self, job, command):
        job_steps = steps(job)
        setup = [step for step in job_steps if "install_winget.ps1" in step]
        consumer = [step for step in job_steps if command in step]
        self.assertEqual(len(setup), 1)
        self.assertEqual(len(consumer), 1)
        self.assertLess(job_steps.index(setup[0]), job_steps.index(consumer[0]))
        self.assertIn("shell: pwsh", setup[0])
        self.assertIn("GH_TOKEN: ${{ github.token }}", setup[0])
        self.assertNotIn("secrets.", setup[0])
        self.assertNotIn("continue-on-error:", setup[0])

    def assert_publisher(self, job):
        self.assertIn("runs-on: windows-2025", job)
        prefix = job.split("    steps:\n", 1)[0]
        self.assertNotIn("secrets.", prefix)
        self.assertNotIn("continue-on-error:", job)
        self.assertNotIn("secrets.RELEASE_TOKEN", job)
        job_steps = steps(job)
        self.assertGreater(len(job_steps), 1)
        credentials = job_steps[0]
        self.assertTrue(credentials.startswith("Check WinGet publication credentials\n"))
        self.assertRegex(credentials, r"(?m)^        id: credentials$")
        self.assertRegex(credentials, r"(?m)^        shell: pwsh$")
        self.assertNotRegex(credentials, r"(?m)^        (if|uses):")
        self.assertEqual(block(credentials, "env", 8),
                         ["WINGET_TOKEN_CONFIGURED: ${{ secrets.WINGET_TOKEN != '' }}"])
        self.assertEqual(re.findall(r"secrets\.([A-Z_]+)", credentials), ["WINGET_TOKEN"])
        check_script = credentials.split("        run: |\n", 1)[1]
        self.assertEqual([line.strip() for line in check_script.splitlines() if line.strip()], [
            "if ($env:WINGET_TOKEN_CONFIGURED -ne 'true') {",
            "'available=false' >> $env:GITHUB_OUTPUT",
            "'Skipping WinGet publication: WINGET_TOKEN is not configured.' >> $env:GITHUB_STEP_SUMMARY",
            "exit 0",
            "}",
            "'available=true' >> $env:GITHUB_OUTPUT",
        ])
        for step in job_steps[1:]:
            self.assertEqual(re.findall(r"(?m)^        if: (.+)$", step),
                             ["steps.credentials.outputs.available == 'true'"])
        secret_steps = [step for step in job_steps if "secrets." in step]
        self.assertEqual(len(secret_steps), 2)
        self.assertEqual(secret_steps[0], credentials)
        publish = secret_steps[1]
        self.assertEqual(re.findall(r"secrets\.([A-Z_]+)", publish), ["WINGET_TOKEN"])
        self.assertIn("GH_TOKEN: ${{ secrets.WINGET_TOKEN }}", publish)
        self.assertIn("python .github/scripts/winget_release.py", publish)
        self.assertNotIn("--validate-only", publish)
        self.assertNotIn("install_winget.ps1", publish)
        self.assertNotRegex(job, r"\bcargo\s|rust-toolchain@|rust-cache@")
        self.assert_installer_before(job, "python .github/scripts/winget_release.py")

    def test_standalone_publishers_use_windows_and_only_the_dedicated_secret(self):
        for name in ("winget.yml", "winget-backfill.yml"):
            with self.subTest(workflow=name):
                text = workflow(name)
                header = text.split("\njobs:\n", 1)[0]
                self.assert_read_only(header, 0)
                self.assertNotIn("secrets.", header)
                self.assertNotIn("id-token:", text)
                self.assertNotRegex(text, r"(?m)^\s+\S+:\s*write\s*$")
                job = jobs(text)["submit"]
                # Either inherit the explicit read-only default or narrow it again.
                if re.search(r"(?m)^    permissions:", job):
                    self.assert_read_only(job, 4)
                self.assert_release_lock(job, 4)
                self.assert_checkout(job, "github.sha")
                self.assert_publisher(job)

    @unittest.skipUnless(shutil.which("pwsh"), "PowerShell is required to execute the credential check")
    def test_optional_credentials_report_skip_or_enable_publication_in_powershell(self):
        for name, job_name in (("release.yml", "winget"), ("winget.yml", "submit"),
                               ("winget-backfill.yml", "submit")):
            job = jobs(workflow(name))[job_name]
            # Verify the exact safe check before executing extracted workflow code.
            self.assert_publisher(job)
            script = steps(job)[0].split("        run: |\n", 1)[1]
            for configured in ("false", "true"):
                with self.subTest(workflow=name, configured=configured), \
                        tempfile.TemporaryDirectory() as temporary:
                    output = Path(temporary) / "output"
                    summary = Path(temporary) / "summary"
                    environment = {key: value for key, value in os.environ.items() if key.upper() in {
                        "PATH", "SYSTEMROOT", "SYSTEMDRIVE", "WINDIR", "TEMP", "TMP",
                        "HOME", "USERPROFILE", "PSMODULEPATH",
                    }}
                    environment.update(WINGET_TOKEN_CONFIGURED=configured,
                                       GITHUB_OUTPUT=str(output), GITHUB_STEP_SUMMARY=str(summary))
                    result = subprocess.run(
                        ["pwsh", "-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script],
                        cwd=temporary, env=environment, text=True, capture_output=True, timeout=15,
                    )
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertEqual(output.read_text(encoding="utf-8-sig").splitlines(),
                                     [f"available={configured}"])
                    if configured == "false":
                        self.assertEqual(summary.read_text(encoding="utf-8-sig").splitlines(),
                                         ["Skipping WinGet publication: WINGET_TOKEN is not configured."])
                    else:
                        self.assertFalse(summary.exists())

    def test_release_event_selects_its_exact_published_stable_tag(self):
        text = workflow("winget.yml")
        self.assertIn("  release:\n    types: [published]", text)
        job = jobs(text)["submit"]
        guard = re.search(r"(?m)^    if: (.+)$", job)
        self.assertIsNotNone(guard)
        self.assertEqual(guard.group(1), "github.event.release.draft == false && "
                         "github.event.release.prerelease == false")
        self.assertIn("RELEASE_TAG: ${{ github.event.release.tag_name }}", job)
        self.assertNotIn("EVENT_PATH:", job)

    def test_backfill_only_executes_the_selected_dev_commit(self):
        text = workflow("winget-backfill.yml")
        self.assertIn("  workflow_dispatch:", text)
        job = jobs(text)["submit"]
        self.assertIn("if: github.ref == 'refs/heads/dev'", job)
        self.assert_checkout(job, "github.sha")
        self.assertIn("RELEASE_TAG: ${{ inputs.release_tag }}", job)
        for step in steps(job):
            if "run:" in step:
                command = step.split("run:", 1)[1]
                self.assertNotIn("${{", command)

    def test_release_publisher_waits_for_success_and_has_no_release_permissions(self):
        text = workflow("release.yml")
        header = text.split("\njobs:\n", 1)[0]
        self.assert_release_lock(header, 0)
        release_jobs = jobs(text)
        job = release_jobs["winget"]
        self.assert_read_only(job, 4)
        self.assertNotIn("id-token:", job)
        self.assertIn("needs: [version-pr, publish]", job)
        self.assertNotIn("always()", job)
        self.assertNotIn("continue-on-error:", job)
        self.assert_checkout(job, "needs.version-pr.outputs.merged")
        self.assert_publisher(job)
        self.assertIn("RELEASE_TAG: v${{ needs.version-pr.outputs.version }}", job)
        self.assertNotIn("WINGET_TOKEN", release_jobs["publish"])
        self.assertEqual(text.count("secrets.RELEASE_TOKEN"), 1)
        self.assertIn("secrets.RELEASE_TOKEN", release_jobs["version-pr"])
        self.assertEqual(text.count("secrets.WINGET_TOKEN"), 2)

    def test_all_actions_in_publication_and_validation_workflows_are_sha_pinned(self):
        for name in ("winget.yml", "winget-backfill.yml", "release.yml",
                     "workflow-security.yml"):
            with self.subTest(workflow=name):
                actions = re.findall(r"(?m)^\s+uses:\s*(\S+)", workflow(name))
                self.assertTrue(actions)
                for action in actions:
                    self.assertRegex(action, r"^[a-zA-Z0-9_.-]+/[a-zA-Z0-9_./-]+@[a-f0-9]{40}$")

    def test_pr_ci_runs_stub_tests_and_the_real_windows_validator_without_secrets(self):
        text = workflow("workflow-security.yml")
        self.assertIn("  pull_request:", text)
        self.assertNotIn("pull_request_target:", text)
        job = jobs(text)["winget-validation"]
        self.assertIn("runs-on: windows-2025", job)
        self.assert_read_only(job, 4)
        self.assert_checkout(job)
        self.assertNotIn("secrets.", job)
        self.assertNotIn("continue-on-error:", job)
        self.assertNotIn("if:", job)
        self.assertIn("python -m unittest discover -s .github/scripts -p 'test_winget*.py'", job)
        self.assertIn("python .github/scripts/winget_release.py --validate-only", job)
        self.assert_installer_before(job, "python .github/scripts/winget_release.py --validate-only")
        validator = next(step for step in steps(job) if "--validate-only" in step)
        self.assertIn("GH_TOKEN: ${{ github.token }}", validator)

    def test_required_security_gate_rejects_a_failed_or_skipped_windows_validator(self):
        gate = jobs(workflow("workflow-security.yml"))["workflow-security-gate"]
        self.assertIn("if: always()", gate)
        needs = re.search(r"(?m)^    needs: \[([^]]+)\]$", gate)
        self.assertIsNotNone(needs)
        self.assertIn("winget-validation", [name.strip() for name in needs.group(1).split(",")])
        self.assertIn("WINGET_RESULT: ${{ needs.winget-validation.result }}", gate)
        self.assertIn('test "$WINGET_RESULT" = success', gate)
        self.assertNotIn("continue-on-error:", gate)

    def test_installer_pins_the_official_microsoft_module_and_verifies_client_version(self):
        text = (ROOT / ".github/scripts/install_winget.ps1").read_text(encoding="utf-8")
        self.assertRegex(text, r"(?m)^\$version = '[0-9]+\.[0-9]+\.[0-9]+'$")
        self.assertIn("$ErrorActionPreference = 'Stop'", text)
        self.assertIn("Install-Module Microsoft.WinGet.Client -RequiredVersion $version "
                      "-Scope CurrentUser -Repository PSGallery", text)
        self.assertIn("Import-Module Microsoft.WinGet.Client -RequiredVersion $version", text)
        self.assertIn("Repair-WinGetPackageManager -Version $version", text)
        self.assertIn("Get-AppxPackage -Name Microsoft.DesktopAppInstaller", text)
        self.assertIn("$wingetDirectory = $package.InstallLocation", text)
        self.assertIn('$env:PATH = "$wingetDirectory;$env:PATH"', text)
        self.assertIn("$wingetDirectory | Out-File -FilePath $env:GITHUB_PATH", text)
        self.assertNotIn("$env:LOCALAPPDATA", text)
        self.assertNotRegex(text, r"(?i)-Latest|-IncludePrerelease|-AllowPrerelease")
        self.assertIn("$installed = winget --version", text)
        self.assertIn('$LASTEXITCODE -ne 0 -or $installed.Trim() -ne "v$version"', text)
        self.assertIn('throw "Expected WinGet v$version', text)
        self.assertNotIn("secrets.", text)


if __name__ == "__main__":
    unittest.main()
