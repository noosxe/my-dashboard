"""The PR page's verification scenarios (docs/0081 §12.1, #769).

Called from generate.py. Every scenario is one open-PR row on
fixture-user/beacon, answered by number: graphql/PrDetail.<n>.json and
REST files keyed by number or head SHA. One session can therefore open
each state in turn. beacon, not atlas, carries the rows: atlas's empty
lane is part of the existing goldens and walk-derived ids.

The shapes follow the queries in crates/modules/github/src/gql.rs,
including their caps: labels, assignees, reviewRequests, latestReviews and
latestOpinionatedReviews first 20;
contexts, commits, reviews and comments first 100. A count above a cap
therefore ships capped nodes with the true totalCount.

`RepoPulls` selects `states: OPEN`, so a merged or closed PR couldn't
really be in the lane; the rows are the only route onto the page, and
they read as a row that went stale after the list landed.
"""

from datetime import timedelta

OWNER = "fixture-user"
REPO = "beacon"
FULL = f"{OWNER}/{REPO}"
BRANCH = "feature/trailing-commas"
RATE = None
REACTIONS = [
    "THUMBS_UP",
    "THUMBS_DOWN",
    "LAUGH",
    "HOORAY",
    "CONFUSED",
    "HEART",
    "ROCKET",
    "EYES",
]
ROW_COLORS = ["d73a4a", "1d76db", "fbca04", "0e8a16", "5319e7"]
GATED_FORK_RUN = 7001
GATED_ENV_RUN = 7002
GATED_ONLY_PR = 109
GATED_ONLY_RUNS = (7003, 7004)
# The threads read is capped (130 threads, one with 130 comments) / fails.
THREADS_CAPPED_PR = 110
THREADS_ERROR_PR = 111
FILES_NOTICES_PR = 112
# docs/0083: the ready-to-merge and the reopenable closed scenarios.
READY_PR = 113
CLOSED_PR = 114
# docs/0083 D4 (#773): merged with the head branch already deleted, and
# merged from a fork the viewer can't write.
MERGED_GONE_PR = 115
MERGED_FORK_PR = 116

BODY = """## Summary

Accept trailing commas in the manifest parser, so hand-edited files stop
failing the load with an opaque error.

- `parse_manifest` tolerates one trailing comma per list
- the error now names the offending line

```rust
let manifest = parse_manifest(input)?;
```

See the [grammar notes](https://example.com/grammar) for the full list.
"""

PARSER_PATCH = """@@ -10,5 +10,6 @@ pub fn parse_manifest(input: &str) -> Result<Manifest> {
     let tokens = lex(input)?;
-    let entries = strict(tokens)?;
+    let entries = tolerant(tokens)
+        .map_err(|e| e.at_line(input))?;
     Ok(Manifest { entries })
 }
 """

TEST_PATCH = """@@ -0,0 +1,4 @@
+#[test]
+fn trailing_comma_is_accepted() {
+    assert!(parse_manifest("[a, b,]").is_ok());
+}"""

DOCS_PATCH = """@@ -1,2 +1 @@
-Lists reject a trailing comma.
 Comments start with a hash."""


# A hunk header that does not parse (#753).
MALFORMED_PATCH = """@@ -x,y +z @@
+let broken = true;
"""


def oid(number):
    return f"{number:07x}" + "b" * 33


def user(login, with_id=False):
    node = {"login": login, "avatarUrl": f"https://avatars.invalid/{login}"}
    if with_id:
        node["id"] = f"U_{login}"
    return node


def reactions(thumbs=0, heart=0, mine=False):
    counts = {"THUMBS_UP": thumbs, "HEART": heart}
    return [
        {
            "content": kind,
            "viewerHasReacted": mine and kind == "THUMBS_UP",
            "reactors": {"totalCount": counts.get(kind, 0)},
        }
        for kind in REACTIONS
    ]


def label(name, color):
    return {"id": f"L_{name}", "name": name, "color": color}


def conn(nodes, total=None):
    return {"totalCount": len(nodes) if total is None else total, "nodes": nodes}


def requests(*logins, teams=0, enterprise_teams=0, bots=()):
    """User requests, then named teams, enterprise teams (no avatar) and bots."""
    nodes = [{"requestedReviewer": {"__typename": "User", **user(login, True)}} for login in logins]
    nodes += [
        {
            "requestedReviewer": {
                "__typename": "Team",
                "id": f"T_team-{i + 1:02d}",
                "name": f"team-{i + 1:02d}",
                "avatarUrl": f"https://avatars.invalid/team-{i + 1:02d}",
            }
        }
        for i in range(teams)
    ]
    nodes += [
        {
            "requestedReviewer": {
                "__typename": "EnterpriseTeam",
                "id": f"ET_platform-{i + 1:02d}",
                "name": f"platform-{i + 1:02d}",
            }
        }
        for i in range(enterprise_teams)
    ]
    nodes += [{"requestedReviewer": {"__typename": "Bot", **user(login, True)}} for login in bots]
    return conn(nodes)


