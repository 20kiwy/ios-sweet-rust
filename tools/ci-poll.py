#!/usr/bin/env python3
"""ci-poll.py -- poll full-boot runs + jobs (loop-friendly CI checker).

Usage:
  python3 tools/ci-poll.py            # latest 5 runs w/ jobs
  python3 tools/ci-poll.py <RUN_ID>   # one run w/ jobs
Exit codes: 0 = at least one run succeeded, 1 = a run failed,
2 = still running/pending, 3 = API error.
"""
import json
import os
import sys
import urllib.request

# Token comes from the environment (push protection: never hardcode).
#   export GH_TOKEN=ghp_...   (the repo's PAT, or a fine-grained read token)
TOKEN = os.environ.get("GH_TOKEN", "")
REPO = os.environ.get("GH_REPO", "20kiwy/ios-sweet-rust")
HDR = {
    "Accept": "application/vnd.github+json",
    "User-Agent": "ci-poll",
}
if TOKEN:
    HDR["Authorization"] = f"token {TOKEN}"


def get(url):
    req = urllib.request.Request(url, headers=HDR)
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)


def main():
    if len(sys.argv) > 1:
        run = get(f"https://api.github.com/repos/{REPO}/actions/runs/{sys.argv[1]}")
        runs = [run]
    else:
        runs = get(
            f"https://api.github.com/repos/{REPO}/actions/workflows/full-boot.yml/runs?per_page=5"
        )["workflow_runs"]

    for r in runs:
        rid = r["id"]
        jobs = get(f"https://api.github.com/repos/{REPO}/actions/runs/{rid}/jobs?per_page=10")
        n = jobs["total_count"]
        print(f"run {rid} | {r['head_sha'][:7]} | {r['event']} | "
              f"{r['status']}/{r['conclusion']} | jobs={n}")
        for j in jobs.get("jobs", []):
            steps = {s["name"]: s["conclusion"] for s in j.get("steps", [])}
            failed = [k for k, v in steps.items() if v not in (None, "success", "skipped")
                      and v is not None]
            print(f"  job {j['name'][:40]} | {j['status']}/{j['conclusion']} "
                  f"| labels={j.get('labels')} | started={j.get('started_at')}")
            if failed:
                print(f"    FAILED STEPS: {failed}")

    live = [r for r in runs if r["status"] in ("queued", "pending", "in_progress")]
    if live:
        return 2
    ok = [r for r in runs if r["conclusion"] == "success"]
    return 0 if ok else (1 if runs else 3)


if __name__ == "__main__":
    sys.exit(main())
