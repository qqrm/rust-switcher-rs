import copy
import unittest
from unittest.mock import patch
import release_gate


def ready():
    return {'headRefOid': 'a'*40, 'baseRefName': 'main', 'isCrossRepository': False,
            'state': 'MERGED', 'mergeCommit': {'oid': 'b'*40}, 'statusCheckRollup': [
                {'__typename': 'CheckRun', 'name': name, 'status': 'COMPLETED', 'conclusion': 'SUCCESS'}
                for name in ['ci-gate', 'workflow-security-gate']]}


class ReleaseGateTests(unittest.TestCase):
    def evaluate(self, value):
        return release_gate.merged_sha(value, 'a'*40, 'main')

    def test_only_exact_checked_merged_commit_is_published(self):
        self.assertEqual(self.evaluate(ready()), 'b'*40)

    def test_open_version_branch_is_never_published(self):
        value = ready(); value['state'] = 'OPEN'
        self.assertIsNone(self.evaluate(value))

    def test_changed_head_base_fork_and_closed_pr_fail(self):
        for key, val in [('headRefOid', 'c'*40), ('baseRefName', 'dev'), ('isCrossRepository', True), ('state', 'CLOSED')]:
            with self.subTest(key=key):
                value = ready(); value[key] = val
                with self.assertRaises(ValueError): self.evaluate(value)

    def test_failed_or_cancelled_analysis_refuses_publication(self):
        for conclusion in ['FAILURE', 'CANCELLED', 'TIMED_OUT', 'ACTION_REQUIRED']:
            value = ready(); value['statusCheckRollup'][0]['conclusion'] = conclusion
            with self.assertRaises(ValueError): self.evaluate(value)

    def test_missing_skipped_and_in_progress_gates_wait(self):
        for checks in [[], [{'__typename': 'CheckRun', 'name': 'ci-gate', 'status': 'IN_PROGRESS'}],
                       [{'__typename': 'CheckRun', 'name': 'ci-gate', 'status': 'COMPLETED', 'conclusion': 'SKIPPED'}]]:
            value = ready(); value['statusCheckRollup'] = checks
            self.assertIsNone(self.evaluate(value))

    def test_invalid_merge_sha_refused(self):
        for sha in ['', 'main', 'b'*39]:
            value = ready(); value['mergeCommit']['oid'] = sha
            with self.assertRaises(ValueError): self.evaluate(value)

    def test_legacy_failed_status_refused(self):
        value = ready(); value['statusCheckRollup'].append({'__typename': 'StatusContext', 'state': 'ERROR'})
        with self.assertRaises(ValueError): self.evaluate(value)

    def test_unknown_check_refused(self):
        value = ready(); value['statusCheckRollup'].append({'__typename': 'FutureCheck'})
        with self.assertRaises(ValueError): self.evaluate(value)

    @patch('release_gate.time.monotonic', side_effect=[0, 1])
    def test_timeout_never_falls_back_to_unmerged_branch(self, _clock):
        with self.assertRaises(TimeoutError): release_gate.wait_for_merge('owner/repo', 1, 'a'*40, 'main', timeout=1)


if __name__ == '__main__': unittest.main()