def latest_reviews(reviews):
    """The review fields the rail reads, derived from the timeline's reviews.

    `latestReviews` is the latest non-pending review per author;
    `latestOpinionatedReviews` the latest approval or change request per
    author, so a commenter-only reviewer is in the first and not the
    second. `reviewDecision` follows the opinionated list.
    """
    latest, opinionated = {}, {}
    for r in reviews["nodes"]:
        who = r["author"]["login"] if r["author"] else None
        if who is None or r["state"] == "PENDING":
            continue
        node = {"id": r["id"], "state": r["state"], "author": r["author"]}
        latest[who] = node
        if r["state"] in ("APPROVED", "CHANGES_REQUESTED"):
            opinionated[who] = node
    states = {n["state"] for n in opinionated.values()}
    decision = (
        "CHANGES_REQUESTED"
        if "CHANGES_REQUESTED" in states
        else "APPROVED" if "APPROVED" in states else None
    )
    return {
        "reviewDecision": decision,
        "latestOpinionatedReviews": {"nodes": list(opinionated.values())},
        "latestReviews": {"nodes": list(latest.values())},
    }


def status_context(context, state):
    return {
        "__typename": "StatusContext",
        "context": context,
        "state": state,
        "targetUrl": "https://ci.invalid/status",
    }


def counts_by_state(contexts, extra=()):
    """The two `...CountsByState` lists of `contexts`, derived from its nodes.

    A check run counts under its conclusion once it completed, else under
    its status; a commit status under its state. `extra` is `(state,
    count)` pairs for what a capped page doesn't list, so the counts stay
    exact where the nodes are not.
    """
    runs, statuses = {}, {}
    for node in contexts:
        if node["__typename"] == "CheckRun":
            done = node["status"] == "COMPLETED"
            state = node["conclusion"] if done else node["status"]
            runs[state] = runs.get(state, 0) + 1
        else:
            statuses[node["state"]] = statuses.get(node["state"], 0) + 1
    for state, count in extra:
        runs[state] = runs.get(state, 0) + count
    as_list = lambda d: [{"state": k, "count": v} for k, v in d.items()]
    return as_list(runs), as_list(statuses)


def rollup(state, contexts, total=None, extra=()):
    runs, statuses = counts_by_state(contexts, extra)
    connection = conn(contexts, total)
    connection["checkRunCountsByState"] = runs
    connection["statusContextCountsByState"] = statuses
    return {"state": state, "contexts": connection}


