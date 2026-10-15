"""Exercise the actual jq programs used by the metadata-only write controller."""

import copy
import json
from pathlib import Path
import subprocess
import unittest


WORKFLOW = (Path(__file__).parents[1] / "workflows/dependabot-automerge.yml").read_text()


def evaluate(policy, value, **args):
    program = WORKFLOW.split(f"# {policy}-policy-begin\n", 1)[1].split(
        f"# {policy}-policy-end", 1
    )[0]
    command = ["jq", "-e"]
    for name, value_arg in args.items():
        command.extend(["--arg", name, value_arg])
    result = subprocess.run(command + [program], input=json.dumps(value),
                            text=True, capture_output=True)
    return result.returncode == 0


def ready_pr():
    return {
        "author": {"login": "app/dependabot"}, "isCrossRepository": False,
        "headRefOid": "checked", "baseRefName": "dev", "isDraft": False,
        "state": "OPEN", "mergeStateStatus": "CLEAN",
        "statusCheckRollup": [
            {"__typename": "CheckRun", "name": name, "status": "COMPLETED",
             "conclusion": "SUCCESS"}
            for name in ["ci-gate", "workflow-security-gate", "Analyze (rust)"]
        ],
    }


class MergePolicyTests(unittest.TestCase):
    def test_checked_bot_needs_no_human_review(self):
        self.assertTrue(evaluate("eligibility", ready_pr(), sha="checked"))

    def test_merged_checked_head_can_recover_failed_dispatch(self):
        pr = ready_pr()
        pr.update(state="MERGED", mergeCommit={"oid": "a" * 40}, mergeStateStatus="UNKNOWN")
        self.assertTrue(evaluate("eligibility", pr, sha="checked"))
        pr["headRefOid"] = "other"
        self.assertFalse(evaluate("eligibility", pr, sha="checked"))

    def test_wrong_identity_or_target_is_rejected(self):
        for key, value in [("author", {"login": "qqrm"}),
                           ("isCrossRepository", True), ("isDraft", True),
                           ("baseRefName", "other"), ("state", "CLOSED"),
                           ("headRefOid", "replaced"),
                           ("mergeStateStatus", "UNKNOWN"),
                           ("mergeStateStatus", "BEHIND")]:
            with self.subTest(key=key, value=value):
                pr = ready_pr()
                pr[key] = value
                self.assertFalse(evaluate("eligibility", pr, sha="checked"))

    def test_missing_failed_skipped_or_pending_required_gate_is_rejected(self):
        for name in ["ci-gate", "workflow-security-gate"]:
            for conclusion in ["FAILURE", "CANCELLED", "SKIPPED", "NEUTRAL", ""]:
                with self.subTest(name=name, conclusion=conclusion):
                    pr = ready_pr()
                    check = next(c for c in pr["statusCheckRollup"] if c["name"] == name)
                    check["conclusion"] = conclusion
                    self.assertFalse(evaluate("eligibility", pr, sha="checked"))
            pr = ready_pr()
            pr["statusCheckRollup"] = [c for c in pr["statusCheckRollup"] if c["name"] != name]
            self.assertFalse(evaluate("eligibility", pr, sha="checked"))

    def test_other_analysis_must_finish_without_failure(self):
        for status, conclusion in [("IN_PROGRESS", ""), ("QUEUED", ""),
                                   ("COMPLETED", "FAILURE"),
                                   ("COMPLETED", "TIMED_OUT")]:
            pr = ready_pr()
            pr["statusCheckRollup"][-1].update(status=status, conclusion=conclusion)
            self.assertFalse(evaluate("eligibility", pr, sha="checked"))

    def test_legacy_status_context_must_succeed(self):
        for state in ["SUCCESS", "PENDING", "FAILURE", "ERROR"]:
            pr = ready_pr()
            pr["statusCheckRollup"].append({"__typename": "StatusContext", "state": state})
            self.assertEqual(evaluate("eligibility", pr, sha="checked"), state == "SUCCESS")

    def test_unknown_check_type_fails_closed(self):
        pr = ready_pr()
        pr["statusCheckRollup"].append({"__typename": "Unknown", "state": "SUCCESS"})
        self.assertFalse(evaluate("eligibility", pr, sha="checked"))

    def test_paginated_signed_provenance(self):
        first = {"sha": "first", "author": {"login": "dependabot[bot]"},
                 "committer": {"login": "web-flow"},
                 "commit": {"verification": {"verified": True}}}
        last = copy.deepcopy(first)
        last["sha"] = "checked"
        self.assertTrue(evaluate("provenance", [[first], [last]], sha="checked"))
        for key, value in [("author", {"login": "qqrm"}),
                           ("committer", {"login": "qqrm"}),
                           ("commit", {"verification": {"verified": False}})]:
            invalid = copy.deepcopy(first)
            invalid[key] = value
            self.assertFalse(evaluate("provenance", [[invalid], [last]], sha="checked"))
        self.assertFalse(evaluate("provenance", [[first]], sha="checked"))
        self.assertFalse(evaluate("provenance", [[]], sha="checked"))

    def test_current_main_must_be_an_ancestor_of_batch(self):
        self.assertTrue(evaluate("freshness", {"behind_by": 0,
            "merge_base_commit": {"sha": "main"}}, base="main"))
        for response in [{}, {"behind_by": 1, "merge_base_commit": {"sha": "main"}},
                         {"behind_by": 0, "merge_base_commit": {"sha": "old"}}]:
            self.assertFalse(evaluate("freshness", response, base="main"))


if __name__ == "__main__":
    unittest.main()
