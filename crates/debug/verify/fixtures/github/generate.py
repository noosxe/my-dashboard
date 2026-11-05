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
# The committed bodies carry no rate data (`rateLimit: null`): the
# default capture keeps the chip at "GH … · GQL ?" and the goldens
# unchanged. A rate variant (rate/<name>.json, selected with
# DASHBOARD_GITHUB_FIXTURE_RATE) is stamped onto every response at serve
# time instead — headers, GraphQL `data.rateLimit` and the /rate_limit
# probe — with resets as offsets from the pinned budget clock (docs/0081
# §12.1, fixture.rs).
RATE = None
# name -> per-bucket snapshot; reset_in_secs is relative to the budget
# clock's "now" (DASHBOARD_FIXED_NOW under captures). `exhausted` resets
# both buckets together so the pause countdown is order-independent.
RATE_VARIANTS = {
    "healthy": {
        "rest": {"limit": 5000, "remaining": 4812, "reset_in_secs": 2460},
        "graphql": {"limit": 5000, "remaining": 4655, "reset_in_secs": 1380},
        "graphql_cost": 1,
    },
    "low": {
        "rest": {"limit": 5000, "remaining": 312, "reset_in_secs": 2460},
        "graphql": {"limit": 5000, "remaining": 188, "reset_in_secs": 1380},
        "graphql_cost": 1,
    },
    "exhausted": {
        "rest": {"limit": 5000, "remaining": 0, "reset_in_secs": 2220},
        "graphql": {"limit": 5000, "remaining": 0, "reset_in_secs": 2220},
        "graphql_cost": 1,
    },
    # docs/0082 Q10: one budget paused, the other live (REST healthy).
    "graphql-exhausted": {
        "rest": {"limit": 5000, "remaining": 4812, "reset_in_secs": 2460},
        "graphql": {"limit": 5000, "remaining": 0, "reset_in_secs": 2220},
        "graphql_cost": 1,
    },
}
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


def history(start, count, total=TOTAL, make=None):
    make = make or commit
    nodes = [make(i) for i in range(start, start + count)]
    end = start + count
    more = end < total
    return {
        "oid": make(0)["oid"],
        "history": {
            "totalCount": total,
            "pageInfo": {
                "hasNextPage": more,
                "endCursor": f"{make(0)['oid']} {end - 1}" if more else None,
            },
            "nodes": nodes,
        },
    }


# docs/0081 D7: the ref selector's branch list and one picked branch.
# The fixture transport answers by operation name, so every picked
# branch serves this history — the receipt picks BRANCH.
BRANCHES = ["feature/ref-picker", "main", "release/1.0"]
BRANCH = "feature/ref-picker"
BRANCH_TOTAL = 6
BRANCH_HEADLINES = [
    "github: ref selector in the commits header",
    "github: per-ref freshness clock",
    "verify: branch fixtures for the receipts",
]


def branch_commit(i):
    # Newer than main's tip and on its own oids, so a pick visibly
    # swaps every row.
    c = commit(i)
    o = f"{(i * 2246822519 + 0xB5C0FFEE) % (1 << 32):08x}" + f"{i + 0xBB00:032x}"
    c.update(
        oid=o,
        abbreviatedOid=o[:7],
        messageHeadline=f"{BRANCH_HEADLINES[i % len(BRANCH_HEADLINES)]} (#{800 - i})",
        url=f"https://github.com/{FULL}/commit/{o}",
        signature=None,
        statusCheckRollup={"state": "PENDING" if i == 0 else "SUCCESS"},
    )
    t = NOW - timedelta(minutes=20 + i * 90)
    c["committedDate"] = c["authoredDate"] = iso(t)
    return c


# docs/0081 §14: the "mine" filter's history — the viewer's commits
# only (every third), served when the request carries `$author`.
MINE = [i for i in range(TOTAL) if AUTHORS[i % len(AUTHORS)]["login"] == VIEWER]


def mine_history():
    tip = commit(0)["oid"]
    return {
        "oid": tip,
        "history": {
            "totalCount": len(MINE),
            "pageInfo": {"hasNextPage": False, "endCursor": None},
            "nodes": [commit(i) for i in MINE],
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
                    # docs/0081 §14: the "mine" filter's CommitAuthor.id.
                    "id": "U_fixture",
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
    write(
        "graphql/RepoCommits.author.json",
        {
            "data": {
                "rateLimit": RATE,
                "repository": {"defaultBranchRef": {"name": "main", "target": mine_history()}},
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
    write(
        "graphql/RepoBranches.json",
        {
            "data": {
                "rateLimit": RATE,
                "repository": {
                    "defaultBranchRef": {"name": "main"},
                    "refs": {"totalCount": len(BRANCHES), "nodes": [{"name": n} for n in BRANCHES]},
                },
            }
        },
    )
    write(
        "graphql/RepoCommitsRef.json",
        {
            "data": {
                "rateLimit": RATE,
                "repository": {
                    "ref": {
                        "name": BRANCH,
                        "target": history(0, BRANCH_TOTAL, BRANCH_TOTAL, branch_commit),
                    }
                },
            }
        },
    )
    write("rest/notifications.json", [])
    write(f"rest/repos_{VIEWER}_{REPO}_notifications.json", [])
    write("rest/user.json", {"login": VIEWER, "id": 1, "avatar_url": None, "name": "Fixture User"})
    for name, variant in RATE_VARIANTS.items():
        write(f"rate/{name}.json", variant)


if __name__ == "__main__":
    main()