class Timeline:
    """Node builders on the capture clock, so relative ages stay stable."""

    def __init__(self, now, iso):
        self.now = now
        self.iso = iso

    def ago(self, hours):
        return self.iso(self.now - timedelta(hours=hours))

    def check_run(self, name, status, conclusion, job=1, app="github-actions", workflow="CI"):
        """A run that finished an hour ago, or one started three minutes ago."""
        actions = app == "github-actions"
        ended = self.now - timedelta(hours=1)
        # A skipped run reports the same start and end upstream.
        took = 0 if conclusion == "SKIPPED" else 38 + job % 9 * 37
        running = status != "COMPLETED"
        started = self.now - timedelta(minutes=3) if running else ended - timedelta(seconds=took)
        return {
            "__typename": "CheckRun",
            "databaseId": job,
            "name": name,
            "status": status,
            "conclusion": conclusion,
            "detailsUrl": f"https://github.com/{FULL}/actions/runs/1/job/{job}",
            "startedAt": self.iso(started),
            "completedAt": None if running else self.iso(ended),
            "checkSuite": {
                "databaseId": job * 10,
                "app": {"slug": app},
                "workflowRun": {"workflow": {"name": workflow}} if actions else None,
            },
        }

    def review(self, i, who, state, body, hours, mine=False):
        at = self.ago(hours)
        return {
            "id": f"PRR_{i}",
            "viewerCanUpdate": mine,
            "viewerCanDelete": mine,
            "updatedAt": at,
            "author": user(who) if who else None,
            "state": state,
            "body": body,
            # A pending review has no submission time upstream.
            "submittedAt": None if state == "PENDING" else at,
            "viewerCanReact": state != "PENDING",
            "reactionGroups": reactions(),
        }

    def comment(self, i, who, body, hours, mine=False, thumbs=0):
        at = self.ago(hours)
        return {
            "id": f"IC_{i}",
            "databaseId": 9000 + i,
            "viewerCanUpdate": mine,
            "viewerCanDelete": mine,
            "updatedAt": at,
            "author": user(who) if who else None,
            "body": body,
            "createdAt": at,
            "viewerCanReact": True,
            "reactionGroups": reactions(thumbs=thumbs),
        }

    def commits(self, count, total=None, who="octo-dev"):
        headlines = [
            "parser: accept trailing commas in manifest lists",
            "tests: cover the trailing-comma cases",
            "docs: note the manifest grammar change",
        ]
        nodes = [
            {
                "commit": {
                    "oid": f"{i + 1:07x}" + "a" * 33,
                    "messageHeadline": headlines[i % len(headlines)],
                    "committedDate": self.ago(72 - (i % 48)),
                    "author": {
                        "name": who or "Unlinked Author",
                        "avatarUrl": f"https://avatars.invalid/{who or 'unlinked'}",
                        "user": {"login": who} if who else None,
                    },
                }
            }
            for i in range(count)
        ]
        return conn(nodes, total)

    def pull_request(self, number, title, **over):
        node = dict(
            id=f"PR_{number}",
            number=number,
            title=title,
            body=BODY,
            state="OPEN",
            isDraft=False,
            changedFiles=3,
            baseRefName="main",
            headRefName=BRANCH,
            headRefOid=oid(number),
            url=f"https://github.com/{FULL}/pull/{number}",
            createdAt=self.ago(96),
            author=user("octo-dev"),
            viewerCanUpdate=True,
            viewerCanUpdateBranch=False,
            viewerCanClose=True,
            viewerCanReopen=False,
            viewerCanReact=True,
            viewerCanLabel=True,
            viewerCanAssign=True,
            labels=conn([]),
            assignees=conn([]),
            milestone=None,
            reviewRequests=conn([]),
            reactionGroups=reactions(),
            mergeable="MERGEABLE",
            mergeStateStatus="CLEAN",
            statusCheckRollup=None,
            merged=False,
            mergedAt=None,
            mergedBy=None,
            closedAt=None,
            # `timelineItems(last: 1, itemTypes: [CLOSED_EVENT])`.
            timelineItems={"nodes": []},
            # Delete branch (#773): `headRef` is null once the branch is
            # gone; the gate is the viewer's permission on the head
            # repository and its default branch.
            headRef={"id": f"REF_{number}", "name": BRANCH},
            headRepository={"viewerPermission": "ADMIN", "defaultBranchRef": {"name": "main"}},
            isInMergeQueue=False,
            mergeQueueEntry=None,
            autoMergeRequest=None,
            commits=self.commits(3),
            reviews=conn([]),
            comments=conn([]),
            files={"totalCount": 3},
        )
        node.update(over)
        node.update(latest_reviews(node["reviews"]))
        return node

    def populated(self, number):
        contexts = [
            self.check_run("build (ubuntu-latest)", "COMPLETED", "SUCCESS", 1),
            self.check_run("build (macos-latest)", "COMPLETED", "FAILURE", 2),
            self.check_run("clippy", "COMPLETED", "SUCCESS", 3, workflow="Lint"),
            self.check_run("test (workspace)", "IN_PROGRESS", None, 4),
            self.check_run("deploy-preview", "COMPLETED", "SKIPPED", 5, workflow="Preview"),
            self.check_run("external-scanner", "COMPLETED", "SUCCESS", 6, app="scanner-app"),
            status_context("ci/legacy-status", "SUCCESS"),
        ]
        return self.pull_request(
            number,
            "Parser: accept trailing commas in manifest lists",
            changedFiles=5,
            files={"totalCount": 5},
            labels=conn(
                [
                    label("bug", "d73a4a"),
                    label("area:parser", "1d76db"),
                    label("needs-review", "fbca04"),
                ]
            ),
            assignees=conn([user("octo-dev", True), user(OWNER, True)]),
            milestone={"id": "M_1", "title": "v1.4 parser cleanup", "state": "OPEN"},
            reviewRequests=requests("reviewer-d", "reviewer-e", teams=1, bots=("review-bot",)),
            reactionGroups=reactions(thumbs=2, heart=1, mine=True),
            mergeStateStatus="BLOCKED",
            statusCheckRollup=rollup("FAILURE", contexts),
            reviews=conn(
                [
                    self.review(1, "reviewer-a", "APPROVED", "", 50),
                    self.review(
                        2,
                        "reviewer-b",
                        "CHANGES_REQUESTED",
                        "The error should carry the column as well.",
                        46,
                    ),
                    self.review(3, "reviewer-c", "COMMENTED", "", 28),
                ]
            ),
            comments=conn(
                [
                    self.comment(
                        1,
                        OWNER,
                        "Looks close. Can we keep the old error for *strict* mode?",
                        49,
                        mine=True,
                        thumbs=1,
                    ),
                    self.comment(
                        2,
                        "octo-dev",
                        "Good point, a `strict` flag landed in the follow-up commit.",
                        27,
                    ),
                ]
            ),
        )

    def overflow(self, number):
        title = (
            "Parser and loader overhaul: accept trailing commas, keep the strict-mode "
            "error, split the manifest grammar into its own module and migrate every "
            "call site in the workspace to the new entry points, now"
        )
        assert len(title) == 200, len(title)
        job_name = "matrix (os-{:02d}, a long job name that keeps going to stress the row width)"
        return self.pull_request(
            number,
            title,
            headRefName="feature/very-long-branch-name-for-the-manifest-parser-and-loader-overhaul",
            labels=conn(
                [
                    label(f"area:subsystem-{i:02d}", ROW_COLORS[i % len(ROW_COLORS)])
                    for i in range(20)
                ],
                34,
            ),
            assignees=conn([user(f"assignee-{i:02d}", True) for i in range(12)]),
            milestone={
                "id": "M_2",
                "title": "2026 Q4 stabilisation milestone with a really long descriptive name",
                "state": "CLOSED",
            },
            reviewRequests=requests(*[f"reviewer-{i:02d}" for i in range(10)], teams=2, enterprise_teams=1),
            viewerCanUpdateBranch=True,
            mergeStateStatus="BEHIND",
            commits=self.commits(100, 137),
            statusCheckRollup=rollup(
                "PENDING",
                [
                    self.check_run(
                        job_name.format(i),
                        "COMPLETED" if i % 10 else "IN_PROGRESS",
                        "SUCCESS" if i % 10 else None,
                        100 + i,
                    )
                    for i in range(100)
                ],
                137,
                # The 37 the 100-node page doesn't list all passed.
                extra=[("SUCCESS", 37)],
            ),
            reviews=conn(
                [
                    self.review(i + 1, "reviewer-a", "COMMENTED", f"Review note {i}", 90 - i)
                    for i in range(12)
                ]
            ),
            comments=conn(
                # The newest 100 of 240 (`last: 100`, ascending): the 140
                # older ones are the cap row's count.
                [self.comment(i + 1, "octo-dev", f"Comment {140 + i}", 80 - i * 0.5) for i in range(100)],
                240,
            ),
        )

    def closed_by(self, login, hours):
        """`timelineItems(last: 1, itemTypes: [CLOSED_EVENT])` for the closer."""
        return {"nodes": [{"actor": user(login), "createdAt": self.ago(hours)}]}

    def scenarios(self):
        """Number → the PullRequest node and its repository envelope."""
        merged_checks = [
            self.check_run("build", "COMPLETED", "SUCCESS", 1),
            self.check_run("clippy", "COMPLETED", "SUCCESS", 2),
        ]
        read_only = dict(viewerCanClose=False, viewerCanUpdate=False, viewerCanLabel=False, viewerCanAssign=False)
        return {
            101: {"pr": self.populated(101)},
            102: {"pr": self.overflow(102)},
            103: {
                "pr": self.pull_request(
                    103,
                    "WIP: loader cleanup",
                    # `PullRequest.body` is non-null upstream: empty, not null.
                    body="",
                    isDraft=True,
                    mergeStateStatus="DRAFT",
                    changedFiles=1,
                    files={"totalCount": 1},
                    commits=self.commits(1),
                )
            },
            104: {
                "methods": (False, True, False),
                "pr": self.pull_request(
                    104,
                    "Docs: fix the manifest grammar example",
                    state="MERGED",
                    merged=True,
                    mergedAt=self.ago(72),
                    mergedBy=user("octo-dev"),
                    closedAt=self.ago(72),
                    mergeable="UNKNOWN",
                    mergeStateStatus="UNKNOWN",
                    changedFiles=1,
                    files={"totalCount": 1},
                    commits=self.commits(1),
                    labels=conn([label("docs", "0e8a16")]),
                    assignees=conn([user("octo-dev", True)]),
                    statusCheckRollup=rollup("SUCCESS", merged_checks),
                    reviews=conn([self.review(1, "reviewer-a", "APPROVED", "", 50)]),
                ),
            },
            105: {
                "permission": "READ",
                "pr": self.pull_request(
                    105,
                    "Loader: drop the legacy manifest format",
                    state="CLOSED",
                    closedAt=self.ago(48),
                    timelineItems=self.closed_by(OWNER, 48),
                    mergeable="CONFLICTING",
                    mergeStateStatus="DIRTY",
                    **read_only,
                ),
            },
            106: {
                "permission": "READ",
                "pr": self.pull_request(
                    106,
                    "Tests: widen the manifest round-trip corpus",
                    statusCheckRollup=rollup(
                        "SUCCESS", [self.check_run("build", "COMPLETED", "SUCCESS", 1)]
                    ),
                    **read_only,
                ),
            },
            # The viewer's last-used method (squash) is no longer allowed
            # here: the button falls back to the first allowed (#776).
            107: {
                "methods": (True, False, False),
                "default_method": "SQUASH",
                "pr": self.pull_request(
                    107,
                    "Parser: report the column in manifest errors",
                    author=None,
                    mergeStateStatus="UNSTABLE",
                    commits=self.commits(2, who=None),
                    statusCheckRollup=rollup(
                        "FAILURE",
                        [
                            self.check_run("build", "COMPLETED", "SUCCESS", 1),
                            self.check_run("build", "COMPLETED", "FAILURE", 2, workflow="Release"),
                            self.check_run("lint", "COMPLETED", "CANCELLED", 3),
                        ],
                    ),
                    reviews=conn(
                        [
                            self.review(9, OWNER, "PENDING", "", 29, mine=True),
                            self.review(1, None, "DISMISSED", "Superseded.", 50),
                            self.review(2, "reviewer-b", "COMMENTED", "", 48),
                        ]
                    ),
                    comments=conn([self.comment(1, None, "Ghost-authored comment.", 49)]),
                ),
            },
            # The only CI is held for approval: no rollup, two gated runs.
            GATED_ONLY_PR: {
                "pr": self.pull_request(
                    GATED_ONLY_PR,
                    "CI: a fork PR whose only runs wait for approval",
                ),
            },
            THREADS_CAPPED_PR: {
                "pr": self.pull_request(
                    THREADS_CAPPED_PR,
                    "Review: more threads than one read carries",
                ),
            },
            THREADS_ERROR_PR: {
                "pr": self.pull_request(
                    THREADS_ERROR_PR,
                    "Review: the threads read answers with an error",
                ),
            },
            # #753/#760: the Files tab's undrawable diffs and a thread
            # on a path no longer in the list.
            FILES_NOTICES_PR: {
                "pr": self.pull_request(
                    FILES_NOTICES_PR,
                    "Files: diffs that can't be drawn",
                    changedFiles=4,
                    files={"totalCount": 4},
                ),
            },
            # docs/0083 D4: a writer's open PR that can merge: clean,
            # approved, passing, behind its base, squash and merge allowed.
            # All three methods are allowed and the viewer's last-used one
            # is squash, so the split button's preselection (#776) differs
            # from the merge-commit rule it would otherwise follow.
            READY_PR: {
                "methods": (True, True, True),
                "default_method": "SQUASH",
                "permission": "WRITE",
                "pr": self.pull_request(
                    READY_PR,
                    "Docs: explain the manifest grammar",
                    viewerCanUpdateBranch=True,
                    changedFiles=2,
                    files={"totalCount": 2},
                    commits=self.commits(2),
                    labels=conn([label("docs", "0e8a16")]),
                    assignees=conn([user("octo-dev", True)]),
                    statusCheckRollup=rollup("SUCCESS", merged_checks),
                    reviews=conn(
                        [
                            self.review(1, "reviewer-a", "APPROVED", "", 50),
                            self.review(2, "reviewer-c", "APPROVED", "", 30),
                        ]
                    ),
                ),
            },
            # docs/0083 D6: closed, not merged, the head branch still
            # there; the viewer may reopen it.
            CLOSED_PR: {
                "pr": self.pull_request(
                    CLOSED_PR,
                    "Loader: try the lazy manifest reader",
                    state="CLOSED",
                    closedAt=self.ago(48),
                    timelineItems=self.closed_by("octo-dev", 48),
                    # Reopen reads `viewerCanReopen` (#770); Delete branch
                    # reads the head repository's permission (#773). The
                    # schema's `viewerCanDeleteHeadRef` is a restore flag,
                    # always false in the cases checked, and no gate.
                    viewerCanReopen=True,
                    viewerCanDeleteHeadRef=False,
                    changedFiles=1,
                    files={"totalCount": 1},
                    commits=self.commits(1),
                    assignees=conn([user("octo-dev", True)]),
                    labels=conn([label("wontfix", "fbca04")]),
                    reviews=conn([self.review(1, "reviewer-b", "CHANGES_REQUESTED", "Not now.", 50)]),
                ),
            },
            # docs/0083 D4 (#773): merged, the head branch already deleted
            # (the repository deletes on merge): no Delete branch.
            MERGED_GONE_PR: {
                "methods": (False, True, False),
                "pr": self.pull_request(
                    MERGED_GONE_PR,
                    "Docs: fix the lockfile example",
                    state="MERGED",
                    merged=True,
                    mergedAt=self.ago(30),
                    mergedBy=user("octo-dev"),
                    closedAt=self.ago(30),
                    mergeable="UNKNOWN",
                    mergeStateStatus="UNKNOWN",
                    headRef=None,
                    changedFiles=1,
                    files={"totalCount": 1},
                    commits=self.commits(1),
                    statusCheckRollup=rollup("SUCCESS", merged_checks),
                ),
            },
            # docs/0083 D4 (#773): merged from a fork. The viewer writes the
            # base repository (ADMIN) but only reads the fork, so the branch
            # is not theirs to delete.
            MERGED_FORK_PR: {
                "methods": (False, True, False),
                "pr": self.pull_request(
                    MERGED_FORK_PR,
                    "Docs: reword the changelog",
                    state="MERGED",
                    merged=True,
                    mergedAt=self.ago(20),
                    mergedBy=user("octo-dev"),
                    closedAt=self.ago(20),
                    mergeable="UNKNOWN",
                    mergeStateStatus="UNKNOWN",
                    isCrossRepository=True,
                    headRepository={"viewerPermission": "READ", "defaultBranchRef": {"name": "main"}},
                    changedFiles=1,
                    files={"totalCount": 1},
                    commits=self.commits(1),
                    statusCheckRollup=rollup("SUCCESS", merged_checks),
                ),
            },
        }

    def thread(self, i, path, line, hours, body, **over):
        at = self.ago(hours)
        node = dict(
            id=f"PRRT_{i}",
            path=path,
            line=line,
            startLine=None,
            diffSide="RIGHT",
            isOutdated=False,
            isResolved=False,
            resolvedBy=None,
            subjectType="LINE",
            viewerCanReply=True,
            viewerCanResolve=True,
            viewerCanUnresolve=False,
            comments=conn(
                [
                    {
                        "id": f"PRRC_{i}",
                        "author": user("reviewer-b"),
                        "body": body,
                        "createdAt": at,
                        "updatedAt": at,
                        "lastEditedAt": None,
                        "url": f"https://github.com/{FULL}/pull/101#discussion_r{i}",
                        "viewerCanUpdate": False,
                        "viewerCanDelete": False,
                        "viewerDidAuthor": False,
                    }
                ]
            ),
        )
        node.update(over)
        return node

    def populated_threads(self):
        parser = "crates/manifest/src/parser.rs"
        return [
            self.thread(1, parser, 11, 46, "Should this keep the original span?"),
            self.thread(2, parser, 12, 45, "Multi-line: the map_err could move up.", startLine=11),
            self.thread(
                3,
                parser,
                None,
                44,
                "An older note on the first revision.",
                isOutdated=True,
            ),
            self.thread(
                4,
                "docs/grammar.md",
                None,
                43,
                "The whole page needs a pass.",
                subjectType="FILE",
                isResolved=True,
                resolvedBy={"login": OWNER},
                viewerCanResolve=False,
                viewerCanUnresolve=True,
            ),
        ]

    def gated_runs(self, head_sha, gated_only_sha):
        def run(run_id, name, run_number, hours, sha=head_sha):
            return {
                "id": run_id,
                "name": name,
                "head_sha": sha,
                "head_branch": BRANCH,
                "run_number": run_number,
                "status": "completed",
                "conclusion": "action_required",
                "event": "pull_request",
                "created_at": self.ago(hours),
            }

        runs = [
            run(GATED_FORK_RUN, "fork-ci", 311, 3),
            run(GATED_ENV_RUN, "deploy-staging", 312, 2),
            run(GATED_ONLY_RUNS[0], "fork-ci", 321, 5, gated_only_sha),
            run(GATED_ONLY_RUNS[1], "fork-lint", 322, 5, gated_only_sha),
        ]
        return {"total_count": len(runs), "workflow_runs": runs}


