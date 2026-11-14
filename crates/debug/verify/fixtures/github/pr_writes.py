"""Answers for the writes the PR page sends (docs/0081 §12.1, #812).

Called from generate.py. The fixture transport answers a mutation from
graphql/<Mutation>.json and a REST write from rest/<path>.<verb>.json
(an envelope {"status", "body"}); a write with no file is refused. The
answers are static: the refetch after a write still serves the pre-write
read files, so a receipt asserts the recorded write
(`dashboard-debug assert-write`), not a changed page.

Payload shapes are the selections the app's documents ask for, checked
against the live GraphQL schema (MergePullRequestPayload.pullRequest,
ClosePullRequestPayload.pullRequest, UpdatePullRequestPayload.pullRequest,
RequestReviewsPayload.pullRequest, MarkPullRequestReadyForReviewPayload,
ConvertPullRequestToDraftPayload and ReopenPullRequestPayload .pullRequest, DeleteRefPayload.clientMutationId,
EnablePullRequestAutoMergePayload and DisablePullRequestAutoMergePayload .pullRequest,
EnqueuePullRequestPayload and DequeuePullRequestPayload .mergeQueueEntry,
AddCommentPayload.clientMutationId,
AddPullRequestReviewPayload.pullRequestReview; PullRequestState and
PullRequestReviewState values) and the REST reference (approve a fork
run: 201; review pending deployments: 200 with an array of deployments).
"""

from pr_scenarios import (
    FULL,
    GATED_ENV_RUN,
    GATED_FORK_RUN,
    GATED_ONLY_RUNS,
    OWNER,
    REPO,
)


def write_pr_writes(write):
    def mutation(name, field, payload):
        write(f"graphql/{name}.json", {"data": {field: payload}})

    mutation("PrMerge", "mergePullRequest", {"pullRequest": {"state": "MERGED"}})
    mutation("PrClose", "closePullRequest", {"pullRequest": {"state": "CLOSED"}})
    # The metadata pickers' writes (docs/0043 D1, #857). `PrUpdateMetadata`
    # answers labels, assignees and milestone alike: one document, three
    # op identities. `PrRequestReviews` is the reviewers' replace-all.
    mutation("PrUpdateMetadata", "updatePullRequest", {"pullRequest": {"id": "PR_101"}})
    mutation("PrRequestReviews", "requestReviews", {"pullRequest": {"id": "PR_101"}})
    # The status block's state flips (docs/0083 D4, D5).
    mutation("PrReady", "markPullRequestReadyForReview", {"pullRequest": {"id": "PR_103", "isDraft": False}})
    mutation("PrConvertToDraft", "convertPullRequestToDraft", {"pullRequest": {"id": "PR_113", "isDraft": True}})
    mutation("PrReopen", "reopenPullRequest", {"pullRequest": {"id": "PR_114", "state": "OPEN"}})
    # `DeleteRefPayload` carries only `clientMutationId` (#773).
    mutation("PrDeleteBranch", "deleteRef", {"clientMutationId": None})
    # Auto-merge and the merge queue (#774, #775): `EnablePullRequestAutoMerge`
    # and `DisablePullRequestAutoMerge` answer `pullRequest`; the two queue
    # mutations answer `mergeQueueEntry` (null once dequeued).
    mutation("PrEnableAutoMerge", "enablePullRequestAutoMerge", {"pullRequest": {"id": "PR_117"}})
    mutation("PrDisableAutoMerge", "disablePullRequestAutoMerge", {"pullRequest": {"id": "PR_118"}})
    mutation("PrEnqueue", "enqueuePullRequest", {"mergeQueueEntry": {"id": "MQE_119"}})
    mutation("PrDequeue", "dequeuePullRequest", {"mergeQueueEntry": None})
    mutation("PrAddComment", "addComment", {"clientMutationId": None})
    mutation(
        "PrUpdateBranch",
        "updatePullRequestBranch",
        {"pullRequest": {"id": "PR_113", "mergeStateStatus": "CLEAN"}},
    )
    mutation(
        "PrAddReview",
        "addPullRequestReview",
        {"pullRequestReview": {"id": "PRR_fixture", "state": "APPROVED"}},
    )

    runs = f"rest/repos_{OWNER}_{REPO}_actions_runs"
    # The fork gate: approve is 201, no meaningful body.
    for run_id in (GATED_FORK_RUN, *GATED_ONLY_RUNS):
        write(
            f"{runs}_{run_id}_approve.post.json",
            {"status": 201, "body": {}},
        )
    # The environment gate: approve or reject answers the deployments.
    write(
        f"{runs}_{GATED_ENV_RUN}_pending_deployments.post.json",
        {
            "status": 200,
            "body": [
                {
                    "url": f"https://api.github.com/repos/{FULL}/deployments/41",
                    "id": 41,
                    "node_id": "D_fixture41",
                    "sha": "0000065bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "ref": "feature/trailing-commas",
                    "task": "deploy",
                    "payload": {},
                    "environment": "staging",
                    "description": None,
                    "creator": None,
                    "created_at": "2026-10-23T10:00:00Z",
                    "updated_at": "2026-10-23T10:00:00Z",
                    "statuses_url": f"https://api.github.com/repos/{FULL}/deployments/41/statuses",
                    "repository_url": f"https://api.github.com/repos/{FULL}",
                }
            ],
        },
    )
