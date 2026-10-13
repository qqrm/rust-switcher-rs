"""Read-only Dependabot provenance check; successful CI decides update eligibility."""

import json
import os
import subprocess
from pathlib import Path


def verified_bot_commits(commits, head):
    return bool(commits) and commits[-1].get("sha") == head and all(
        (commit.get("author") or {}).get("login") == "dependabot[bot]"
        and commit.get("commit", {}).get("verification", {}).get("verified") is True
        for commit in commits
    )


def main():
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    pr = event["pull_request"]
    repo = event["repository"]["full_name"]
    endpoint = f"repos/{repo}/pulls/{pr['number']}/commits?per_page=100"
    pages = json.loads(subprocess.check_output(
        ["gh", "api", endpoint, "--paginate", "--slurp"], text=True
    ))
    commits = [commit for page in pages for commit in page]
    if not verified_bot_commits(commits, pr["head"]["sha"]):
        raise SystemExit("The checked batch must contain only verified Dependabot "
                         "commits and still match the event's head commit.")
    print("Dependabot provenance verified. All update types use the required CI gates.")


if __name__ == "__main__":
    main()
