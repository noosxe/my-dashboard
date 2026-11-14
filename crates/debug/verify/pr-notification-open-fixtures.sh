#!/bin/sh
# Builds the fixture overlay pr-notification-open.txt runs over (#874).
#
# The committed GitHub fixtures carry no notifications, and adding one to
# them would move every goldens' notification counts. This copies the tree
# to $1 and adds one unread notification whose subject is beacon's PR 101
# (the same PR the Open PRs lane lists), as the repo list and the thread
# GET both return it.
#
#   crates/debug/verify/pr-notification-open-fixtures.sh /tmp/fixtures-notif
#   export DASHBOARD_GITHUB_FIXTURE=/tmp/fixtures-notif
set -eu
src="$(cd "$(dirname "$0")" && pwd)/fixtures/github"
dst="${1:?usage: $0 <destination-dir>}"
rm -rf "$dst"
cp -r "$src" "$dst"
thread='{"id":"9001","unread":true,"reason":"review_requested","updated_at":"2026-10-23T11:30:00Z","subject":{"title":"Parser: accept trailing commas in manifest lists","url":"https://api.github.com/repos/fixture-user/beacon/pulls/101","type":"PullRequest"},"repository":{"full_name":"fixture-user/beacon"}}'
# A non-empty top-level array is a response sequence (fixture.rs), so the
# one page is wrapped once more.
printf '[[%s]]\n' "$thread" > "$dst/rest/notifications.json"
printf '[[%s]]\n' "$thread" > "$dst/rest/repos_fixture-user_beacon_notifications.json"
printf '%s\n' "$thread" > "$dst/rest/notifications_threads_9001.json"
