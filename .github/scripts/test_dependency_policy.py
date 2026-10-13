import unittest

from dependency_policy import verified_bot_commits


def commit(sha="head", author="dependabot[bot]", verified=True):
    return {"sha": sha, "author": {"login": author},
            "commit": {"verification": {"verified": verified}}}


class PolicyTests(unittest.TestCase):
    def test_signed_bot_batch_needs_no_version_metadata_or_review(self):
        self.assertTrue(verified_bot_commits([commit("first"), commit()], "head"))

    def test_empty_batch_fails_closed(self):
        self.assertFalse(verified_bot_commits([], "head"))

    def test_changed_head_fails_closed(self):
        self.assertFalse(verified_bot_commits([commit("new")], "head"))

    def test_every_commit_must_be_from_dependabot(self):
        for author in ["contributor", "github-actions[bot]", "dependabot"]:
            with self.subTest(author=author):
                self.assertFalse(verified_bot_commits(
                    [commit("first", author=author), commit()], "head"))

    def test_every_commit_must_have_a_verified_signature(self):
        for verified in [False, None, "true"]:
            with self.subTest(verified=verified):
                self.assertFalse(verified_bot_commits(
                    [commit("first", verified=verified), commit()], "head"))

    def test_missing_provenance_fails_closed(self):
        for incomplete in [{}, {"author": None},
                           {"author": {"login": "dependabot[bot]"}}]:
            self.assertFalse(verified_bot_commits([incomplete, commit()], "head"))


if __name__ == "__main__":
    unittest.main()
