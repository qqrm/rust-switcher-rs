"""Wait for the exact version PR to merge through native protection before publishing."""
import argparse
import json
import re
import subprocess
import time


def merged_sha(info, head, base):
    if info.get('headRefOid') != head or info.get('baseRefName') != base or info.get('isCrossRepository') is not False:
        raise ValueError('Version PR identity changed; publication refused.')
    if info.get('state') == 'CLOSED':
        raise ValueError('Version PR closed without merging; publication refused.')
    checks = info.get('statusCheckRollup') or []
    passed = set()
    pending = False
    for check in checks:
        if check.get('__typename') == 'CheckRun':
            if check.get('status') != 'COMPLETED':
                pending = True
            elif check.get('conclusion') not in ('SUCCESS', 'NEUTRAL', 'SKIPPED'):
                raise ValueError('A version PR check failed; publication refused.')
            elif check.get('conclusion') == 'SUCCESS':
                passed.add(check.get('name'))
        elif check.get('__typename') == 'StatusContext':
            if check.get('state') == 'PENDING':
                pending = True
            elif check.get('state') != 'SUCCESS':
                raise ValueError('A version PR status failed; publication refused.')
        else:
            raise ValueError('Unknown version PR check; publication refused.')
    if info.get('state') != 'MERGED' or pending or not {'ci-gate', 'workflow-security-gate'} <= passed:
        return None
    sha = (info.get('mergeCommit') or {}).get('oid', '')
    if not re.fullmatch('[a-f0-9]{40}', sha):
        raise ValueError('Missing immutable merge commit; publication refused.')
    return sha


def wait_for_merge(repo, pr, head, base, timeout=1200):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        data = subprocess.check_output([
            'gh', 'pr', 'view', str(pr), '--repo', repo, '--json',
            'headRefOid,baseRefName,isCrossRepository,state,mergeCommit,statusCheckRollup',
        ], text=True)
        sha = merged_sha(json.loads(data), head, base)
        if sha:
            return sha
        time.sleep(10)
    raise TimeoutError('Version PR has not merged with successful checks; retry after CI succeeds.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', required=True)
    parser.add_argument('--pr', required=True, type=int)
    parser.add_argument('--head', required=True)
    parser.add_argument('--base', required=True)
    args = parser.parse_args()
    print(wait_for_merge(args.repo, args.pr, args.head, args.base))