def file_entry(filename, status, additions, deletions, patch=None, **over):
    entry = {
        "filename": filename,
        "status": status,
        "additions": additions,
        "deletions": deletions,
        "changes": additions + deletions,
    }
    # `patch` is absent for binary and oversized files upstream.
    if patch is not None:
        entry["patch"] = patch
    entry.update(over)
    return entry


def changed_files(count):
    files = [
        file_entry("crates/manifest/src/parser.rs", "modified", 2, 1, PARSER_PATCH),
        file_entry("crates/manifest/tests/trailing.rs", "added", 4, 0, TEST_PATCH),
        file_entry("docs/grammar.md", "modified", 0, 1, DOCS_PATCH),
        file_entry("assets/manifest-diagram.png", "added", 0, 0),
        file_entry(
            "crates/manifest/src/grammar.rs",
            "renamed",
            0,
            0,
            previous_filename="crates/manifest/src/syntax.rs",
        ),
    ]
    return files[:count]


def notice_files():
    """#753: one file per notice, and one that draws."""
    return [
        file_entry("crates/manifest/src/lexer.rs", "modified", 1, 0, MALFORMED_PATCH),
        file_entry("crates/manifest/src/generated.rs", "modified", 5000, 4000),
        file_entry("assets/manifest-diagram.png", "added", 0, 0),
        file_entry("crates/manifest/tests/trailing.rs", "added", 4, 0, TEST_PATCH),
    ]


