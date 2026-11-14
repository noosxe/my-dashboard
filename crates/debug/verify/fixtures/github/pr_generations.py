"""Generation 1 of the synthetic PR #101 (docs/0084 §8, docs/0081 §12.1).

Called from generate.py after pr_scenarios.py. Files named
`<name>.g1.json` answer once `dashboard-debug fixture bump` moved the
fixture generation to 1; before that, and for every file without a
generation sibling, the plain names answer. Generation 1 is "a minute
later on GitHub's side", with four changes to #101:

- a new comment (#3);
- the `test (workspace)` check run goes from in progress to success;
- the `needs-review` label is replaced by `approved`;
- a new head commit, whose changed file list gains a sixth file.

The REST files are what the freshness probe reads: `pulls/{n}`,
`commits/{sha}/check-runs` and `commits/{sha}/status` (also
`pulls/{n}/files` for the file list). The GraphQL detail is the same PR
as the page reads it.
"""

import copy
import json
import os
from datetime import timedelta

from pr_scenarios import (
    FULL,
    BRANCH,
    OWNER,
    REPO,
    Timeline,
    changed_files,
    conn,
    file_entry,
    label,
    oid,
    rollup,
)

PR = 101
HERE = os.path.dirname(os.path.abspath(__file__))
NEW_FILE = file_entry(
    "crates/manifest/src/errors.rs",
    "modified",
    6,
    2,
    "@@ -3,4 +3,8 @@ pub enum ManifestError {\n     Lex(String),\n+    Column(usize),\n }\n ",
)


def new_oid():
    """The head after the push: same shape as `oid`, `c` where it has `b`."""
    return f"{PR:07x}" + "c" * 33


def rest_check_run(node, sha):
    """The REST form of one GraphQL check-run context."""
    done = node["status"] == "COMPLETED"
    return {
        "id": node["databaseId"],
        "name": node["name"],
        "head_sha": sha,
        "status": "completed" if done else "in_progress",
        "conclusion": node["conclusion"].lower() if node["conclusion"] else None,
        "started_at": node["startedAt"],
        "completed_at": node["completedAt"],
        "app": {"slug": node["checkSuite"]["app"]["slug"]},
    }


def check_runs_body(contexts, sha):
    runs = [rest_check_run(c, sha) for c in contexts if c["__typename"] == "CheckRun"]
    return {"total_count": len(runs), "check_runs": runs}


def status_body(sha):
    return {
        "state": "success",
        "sha": sha,
        "total_count": 1,
        "statuses": [{"context": "ci/legacy-status", "state": "success"}],
    }


def pull_body(pr, sha, labels, comments, commits, changed, updated_at):
    return {
        "number": PR,
        "state": "open",
        "draft": False,
        "title": pr["title"],
        "updated_at": updated_at,
        "head": {"ref": BRANCH, "sha": sha},
        "base": {"ref": "main"},
        "labels": [{"name": n, "color": c} for n, c in labels],
        "comments": comments,
        "review_comments": 3,
        "commits": commits,
        "changed_files": changed,
    }


def write_generation_one(write, now, iso):
    line = Timeline(now, iso)
    before = line.populated(PR)
    old_sha, sha = before["headRefOid"], new_oid()
    assert old_sha == oid(PR)
    old_contexts = before["statusCheckRollup"]["contexts"]["nodes"]

    # The running check finishes (on the old head; the pushed head's
    # runs are all done a minute later).
    new_contexts = copy.deepcopy(old_contexts)
    running = next(i for i, c in enumerate(new_contexts) if c["status"] == "IN_PROGRESS")
    new_contexts[running] = line.check_run(
        new_contexts[running]["name"], "COMPLETED", "SUCCESS", new_contexts[running]["databaseId"]
    )

    after = copy.deepcopy(before)
    after["headRefOid"] = sha
    after["changedFiles"] = 6
    after["files"] = {"totalCount": 6}
    after["labels"] = conn(
        [label("bug", "d73a4a"), label("area:parser", "1d76db"), label("approved", "0e8a16")]
    )
    after["comments"] = conn(
        before["comments"]["nodes"]
        + [
            {
                **before["comments"]["nodes"][1],
                "id": "IC_3",
                "databaseId": 9003,
                "updatedAt": iso(now - timedelta(minutes=1)),
                "createdAt": iso(now - timedelta(minutes=1)),
                "body": "Pushed the column into the error; checks are green again.",
            }
        ]
    )
    after["statusCheckRollup"] = rollup("FAILURE", new_contexts)

    # GraphQL: the same wrapper as the plain file, with the changed PR.
    with open(os.path.join(HERE, f"graphql/PrDetail.{PR}.json")) as f:
        detail = json.load(f)
    detail["data"]["repository"]["pullRequest"] = after
    write(f"graphql/PrDetail.{PR}.g1.json", detail)

    # REST: what the probe reads.
    prefix = f"rest/repos_{OWNER}_{REPO}"
    old_labels = [(n["name"], n["color"]) for n in before["labels"]["nodes"]]
    new_labels = [(n["name"], n["color"]) for n in after["labels"]["nodes"]]
    updated = iso(now - timedelta(hours=1))
    write(f"{prefix}_pulls_{PR}.json", pull_body(before, old_sha, old_labels, 2, 3, 5, updated))
    write(
        f"{prefix}_pulls_{PR}.g1.json",
        pull_body(after, sha, new_labels, 3, 4, 6, iso(now - timedelta(minutes=1))),
    )
    write(f"{prefix}_commits_{old_sha}_check-runs.json", check_runs_body(old_contexts, old_sha))
    write(f"{prefix}_commits_{old_sha}_check-runs.g1.json", check_runs_body(new_contexts, old_sha))
    write(f"{prefix}_commits_{sha}_check-runs.json", check_runs_body(new_contexts, sha))
    for s in (old_sha, sha):
        write(f"{prefix}_commits_{s}_status.json", status_body(s))
        write(f"{prefix}_commits_{s}_check-suites.json", {"total_count": 0, "check_suites": []})
    # One page of files, nested like the plain file (a body array is a
    # sequence to the transport).
    write(f"{prefix}_pulls_{PR}_files.g1.json", [changed_files(5) + [NEW_FILE]])
