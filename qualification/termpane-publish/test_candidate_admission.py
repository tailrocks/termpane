# SPDX-FileCopyrightText: 2026 Alexey Zhokhov
# SPDX-License-Identifier: Apache-2.0

import unittest

from candidate_admission import select_run, validate_run

SHA = "a" * 40
REPO = "tailrocks/velnor-new"


def fixture():
    pull = {"number": 42, "state": "open", "draft": False, "mergeable_state": "clean",
            "base": {"ref": "main", "repo": {"full_name": REPO}},
            "head": {"sha": SHA, "repo": {"full_name": REPO}}}
    rules = [{"type": "pull_request", "parameters": {"required_review_thread_resolution": True}},
             {"type": "required_status_checks", "parameters": {"strict_required_status_checks_policy": True, "required_status_checks": [{"context": "Required"}]}}]
    check = {"name": "Required", "app": {"id": 15368}, "head_sha": SHA, "status": "completed",
             "completed_at": "2026-11-04T12:00:00Z", "conclusion": "success",
             "details_url": "https://github.com/tailrocks/velnor-new/actions/runs/12/job/34"}
    checks = {"total_count": 1, "check_runs": [check]}
    run = {"id": 12, "head_sha": SHA, "event": "pull_request", "status": "completed", "conclusion": "success",
           "path": ".github/workflows/ci.yml", "repository": {"full_name": REPO}, "head_repository": {"full_name": REPO}}
    return pull, rules, checks, run


class CandidateAdmission(unittest.TestCase):
    def test_exact_current_candidate_with_required_ci_is_accepted(self):
        pull, rules, checks, run = fixture()
        self.assertEqual(select_run(SHA, "42", pull, rules, checks), 12)
        self.assertTrue(validate_run(SHA, 12, run))

    def test_unsafe_or_incomplete_evidence_is_rejected(self):
        for case in ("stale", "fork", "base", "pending", "draft", "non_strict", "unresolved", "incomplete", "check_sha", "check_app", "check_result", "run_url"):
            with self.subTest(case=case):
                pull, rules, checks, _ = fixture()
                check = checks["check_runs"][0]
                if case == "stale": pull["head"]["sha"] = "b" * 40
                if case == "fork": pull["head"]["repo"]["full_name"] = "other/repo"
                if case == "base": pull["base"]["ref"] = "other"
                if case == "pending": pull["mergeable_state"] = "blocked"
                if case == "draft": pull["draft"] = True
                if case == "non_strict": rules[1]["parameters"]["strict_required_status_checks_policy"] = False
                if case == "unresolved": rules[0]["parameters"]["required_review_thread_resolution"] = False
                if case == "incomplete": checks["total_count"] += 1
                if case == "check_sha": check["head_sha"] = "b" * 40
                if case == "check_app": check["app"]["id"] = 1
                if case == "check_result": check["conclusion"] = "failure"
                if case == "run_url": check["details_url"] = "https://example.invalid"
                with self.assertRaises((ValueError, KeyError, TypeError)):
                    select_run(SHA, "42", pull, rules, checks)
        for case in ("workflow", "repository", "run_sha"):
            with self.subTest(case=case):
                _, _, _, run = fixture()
                if case == "workflow": run["path"] = ".github/workflows/other.yml"
                if case == "repository": run["repository"]["full_name"] = "other/repo"
                if case == "run_sha": run["head_sha"] = "b" * 40
                self.assertFalse(validate_run(SHA, 12, run))


if __name__ == "__main__":
    unittest.main()
