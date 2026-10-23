#!/usr/bin/env python3
"""Regenerate the GitHub verification fixtures (docs/0081 §12).

The app serves these files when a debug build runs with
DASHBOARD_GITHUB_FIXTURE=<this dir> (see crates/modules/github/src/
fixture.rs for the lookup rules). Captures also pin
DASHBOARD_FIXED_NOW=2026-10-23T12:00:00Z so relative ages are stable.

Run from anywhere: python3 crates/debug/verify/fixtures/github/generate.py
"""

import json
import os
from datetime import datetime, timedelta, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
NOW = datetime(2026, 10, 23, 12, 0, tzinfo=timezone.utc)
VIEWER = "fixture-user"
REPO = "atlas"
FULL = f"{VIEWER}/{REPO}"
# No rate data anywhere: the usage chip's label counts down to the
# reset on the real clock, which would make every golden drift.
RATE = None
TOTAL = 120
PAGE = 50

HEADLINES = [
    "github: commits tab rows and day headers",
    "build: pin the toolchain in all four places",
    "docs: record the stage notes",
    "ci: cache the cargo registry between jobs",
    "core: freshness label reads the lane clock",
    "favicon: inspector spacing follows the 4px scale",
    "usage: split GraphQL and REST buckets",
    "app: quit when the last window closes",
    "debug: wait-frame samples before dispatch",
    "settings: theme picker remembers the choice",
]
AUTHORS = [
    {"name": "Fixture User", "login": VIEWER},
    {"name": "Octo Cat", "login": "octocat"},
    {"name": "Dev Box", "login": None},  # unlinked email
]
ROLLUPS = ["SUCCESS", "SUCCESS", "FAILURE", "PENDING", None]


def iso(t):
    return t.strftime("%Y-%m-%dT%H:%M:%SZ")


def oid(i):
    return f"{(i * 2654435761) % (1 << 32):08x}" + f"{i:032x}"


def commit(i):
    t = NOW - timedelta(hours=1 + i * 5)
    a = AUTHORS[i % len(AUTHORS)]
    sig = None
    if i % 4 == 1:
        sig = {"isValid": True, "state": "VALID", "wasSignedByGitHub": True}
    elif i % 9 == 4:
        sig = {"isValid": False, "state": "UNKNOWN_KEY", "wasSignedByGitHub": False}
    rollup = ROLLUPS[i % len(ROLLUPS)]
    o = oid(i)
    return {
        "oid": o,
        "abbreviatedOid": o[:7],
        "messageHeadline": f"{HEADLINES[i % len(HEADLINES)]} (#{700 - i})",
        "committedDate": iso(t),
        "authoredDate": iso(t),
        "url": f"https://github.com/{FULL}/commit/{o}",
        "author": {
            "name": a["name"],
            "email": f"{a['name'].split()[0].lower()}@example.com",
            "avatarUrl": None,
            "user": {"login": a["login"]} if a["login"] else None,
        },
        "signature": sig,
        "statusCheckRollup": {"state": rollup} if rollup else None,
    }


def history(start, count):
    nodes = [commit(i) for i in range(start, start + count)]
    end = start + count
    more = end < TOTAL
    return {
        "oid": oid(0),
        "history": {
            "totalCount": TOTAL,
            "pageInfo": {
                "hasNextPage": more,
                "endCursor": f"{oid(0)} {end - 1}" if more else None,
            },
            "nodes": nodes,
        },
    }


def repo(name, desc, pushed_days):
    return {
        "databaseId": 1000 + len(name),
        "id": f"R_{name}",
        "name": name,
        "nameWithOwner": f"{VIEWER}/{name}",
        "description": desc,
        "isPrivate": False,
        "isArchived": False,
        "pushedAt": iso(NOW - timedelta(days=pushed_days)),
        "hasIssuesEnabled": True,
        "url": f"https://github.com/{VIEWER}/{name}",
        "owner": {"login": VIEWER},
    }


REPOS = [
    repo(REPO, "Fixture repository for verification captures", 0),
    repo("beacon", "A second fixture repository", 3),
    repo("compass", None, 9),
]


def write(rel, body):
    path = os.path.join(HERE, rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        json.dump(body, f, indent=1, sort_keys=True)
        f.write("\n")


def main():
    write(
        "graphql/SessionInventory.json",
        {
            "data": {
                "rateLimit": RATE,
                "viewer": {
                    "login": VIEWER,
                    # Unreachable on purpose: the container has no
                    # network, so avatars render their fallback.
                    "avatarUrl": "https://avatars.invalid/fixture-user",
                    "organizations": {"nodes": []},
                    "repositories": {
                        "pageInfo": {"hasNextPage": False, "endCursor": None},
                        "nodes": REPOS,
                    },
                },
            }
        },
    )
    write(
        "graphql/CardTotals.json",
        {
            "data": {
                "rateLimit": RATE,
                "viewer": {
                    "repositories": {
                        "pageInfo": {"hasNextPage": False, "endCursor": None},
                        "nodes": [
                            {"pullRequests": {"totalCount": 0}, "issues": {"totalCount": 0}}
                            for _ in REPOS
                        ],
                    }
                },
            }
        },
    )
    write(
        "graphql/AccountPullRequests.json",
        {
            "data": {
                "rateLimit": RATE,
                "search": {
                    "issueCount": 0,
                    "pageInfo": {"hasNextPage": False, "endCursor": None},
                    "nodes": [],
                },
            }
        },
    )
    for op, conn in (("RepoPulls", "pullRequests"), ("RepoIssues", "issues")):
        write(
            f"graphql/{op}.json",
            {
                "data": {
                    "rateLimit": RATE,
                    "repository": {
                        "name": REPO,
                        "nameWithOwner": FULL,
                        "owner": {"login": VIEWER},
                        conn: {"totalCount": 0, "nodes": []},
                    },
                }
            },
        )
    write(
        "graphql/RepoCommits.json",
        {
            "data": {
                "rateLimit": RATE,
                "repository": {"defaultBranchRef": {"name": "main", "target": history(0, PAGE)}},
            }
        },
    )
    # Page N, in order: 50..99, then 100..119 (the end of history).
    write(
        "graphql/RepoCommitsMore.json",
        [
            {"data": {"rateLimit": RATE, "repository": {"object": history(s, min(PAGE, TOTAL - s))}}}
            for s in range(PAGE, TOTAL, PAGE)
        ],
    )
    write("rest/notifications.json", [])
    write(f"rest/repos_{VIEWER}_{REPO}_notifications.json", [])
    write("rest/user.json", {"login": VIEWER, "id": 1, "avatar_url": None, "name": "Fixture User"})


if __name__ == "__main__":
    main()