def repo_node(iso, now):
    return {
        "databaseId": 2001,
        "id": f"R_{REPO}",
        "hasIssuesEnabled": True,
        "name": REPO,
        "nameWithOwner": FULL,
        "description": "A second fixture repository",
        "isPrivate": False,
        "isArchived": False,
        "pushedAt": iso(now),
        "owner": {"login": OWNER},
    }


def lane_row(pr, updated_at):
    labels = pr["labels"]
    wanted = [r["requestedReviewer"] for r in pr["reviewRequests"]["nodes"]]
    rollup_node = pr["statusCheckRollup"]
    return {
        "databaseId": 50000 + pr["number"],
        "number": pr["number"],
        "title": pr["title"],
        "isDraft": pr["isDraft"],
        "state": "OPEN",
        "author": pr["author"],
        "updatedAt": updated_at,
        "url": pr["url"],
        "statusCheckRollup": {"state": rollup_node["state"]} if rollup_node else None,
        "labels": {
            "totalCount": labels["totalCount"],
            "nodes": [{"name": n["name"], "color": n["color"]} for n in labels["nodes"][:8]],
        },
        "reviewRequests": {
            "totalCount": len(wanted),
            "nodes": [
                {
                    # The list query selects `... on User { login }` only.
                    "requestedReviewer": {
                        k: v
                        for k, v in who.items()
                        if k == "__typename" or (k == "login" and who["__typename"] == "User")
                    }
                }
                for who in wanted
            ],
        },
    }


