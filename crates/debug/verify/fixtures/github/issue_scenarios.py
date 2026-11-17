"""The issue page's verification scenarios (docs/0085 D9, #923).

Called from generate.py. Every scenario is an issue row on
fixture-user/beacon, answered by number: graphql/IssueDetail.<n>.json.
The shapes follow ISSUE_DETAIL_QUERY in crates/modules/github/src/gql.rs.
Sources are synthetic: `fixture-user/atlas` and `fixture-user/beacon` are
the repositories the module lists (a source there opens in the app),
`fixture-org/elsewhere` is not (it opens the browser).

- 201: an open issue that PR 103 (a beacon PR scenario) will close, with a
  mention from an issue, a merged PR in atlas, an untracked source and a
  deleted actor; also a comment and a label event, so the references
  interleave with the existing rows.
- 198: a closed, locked issue (completed) closed by a merged PR, seen by a
  reader: no comment box, so the keyboard walk (#931) completes.
- 202: 100 of 130 references, so the cap row counts the 30 cut.
"""

from datetime import timedelta

import pr_references
from pr_scenarios import FULL, OWNER, REPO, conn, label, reactions, repo_node, user

POPULATED = 201
CLOSED = 198
OVERFLOW = 202


def _comment(line, i, who, body, hours):
    at = line.ago(hours)
    return {
        "id": f"IC_{i}",
        "databaseId": 9100 + i,
        "viewerCanUpdate": False,
        "viewerCanDelete": False,
        "updatedAt": at,
        "author": user(who),
        "body": body,
        "createdAt": at,
        "viewerCanReact": True,
        "reactionGroups": reactions(),
    }


def _event(line, kind, i, who, hours, **fields):
    return {
        "__typename": kind,
        "id": f"{kind[:3].upper()}_{i}",
        "createdAt": line.ago(hours),
        "actor": user(who),
        **fields,
    }


def _issue(line, number, title, **over):
    node = dict(
        id=f"I_{number}",
        number=number,
        title=title,
        body="`parse_manifest` stops at the first trailing comma in a list.",
        state="OPEN",
        stateReason=None,
        closedAt=None,
        url=f"https://github.com/{FULL}/issues/{number}",
        createdAt=line.ago(96),
        author=user("reviewer-a"),
        viewerCanUpdate=True,
        viewerCanLabel=True,
        viewerCanAssign=True,
        viewerCanSetMilestone=True,
        viewerCanClose=True,
        viewerCanReopen=False,
        viewerCanReact=True,
        locked=False,
        activeLockReason=None,
        reactionGroups=reactions(),
        labels=conn([label("bug", "d73a4a")]),
        assignees=conn([]),
        milestone=None,
        comments=conn([]),
        timelineItems={"totalCount": 0, "filteredCount": 0, "nodes": []},
        references=pr_references.references(line),
    )
    node.update(over)
    return node


def _detail(issue, permission="ADMIN"):
    return {
        "data": {
            "rateLimit": None,
            "repository": {
                "nameWithOwner": FULL,
                "viewerPermission": permission,
                "issue": issue,
            },
        }
    }


def populated(line):
    b, a, other = f"{OWNER}/beacon", f"{OWNER}/atlas", "fixture-org/elsewhere"
    pr = pr_references
    events = [_event(line, "LabeledEvent", 1, "reviewer-a", 95, label={"name": "bug"})]
    refs = pr.references(
        line,
        ("octo-dev", 70, pr.issue(57, "Tracking: manifest consumers", b)),
        ("octo-dev", 50, pr.pull(104, "Docs: fix the manifest grammar example", a, state="MERGED")),
        ("octo-dev", 30, pr.issue(12, "Docs: manifest list syntax", other)),
        (None, 20, pr.issue(77, "Loader notes", b)),
        ("octo-dev", 10, pr.pull(103, "WIP: loader cleanup", b, draft=True)),
    )
    # The last one closes this issue when it merges.
    refs["nodes"][-1]["willCloseTarget"] = True
    return _issue(
        line,
        POPULATED,
        "Manifest parser rejects trailing commas",
        comments=conn([_comment(line, 1, "octo-dev", "Reproduced on main.", 60)]),
        timelineItems={"totalCount": 1, "filteredCount": 1, "nodes": events},
        references=refs,
    )


def closed(line):
    b = f"{OWNER}/beacon"
    pr = pr_references
    refs = pr.references(
        line,
        ("octo-dev", 40, pr.pull(99, "Revert parser strictness", b, state="MERGED")),
    )
    refs["nodes"][0]["willCloseTarget"] = True
    return _issue(
        line,
        CLOSED,
        "Trailing comma crash",
        state="CLOSED",
        stateReason="COMPLETED",
        closedAt=line.ago(20),
        # A reader: a locked conversation hides the comment box from
        # one without lock rights, so the keyboard walk (`tab-stops`) is
        # not stopped by the composer's Tab trap (#931).
        viewerCanUpdate=False,
        viewerCanLabel=False,
        viewerCanAssign=False,
        viewerCanSetMilestone=False,
        viewerCanClose=False,
        viewerCanReopen=False,
        locked=True,
        activeLockReason="RESOLVED",
        timelineItems={
            "totalCount": 1,
            "filteredCount": 1,
            "nodes": [_event(line, "ClosedEvent", 1, "octo-dev", 20, stateReason="COMPLETED")],
        },
        references=refs,
    )


def overflow(line):
    return _issue(
        line,
        OVERFLOW,
        "Mentioned from everywhere",
        references=pr_references.overflow(line),
    )


def write_issue_scenarios(write, now, iso, line):
    for builder in (populated, closed, overflow):
        issue = builder(line)
        reader = builder is closed
        write(
            f"graphql/IssueDetail.{issue['number']}.json",
            _detail(issue, "READ" if reader else "ADMIN"),
        )
    rows = []
    for i, issue in enumerate(builder(line) for builder in (populated, closed, overflow)):
        rows.append(
            {
                "databaseId": 60000 + issue["number"],
                "number": issue["number"],
                "title": issue["title"],
                "author": issue["author"],
                "updatedAt": line.ago(1 + i),
                "url": issue["url"],
                "labels": issue["labels"],
            }
        )
    write(
        f"graphql/RepoIssues.{REPO}.json",
        {
            "data": {
                "rateLimit": None,
                "repository": {**repo_node(iso, now), "issues": conn(rows)},
            }
        },
    )
