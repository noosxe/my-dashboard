"""The cross-reference rows' fixtures (docs/0085 D9): the `references:
timelineItems(last: 100, itemTypes: [CROSS_REFERENCED_EVENT])` selection of
the PR detail read. Synthetic sources only; `fixture-user/atlas` and
`fixture-user/beacon` are the repositories the module lists (so a source
there opens in the app), `fixture-org/elsewhere` is not (it opens the
browser)."""

OWNER = "fixture-user"


def _repo(full):
    return {"nameWithOwner": full, "isPrivate": False}


def issue(number, title, repo, state="OPEN", reason=None):
    return {
        "__typename": "Issue",
        "number": number,
        "title": title,
        "state": state,
        "stateReason": reason,
        "url": f"https://github.com/{repo}/issues/{number}",
        "repository": _repo(repo),
    }


def pull(number, title, repo, state="OPEN", draft=False):
    return {
        "__typename": "PullRequest",
        "number": number,
        "title": title,
        "state": state,
        "isDraft": draft,
        "merged": state == "MERGED",
        "url": f"https://github.com/{repo}/pull/{number}",
        "repository": _repo(repo),
    }


def references(line, *items, total=None):
    """The `references` connection, oldest first. Each item is (actor login
    or None, hours ago, source). `total` is the server's `filteredCount`
    when the window carries fewer than it counts."""
    nodes = []
    for ix, (login, hours, source) in enumerate(items):
        nodes.append(
            {
                "__typename": "CrossReferencedEvent",
                "id": f"XREF_{ix}",
                "createdAt": line.ago(hours),
                "actor": {"login": login} if login else None,
                "isCrossRepository": source["repository"]["nameWithOwner"] != f"{OWNER}/beacon",
                "willCloseTarget": False,
                "source": source,
            }
        )
    return {"filteredCount": total if total is not None else len(nodes), "nodes": nodes}


LONG_TITLE = (
    "Tracking: migrate every manifest consumer to the new loader entry points, "
    "drop the legacy reader and document the strict-mode error in the guide"
)


def populated(line):
    """PR 101: every state, a cross-repository and an untracked source, a
    deleted actor and a long title."""
    b, a, other = f"{OWNER}/beacon", f"{OWNER}/atlas", "fixture-org/elsewhere"
    return references(
        line,
        ("reviewer-a", 70, issue(201, "Manifest parser rejects trailing commas", b)),
        ("octo-dev", 66, pull(103, "WIP: loader cleanup", b, draft=True)),
        ("octo-dev", 50, pull(104, "Docs: fix the manifest grammar example", a, state="MERGED")),
        ("reviewer-b", 40, pull(99, "Revert parser strictness", b, state="CLOSED")),
        ("octo-dev", 30, issue(12, "Docs: manifest list syntax", other)),
        (None, 20, issue(198, "Trailing comma crash", b, state="CLOSED", reason="COMPLETED")),
        ("octo-dev", 10, issue(57, LONG_TITLE, a, state="CLOSED", reason="NOT_PLANNED")),
    )


def overflow(line):
    """PR 102: the newest 100 of 130, so the cap row counts 30 more."""
    b = f"{OWNER}/beacon"
    return references(
        line,
        *[
            ("octo-dev", 90 - i * 0.5, issue(300 + i, f"Mention {i}", b))
            for i in range(100)
        ],
        total=130,
    )