def write_pr_scenarios(write, now, iso):
    line = Timeline(now, iso)
    scenarios = line.scenarios()
    for number, scenario in scenarios.items():
        merge, squash, rebase = scenario.get("methods", (True, True, True))
        pr = scenario["pr"]
        # A same-repository head is the base repository: the viewer's
        # permission there is the scenario's (a fork overrides it).
        head_repo = pr.get("headRepository")
        if head_repo and head_repo["viewerPermission"] == "ADMIN":
            head_repo["viewerPermission"] = scenario.get("permission", "ADMIN")
        write(
            f"graphql/PrDetail.{number}.json",
            {
                "data": {
                    "rateLimit": RATE,
                    "repository": {
                        "mergeCommitAllowed": merge,
                        "squashMergeAllowed": squash,
                        "rebaseMergeAllowed": rebase,
                        "viewerDefaultMergeMethod": scenario.get("default_method", "MERGE"),
                        "viewerPermission": scenario.get("permission", "ADMIN"),
                        "pullRequest": pr,
                    },
                }
            },
        )
        # A body array is a sequence to the transport, so the one page of
        # files nests: the first request (and every later one) gets it.
        write(
            f"rest/repos_{OWNER}_{REPO}_pulls_{number}_files.json",
            [notice_files() if number == FILES_NOTICES_PR else changed_files(pr["changedFiles"])],
        )
        write(
            f"rest/repos_{OWNER}_{REPO}_commits_{pr['headRefOid']}_check-suites.json",
            {"total_count": 0, "check_suites": []},
        )

    # The read fails upstream with 200 + errors and a null PR: the page
    # keeps the row's header and shows the load error.
    missing = 108
    write(
        f"graphql/PrDetail.{missing}.json",
        {
            "data": {"rateLimit": RATE, "repository": {"pullRequest": None}},
            "errors": [
                {
                    "type": "NOT_FOUND",
                    "path": ["repository", "pullRequest"],
                    "message": f"Could not resolve to a PullRequest with the number of {missing}.",
                }
            ],
        },
    )

    rows = [
        lane_row(scenario["pr"], line.ago(1 + i))
        for i, scenario in enumerate(scenarios.values())
        if scenario["pr"]["number"] not in (GATED_ONLY_PR, THREADS_CAPPED_PR, THREADS_ERROR_PR, FILES_NOTICES_PR, READY_PR, CLOSED_PR, MERGED_GONE_PR, MERGED_FORK_PR)
    ]
    error_row = lane_row(
        line.pull_request(missing, "Loader: a PR that fails its detail read"), line.ago(9)
    )
    rows.append(error_row)
    # Last, so the rows above keep the positions the receipts click.
    rows.append(lane_row(scenarios[GATED_ONLY_PR]["pr"], line.ago(10)))
    rows.append(lane_row(scenarios[THREADS_CAPPED_PR]["pr"], line.ago(11)))
    rows.append(lane_row(scenarios[THREADS_ERROR_PR]["pr"], line.ago(12)))
    rows.append(lane_row(scenarios[FILES_NOTICES_PR]["pr"], line.ago(13)))
    rows.append(lane_row(scenarios[READY_PR]["pr"], line.ago(14)))
    rows.append(lane_row(scenarios[CLOSED_PR]["pr"], line.ago(15)))
    rows.append(lane_row(scenarios[MERGED_GONE_PR]["pr"], line.ago(16)))
    rows.append(lane_row(scenarios[MERGED_FORK_PR]["pr"], line.ago(17)))
    write(
        f"graphql/RepoPulls.{REPO}.json",
        {
            "data": {
                "rateLimit": RATE,
                "repository": {**repo_node(iso, now), "pullRequests": conn(rows)},
            }
        },
    )

    # The threads answer by number too; every PR but the populated one
    # reads the plain, empty file.
    def threads(number, nodes):
        return {
            "data": {
                "rateLimit": RATE,
                "repository": {
                    "pullRequest": {"id": f"PR_{number}", "reviewThreads": conn(nodes)}
                },
            }
        }

    write("graphql/PrThreads.json", threads(0, []))
    write("graphql/PrThreads.101.json", threads(101, line.populated_threads()))

    # #745: 130 threads upstream, the read carries the first 100 (two
    # here); the first thread has 130 comments, one carried.
    capped = line.populated_threads()[:2]
    capped[0]["comments"]["totalCount"] = 130
    capped_body = threads(THREADS_CAPPED_PR, capped)
    capped_body["data"]["repository"]["pullRequest"]["reviewThreads"]["totalCount"] = 130
    write(f"graphql/PrThreads.{THREADS_CAPPED_PR}.json", capped_body)
    # #760: one thread on a path the list no longer has, one on the
    # binary file (a notice, no diff), one resolved on the file that
    # draws — two unresolved.
    write(
        f"graphql/PrThreads.{FILES_NOTICES_PR}.json",
        threads(
            FILES_NOTICES_PR,
            [
                line.thread(1, "crates/manifest/src/old_name.rs", 7, 40, "Moved in the force-push."),
                line.thread(2, "assets/manifest-diagram.png", None, 39, "Is this the final diagram?", subjectType="FILE"),
                line.thread(
                    3,
                    "crates/manifest/tests/trailing.rs",
                    2,
                    38,
                    "Looks right.",
                    isResolved=True,
                    resolvedBy={"login": OWNER},
                    viewerCanResolve=False,
                    viewerCanUnresolve=True,
                ),
            ],
        ),
    )
    # A 200 with `data: null` and errors, like a rate limit or SAML wall.
    write(
        f"graphql/PrThreads.{THREADS_ERROR_PR}.json",
        {"data": None, "errors": [{"message": "API rate limit exceeded for user"}]},
    )

    # The sweep's rows are filtered by head SHA client-side, so one file
    # serves every PR and only the populated one shows them. The gate
    # probe tells the two gates apart: empty = fork gate, a pending
    # environment = environment gate.
    runs_prefix = f"rest/repos_{OWNER}_{REPO}_actions_runs"
    write(f"{runs_prefix}.json", line.gated_runs(oid(101), oid(GATED_ONLY_PR)))
    for run_id in GATED_ONLY_RUNS:
        write(f"{runs_prefix}_{run_id}_pending_deployments.json", [])
    write(f"{runs_prefix}_{GATED_FORK_RUN}_pending_deployments.json", [])
    write(
        f"{runs_prefix}_{GATED_ENV_RUN}_pending_deployments.json",
        [
            [
                {
                    "environment": {
                        "id": 41,
                        "name": "staging",
                        "html_url": f"https://github.com/{FULL}/deployments/activity_log?environments_filter=staging",
                    },
                    "wait_timer": 0,
                    "current_user_can_approve": True,
                    "reviewers": [],
                }
            ]
        ],
    )
    write(f"{runs_prefix}_{GATED_ENV_RUN}_approvals.json", [])

    # The rail pickers' source pages; repository-independent answers.
    picker_labels = [label(f"label-{i:02d}", "1d76db") for i in range(60)]
    picker_labels += [label("bug", "d73a4a"), label("area:parser", "1d76db")]
    write(
        "graphql/RepoLabels.json",
        {"data": {"rateLimit": RATE, "repository": {"labels": {"nodes": picker_labels}}}},
    )
    people = [user(f"user-{i:02d}", True) for i in range(50)]
    people += [user("octo-dev", True), user(OWNER, True)]
    write(
        "graphql/RepoAssignableUsers.json",
        {"data": {"rateLimit": RATE, "repository": {"assignableUsers": {"nodes": people}}}},
    )
    write(
        "graphql/RepoMilestones.json",
        {
            "data": {
                "rateLimit": RATE,
                "repository": {
                    "milestones": {
                        "nodes": [
                            {"id": "M_1", "title": "v1.4 parser cleanup", "state": "OPEN"},
                            {"id": "M_3", "title": "v1.5", "state": "OPEN"},
                        ]
                    }
                },
            }
        },
    )
