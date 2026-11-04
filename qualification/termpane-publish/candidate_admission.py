# SPDX-FileCopyrightText: 2026 Alexey Zhokhov
# SPDX-License-Identifier: Apache-2.0

import json
import re
import sys
import urllib.parse
import urllib.request

REPOSITORY = "tailrocks/velnor-new"
API = f"https://api.github.com/repos/{REPOSITORY}"


def select_run(candidate, number, pull, rules, checks):
    if not re.fullmatch(r"[0-9a-f]{40}", candidate) or not re.fullmatch(r"[1-9][0-9]*", number):
        raise ValueError("invalid candidate identity")
    if not (pull["number"] == int(number) and pull["state"] == "open" and pull["draft"] is False
            and pull["base"]["ref"] == "main" and pull["base"]["repo"]["full_name"] == REPOSITORY
            and pull["head"]["repo"]["full_name"] == REPOSITORY and pull["head"]["sha"] == candidate
            and pull["mergeable_state"] == "clean"):
        raise ValueError("candidate PR is not eligible")
    required = [r["parameters"] for r in rules if r["type"] == "required_status_checks"]
    reviews = [r["parameters"] for r in rules if r["type"] == "pull_request"]
    if not (any(r["strict_required_status_checks_policy"] is True and any(c["context"] == "Required" for c in r["required_status_checks"]) for r in required)
            and any(r["required_review_thread_resolution"] is True for r in reviews)):
        raise ValueError("main rules are not eligible")
    runs = checks["check_runs"]
    if checks["total_count"] != len(runs) or not runs:
        raise ValueError("incomplete checks")
    matches = [r for r in runs if r["name"] == "Required" and r["app"]["id"] == 15368 and r["head_sha"] == candidate]
    if not matches or any(r["status"] != "completed" or not r["completed_at"] for r in matches):
        raise ValueError("required check is incomplete")
    matches.sort(key=lambda r: r["completed_at"], reverse=True)
    if len(matches) > 1 and matches[0]["completed_at"] == matches[1]["completed_at"]:
        raise ValueError("ambiguous required check")
    if matches[0]["conclusion"] != "success":
        raise ValueError("required check failed")
    match = re.fullmatch(r"https://github\.com/tailrocks/velnor-new/actions/runs/([1-9][0-9]*)/job/[1-9][0-9]*", matches[0]["details_url"])
    if not match:
        raise ValueError("invalid workflow run URL")
    return int(match.group(1))


def validate_run(candidate, run_id, run):
    return (run["id"] == run_id and run["head_sha"] == candidate and run["event"] == "pull_request"
            and run["status"] == "completed" and run["conclusion"] == "success"
            and run["path"] == ".github/workflows/ci.yml"
            and run["repository"]["full_name"] == REPOSITORY
            and run["head_repository"]["full_name"] == REPOSITORY)


def get(path):
    request = urllib.request.Request(API + path, headers={"Accept": "application/vnd.github+json", "User-Agent": "termpane-no-publish-rehearsal"})
    with urllib.request.urlopen(request, timeout=20) as response:
        if response.headers.get("Link") or urllib.parse.urlparse(response.url).netloc != "api.github.com":
            raise ValueError("untrusted or paginated response")
        body = response.read(2_000_001)
        if len(body) > 2_000_000:
            raise ValueError("response too large")
        return json.loads(body)


def main():
    candidate, number = sys.argv[1:]
    if not re.fullmatch(r"[0-9a-f]{40}", candidate) or not re.fullmatch(r"[1-9][0-9]*", number):
        raise ValueError("invalid candidate identity")
    pull = get(f"/pulls/{number}")
    rules = get("/rules/branches/main")
    checks = get(f"/commits/{candidate}/check-runs?check_name=Required&filter=latest&per_page=100")
    run_id = select_run(candidate, number, pull, rules, checks)
    if not validate_run(candidate, run_id, get(f"/actions/runs/{run_id}")):
        raise ValueError("workflow run is not eligible")


if __name__ == "__main__":
    main()
