# 0034 · Cache TTL and staleness marking

- **Status:** design — PR open against `main` (implements #193 after acceptance)
- **Builds on:** docs/0007 (cache paint, stale-but-present), docs/0031 (records), docs/0033 (session-age labels)
- **Related:** #193 (this), #192/#0033 (shipped: session-age labels), docs/0029 §4

## 1. Problem

The on-disk `last-known` blobs persist indefinitely and have no age semantics. On cold
start `GithubState::new` seeds the snapshot (`user`/`orgs`/`repos`) and the
notifications list from disk, the dashboard paints immediately ("cache paint, then the
snapshot lands" — docs/0007 §8), and the first tick fires the snapshot refresh. With
network up, the stale window is one beat; offline (or rate-limited / revoked-token), the
dashboard renders confidently wrong counts — PRs merged hours ago shown as open — with
no cue and no expiry. That is the last unmarked staleness in the module: session-age is
tracked per record since docs/0031 and labeled since docs/0033, but the *disk-seeded*
state carries no age at all.

Two surfaces are fed from disk at seed time: the home **GithubCard** (notifications/PR/
issue counts) and the module **main page** (repo list, notification list). Detail pages
never read the cache (page-owned fetches only), so they are out of scope.

## 2. Decisions

**D1 — Age source: the blob's mtime; no schema change.** `GithubCache::age(rel) ->
Option<Duration>` reads `last-known/<rel>.json`'s `metadata().modified()`. The cache
writes are atomic-rename (`store::save_json_atomic`), so mtime is exactly the last
`put` — no envelope migration (which would orphan every existing blob on upgrade), no
sidecar to drift out of sync, no new failure modes beyond a stat call. Staleness
granularity is minutes; mtime is more than enough. Missing blob → `None` (first run:
nothing seeded, nothing to mark). Tests backdate mtimes with std `File::set_times` —
no new dependency.

**D2 — The age lives on the state, seeded and cleared.** `GithubState::new` reads
`age(SNAPSHOT_CACHE_KEY)` and `age("notifications")` into `snapshot_age` /
`notifications_age` (same connected-gate as the existing seed loads). A landed apply
clears its age: the snapshot apply clears `snapshot_age`, the notifications apply clears
`notifications_age`. A landed sync means GitHub answered and the merge ran — from there
the session records own freshness (docs/0031/0032), and docs/0007 already governs the
residuals of partial-error envelopes. Clearing only on "fully clean" applies would need
per-node age tracking to be honest — overkill for the chip's job.

**D3 — One chip, the older age, on both cold-start surfaces.** When connected and
`max(snapshot_age, notifications_age) >= STALE_AGE` (30 min — the issue's example),
render a muted warning chip: "last synced 2h ago — reconnect to update" (exact copy at
implementation; `muted_foreground`/warning theme tokens, no literal colors). It renders
on the home GithubCard, under the counts, and in the module main page's header. One
signal, not two: both caches feed the same two surfaces, and the older age is the
honest worst case. The chip clears the moment either apply lands (D2) — the card
already observes the state.

**D4 — The 24-hour "syncing… overlay" is rejected.** The issue says "consider"; we
considered and declined. docs/0007's stale-but-present is the module's backbone, and an
offline cold start is a supported mode — a user on a plane wants to *read* their cached
dashboard, and an overlay that demands a sync we cannot perform hides the only thing of
value on screen. The chip already states the magnitude at every age ("last synced 5d
ago"); hiding data adds no information. If the owner wants a stronger treatment at
extreme ages, that is a follow-up on top of this substrate, not a blocker for it.

**D5 — No deletion TTL.** The issue's "no TTL" framing is answered presentation-only:
blobs keep persisting (docs/0007 D7; sign-out clears — Q5). Deleting cached data on a
timer would manufacture empty dashboards for exactly the users (offline, lapsed) who
need the cache most. "TTL" here means *mark*, not *evict*.

**D6 — Thresholds are constants, not settings.** `STALE_AGE = 30 min` ships as a
module constant next to docs/0032's `AUTO_REFRESH_INTERVAL`/`FOCUS_MIN_AGE`. The issue's
"configurable" has no requester and no settings precedent for data-policy knobs in this
module; a follow-up can wire it into the settings page (docs/0006) if wanted.

## 3. Test plan

- **`age()`** (tmp cache): a fresh blob → small age; a backdated mtime → the backdated
  age; missing file → `None`.
- **Seeding**: connected state with an old snapshot/notifications blob → the ages land
  on the state; disconnected → `None` (the seed gate).
- **Clear-on-apply**: apply a snapshot SpecResult → `snapshot_age` gone; notifications
  likewise. (Reuses the existing apply test fixtures.)
- **Threshold marker**: boundary unit test at `STALE_AGE` (29:59 vs 30:00).
- **Card probe**: a stale-seeded connected state renders the chip on the GithubCard; a
  landed apply removes it. Main-page chip: same fixture pattern as the header tests.

## 4. Shipped

*(filled by the implementation PR)*
