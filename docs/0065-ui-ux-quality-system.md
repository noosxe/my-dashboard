0065 — UI/UX quality system for agent-authored surfaces
========================================================

Status: **Phases 1–3 implemented** (Phase 1: audit verb + contrast
gate + structural goldens + process wiring, #440; Phase 2: pixel
goldens via `golden`/`assert-golden` + the paired `critique` capture,
#441; Phase 3: owner-picked anchor library — Linear, Things 3, and
provisional Zed under `docs/references/` — plus the AGENTS.md
anchor-naming slot, #442). Remaining execution: the #445 Linux-box
capture run, then the #437/#439 redos under the full system. Born from an owner-directed research
round (prompt: "figure out a systematic approach for giving AI agents
proper tools for high-quality UI/UX design") after the favicon module's
PR verdict — functionally right, visually not good at all.

Scope note (owner): the quality problem is **not favicon-specific** — the
usage/analytics pages already need review and improvement too. This doc
defines the system for **all** UI surfaces; the initial review backlog is
favicon (#437) + usage/analytics (issue filed with this PR).


## 1. Context & problem

The agent writes every line of UI in this repo. The owner human-tests
each PR but is not a designer and cannot articulate visual fixes — the
verdict arrives as "not good at all", which is unfollowable as an
instruction. Three structural gaps cause this:

1. **Leaf styling is already railed and was never the failure.** Colors,
   type, and spacing rhythm are token-locked (docs/0002 D5/D9/D10,
   enforced by `clippy.toml` `disallowed-methods`). D10 *deliberately*
   exempts widget geometry — and that exemption zone (column widths,
   layout composition, grouping, hierarchy, density) has no guidance and
   no gate. That is where the favicon page actually failed.
2. **Nobody looks at pixels.** The debug bridge produces tree dumps and
   (Linux-only) screenshots; PR review runs on code and receipts, never
   on rendered output. macOS cannot even screenshot (#401; in-app frame
   capture is a pending follow-up).
3. **The owner has no operational review instrument.** Human checks are
   free-text asks; nothing converts taste into named verdicts a
   non-designer can mark and an agent can act on.


## 2. Research base (evidence-graded)

Sources actually fetched and verified during the research round; three
declared sources (WebAIM contrast, Chromatic docs, Material 3 tokens)
were unreachable and are **excluded** — nothing below relies on them.

| Source | Class | What it contributes |
|---|---|---|
| NN/g, *Ten Usability Heuristics* (1994) | canon | The heuristic rubric backbone (Appendix A, part 1) |
| Refactoring UI (Wathan & Schoger) | canon (efficacy anecdotal) | Symptom→fix visual tactics (Appendix A, part 2) |
| Playwright visual-comparison docs | tool docs | Golden-image semantics: committed baselines, tolerance, explicit update path, same-environment rule |
| axe-core | vendor pattern | Deterministic rule engines over structured UI state; zero-false-positive bias; "incomplete → human" routing |
| Design2Code (arXiv 2403.03163, NAACL 2025) | **measured** | MLLM judgment of rendered-vs-target is real and rankable; models are measurably weak at layout recall → **pixels must never be the only critique input** |
| DTCG Design Tokens Format | spec (preview draft) | Concepts only; the draft itself says "do not attempt to implement this version" — deliberately skipped (D11) |

Standing caveat, honestly held: **no fetched source measures any of these
approaches for agent-authored native-desktop UI.** The measured parts are
web-scoped; the composition into our stack is inference. The receipts
this system produces are how this project generates its own evidence.


## 3. The system — six complementary layers

Each layer catches a failure class the others cannot see:

    tokens        what code may express          (have: 0002 + clippy gate)
    objective     mechanical pass/fail rules     (new: audit verb + contrast test)
    structural    tree-shaped goldens            (new: committed snapshots + diff)
    pixel         image-shaped goldens           (new: Linux baselines + differ)
    critique      rubric + eyes, agent-side      (new: vision loop per UI PR)
    anchors       taste by selection             (new: owner-picked exemplars)

Goldens hold a once-approved look; critique lifts first-pass quality;
mechanical layers catch layout facts that vision measurably misses. The
owner stays the approver throughout — the system's job is to hand them
named, checkable findings instead of "not good at all".


## 4. Decisions

### D1 — Scope and backlog

All UI surfaces (shell, home cards, github, hub, notifications, settings,
usage/analytics, favicon, future modules). The system ships as: the
rubric (Appendix A), bridge/tool additions (D3–D6), the per-PR ritual
(D7, D10), and the anchor library (D8). The **initial review backlog**
executes under the full system once implemented: #437 (favicon visual
redo) and the usage/analytics review issue filed with this PR.

### D2 — The rubric (Appendix A in this doc)

NN/g's ten heuristics restated as per-surface checkables + the
Refactoring UI tactics checklist + cross-references to the mechanical
gates (so items that should never be eyeballed aren't). Every UI-facing
PR body carries the checklist; the **owner marks boxes** — pass, fail,
or n/a — and writes no prose. An unfollowable verdict becomes twelve to
twenty followable ones.

### D3 — `audit` verb (deterministic tree checks)

`dashboard-debug audit [--session <n>] [--window N]` over the existing
`tree --json` output. Starter rule set:

- every focusable/interactive node has role + label (docs/0010 D10,
  made mechanical — the current "missing node = element bug" rule stops
  depending on human noticing);
- no zero-size / negative / degenerate bounds;
- gaps between aligned siblings belong to the D10 spacing scale
  (2/4/6/8/10/12/16/24/32px) — off-scale gaps are findings, not errors;
- sibling bounds do not overlap (excluding intentional layering flagged
  by id prefix);
- duplicate visible labels within one window;
- tab-stop trap heuristics via the `--tab-stops` walk.

Design virtues lifted from axe: deterministic, biased toward zero false
positives, and anything the rules cannot decide is reported
`incomplete` and routed to the owner checklist instead of hard-failing.
`audit` is receipt-scriptable (`crates/debug/verify/*.txt` gains the
step) so one command produces the audit receipt.

### D4 — Contrast gate over the theme

`aurora.json` is already data with semantic fg/bg slot pairs. A unit
test computes WCAG contrast ratios for every declared pair and fails
below threshold — text pairs at 4.5:1 (SC 1.4.3 AA; verified against
primary W3C text at implementation). Decorative/non-text pairs are
exempt by explicit list in the test, not by silence.


**Amendment (docs/0078):** the gate is scoped to *app-owned* theme
content. The factory pair is retired — the bundled set is the upstream
gallery verbatim — so the app owns no palette bytes to vouch for; the
threshold table above stays normative for any future app-owned surface.
Upstream curation is trusted, not gated (the docs/0074 D4 stance).
**Amendment (Phase 1 implementation):** pre-computing every pair found
(a) two real text failures in Aurora Light — `info.foreground` and
`warning.foreground` were white ink on fills darker than their
dark-theme twins (4.11:1 and 3.39:1); both now use the dark-theme ink
pattern (#0e0f15 → 4.66:1 / 5.65:1); and (b) the entire hairline
border/bar register sitting at 1.0–1.7:1 against its surfaces. (b) is
the quiet-chrome aesthetic working as designed, and SC 1.4.11 exempts
visual information not required to identify components — so the *gate*
covers text pairs only, and the non-text slots live on the documented
exemption list. Promoting any of them to gating is an owner decision
made in a PR, never a test.

### D5 — Structural goldens (same-environment, gated in the container)

Per-surface tree snapshots committed under
`crates/debug/verify/snapshots/<surface>.json`; the committed receipts
`assert-baseline` them after each stop's audit (D3). Structural drift —
a missing section, a renamed control, a changed role or label — fails
the receipt. Baseline updates go through PRs like code; the diff in the
PR body is the review.

First capture round shipped 2026-10-13 (#535): `home`, `home-return`,
`github`, `usage-overview`, `usage-quota`, `hub`, plus `settings` and
`favicon` staged with their expected-red hops (#530). The two usage goldens
retired with the usage page shell (#696, docs/0082 §9); #700 captured
the redesigned set (docs/0082 §10): `usage-budget` and `usage-requests`
pin the empty-ledger shape in audit.txt's walk, and `usage-budget-seeded`,
`usage-requests-seeded` and `usage-requests-log` (the Q8 log filling the
viewport after the page scrolls to it) pin the populated shape in
`usage-seeded.txt`, over the fixture session's 90-day seed (docs/0081
§12.1). Three notes
tighten the original sketch: (1) the diff includes window-relative
bounds and the `focused` flag, so the same-environment rule (D6)
applies to structure too — goldens are captured and asserted in the
docs/0071 container at 1280×800, not across platforms; (2)
capture-local values never reach the file — frame number and timestamp
are zeroed on write, and wall-clock times in labels ("as of 23:15", the
usage status line) are normalized to fixed tokens; (3) the write path
is `baseline <surface> --update` only, and the one-shot capture walks
the exact step order of the receipt that asserts it (focus moves
matter). The seeded-ledger capture that #536 tracked is the fixture
seed plus `usage-seeded.txt`.

A golden compares structure, not ids *(amendment, #769)*: a removed and
an added node with the same role, label, bounds and focus state are one
node that re-keyed, so they aren't drift. Ids inside a view hash its
entity id, which depends on how many entities came before, and the PR
page's differ between identical walks. `tree --diff` still reports every
id, and a stale id in a script still fails loudly with `4005
UNKNOWN_NODE`.

### D6 — Pixel goldens (Linux, Playwright semantics)

Committed baselines keyed by size × theme; re-capture in a pinned
headless session (`--size WxH`, `wait-frame` before capture — stronger
determinism than Playwright's two-consecutive-shots trick since the
frame counter only advances on change); pixel diff with a small
tolerance implemented over the in-tree `image` crate; an explicit
`baseline update` verb as the only deliberate refresh path. The
same-environment rule is absolute: baselines live and die on the Linux
runner; macOS captures are never compared against them.

### D7 — Vision critique loop (per UI-facing PR)

On the Linux box: `session start` → drive to the surface → capture
**screenshot AND tree snapshot** (Design2Code's measured layout-recall
weakness is the standing rule: never pixels-only) → multimodal critique
against Appendix A → fix named findings → re-capture. **Iteration cap:
3** — findings remaining after the cap ship as known-issues listed in
the PR, not as silent debt. The PR body carries the before/after pair
plus the findings list; the owner's final human pass remains the merge
gate.

The judge step is a tool (#539 §2): `dashboard-debug critique --panel
(<surface> | --image <path>) [--name l] [--judges f] [--brief f]
[--out d]` runs the local multi-judge vision panel against one frame —
one llama-server at a time, bounded context (the default provisions the
model's full KV cache and OOMs Metal), verdicts + a `run.json` manifest
into `--out` (default `.pi/panel/<label>`). Surface mode captures the
settled frame first (session-bound, Linux-only); image mode judges any
frame on disk, wherever the judges live. The lineup lives in
`$HOME/.config/dashboard/panel-judges.json` — template:
`crates/debug/verify/panel-judges.example.json` — with per-judge request
`overrides` (e.g. MiniCPM's `{"enable_thinking": false}`: the model
card's fast mode is the documented default; thinking is opt-in).
Sampling is card-faithful where the card transfers to our llama.cpp
runtime, empirical where it does not (recorded per judge in the
example lineup): Qwen3-VL keeps the card's concentration (`top_p 0.8,
top_k 20`) but not its `temperature 0.7` — on llama.cpp that samples a
spurious "I cannot see an image" refusal in roughly a third of runs
regardless of image-token budget, while `0.3` never has; it also
needs `extra_args: ["--image-min-tokens", "1024"]` since llama.cpp
0.6.0, without which desktop frames encode below the model's minimum
image-token budget and refusal is deterministic. MiniCPM-V 4.5 runs
its llama.cpp cookbook recipe verbatim (`temperature 0.7, top_p 0.8,
top_k 100, repeat_penalty 1.05`). UI-TARS runs greedy
(`temperature 0.0`) per its deploy guide — that removes the sampling
die, but not all variance: on Metal, near-tie logits still flip
between runs (verified: two temp-0.0 runs on one frame produced
different finding lists), so clustering stays mandatory for every
judge. (Its models moved `bytedance-research` → `ByteDance-Seed`, and
the repo to `bytedance/UI-TARS`.)
Reconciliation stays agent/human-side, and single verdicts are noise:
cluster across judges and runs before acting on a finding — the panel's
own shakedown showed a judge flipping its "best element" between
identical runs. Blind judges are detected, not trusted (#543): JPEG is
the tested-everywhere frame format (MiniCPM's projector has gone blind
on PNG data-URIs); the verb warns on non-JPEG frames, records
`image_mime`/`blind_risk` in the manifest, and refusal-matches every
verdict — a judge that answers without seeing is marked `ok: false,
blind: true` with the refusal quoted in its `.err` sidecar, so an
absence-of-findings from a model that never saw the frame can never
masquerade as a fixed finding.

### D8 — Reference anchors (taste by selection)

The owner cannot articulate fixes but **can pick what looks right**.
3–6 owner-approved native-desktop exemplars (whole apps, panels, tool
windows — screenshots or links, whatever is easiest) get committed under
`docs/references/` with a one-line note each. Every new UI surface
**names its anchor in the PR body** and composes toward it using
gpui-component's widget vocabulary + Aurora tokens; deviations are
noted, not smuggled. The pinned gpui-component `story` crate gallery
(60+ live widget panels in the house visual register) is the always-on
secondary anchor for widget-level composition.

### D9 — Close the macOS eyes (first-class priority)

The in-app frame capture follow-up (docs/0010/0055, #401-adjacent)
moves from "known follow-up" to **blocking priority for this system**:
until it lands, every visual gate optimizes a rendering the owner never
sees on their platform. The implementation round files/tracks the
capture issue if it doesn't already exist and sequences it early.

### D10 — Process wiring (AGENTS.md, at implementation)

The PR-discipline section gains the UI-facing PR checklist: rubric
boxes from Appendix A, audit receipt reference, (Linux) critique
before/after artifacts, anchor named. The debug-bridge section gains
the `audit` verb. This converts AGENTS.md's existing "exercise the
bridge and say so" from ritual to artifact.

### D11 — Deliberately not adopted

- **DTCG token format** — self-declared preview draft ("do not attempt
  to implement this version"); `aurora.json` + the clippy gate already
  deliver the constraint mechanism without a second token pipeline.
- **Visual-regression SaaS** (Chromatic-style) — unverified source,
  budget-constrained, and D6 reproduces the needed workflow locally.
- **Standalone multi-agent generator/critic loops as a bet** — no
  source evidence; the gated pipeline D2–D7 is the defensible version.
- **Expecting rubric+vision to fix layout precision** — measured
  weakness; layout stays with the mechanical layers (D3/D5/D6).

### D12 — Sequencing

- **Phase 1** (macOS-safe, ~a week): rubric adoption + contrast test +
  `audit` verb + structural goldens. Immediately makes every review —
  including the backlog's — operational for a non-designer owner.
- **Phase 2** (Linux CI): pixel goldens + the critique ritual wired
  into PRs.
- **Phase 3**: anchor library once the owner picks exemplars; backlog
  reviews (#437, usage/analytics) execute under the full system.

Implementation issues are split per phase after this doc merges.


## 5. Verification & testing

- `audit` rules get unit tests over synthetic `tree --json` fixtures
  (each rule: a passing tree, a failing tree, an incomplete tree).
- Contrast test covers every declared aurora pair; fixture pairs pin
  both sides of the threshold. (Retired by docs/0078 with the theme it
  vouched for.)
  both sides of the threshold.
- The pixel-differ gets tolerance tests over crafted fixture PNGs
  (identical / one-pixel change / tolerance boundary).
- Receipt scripts for the new verbs live under `crates/debug/verify/`
  alongside the existing ones.
- **The system measures itself**: rubric outcomes, audit findings and
  critique iteration counts accumulate in PR bodies — over time that is
  this project's own evidence base for what actually moves UI quality
  (§2's standing caveat repaid).


## 6. Worked example — the backlog under this system

**#437 (favicon redo), after Phase 1–3 land:**

1. Owner picks anchors (maybe one exemplar per surface type).
2. Agent runs `audit` + contrast + goldens → mechanical findings fixed
   first, no vision spent on them.
3. Critique loop (screenshot + tree, Appendix A, ≤3 iterations) on the
   Linux box; before/after + findings land in the redo PR.
4. Owner opens the PR: rubric boxes already marked by the agent with
   evidence; owner flips the ones they disagree with — a checkbox, not
   a design essay.
5. Merge only when the box list is clean or remaining n/a's are
   accepted explicitly.

The usage/analytics review follows the identical flow.


## 7. Non-goals

- No design staff, no agency process, no paid tooling required (the
  optional Refactoring UI purchase, ~$99, is an owner convenience
  ruling — the system works without it; its galleries are web-styled
  and only ever secondary to D8 anchors).
- No re-litigation of docs/0002 token rules; this doc governs the
  layers above them.
- No macOS a11y-tree work here — that is #401's track; D9 links to it,
  does not absorb it.
- No automated taste. The system produces **named findings**; the owner
  remains the approver, always.


## 8. Docs amendments (ride the implementation PRs)

- AGENTS.md: UI-facing PR checklist + `audit` verb pointer (D10).
- docs/0055: the `audit` verb + snapshot-baseline policy references.
- docs/0002 D10: the geometry-exemption note gains a pointer —
  composition guidance lives in 0065 (Appendix A) and D8 anchors.
- docs/README.md: this row (lands with this PR).
- Each phase's implementation PR amends this doc only if reality
  contradicts a decision — same amendment-rules as every house doc.


## 9. Review resolutions (owner)

None yet — the doc stands as written pending review.


## Appendix A — the UI review rubric

**Part 1 — per-surface heuristic checkables** (NN/g's ten, restated).
Mark each: pass / fail / n/a. A *fail* names what and where.

1. **Status visible** — every action/deferred job shows its state;
   nothing silently works. *(bridge observable: status nodes in tree)*
2. **World-native** — conventions match the platform and the house
   (github module register, gpui-component idioms); no web-isms
   (underlined links where buttons belong, browser-y chips walls).
3. **User control** — destructive actions confirm (favicon overwrite
   dialog is the house exemplar); escape paths exist.
4. **Consistency** — same widget for same job everywhere; identical
   spacing rhythm across sections; one button variant per role.
5. **Error recovery** — messages name the cause and the next step in
   plain language (toasts included).
6. **Recognition over recall** — options visible or reachable by one
   affordance; state (selected tab/platform/dark mode) readable from
   the screen alone. *(tree observable: labels/state text present)*
7. **Shortcuts for frequent paths** — the main flow is ≤3 interactions;
   anything repeated per-use has a faster path or a default.
8. **Minimalist** — no information the surface can survive without;
   the favicon header-paragraph was the anti-exemplar (removed in the
   0064 §10 revision).
9. **Recovery from mistakes** — undo/confirm where mistakes are likely;
   destructive buttons visually distinct from safe ones.
10. **Help affordance** — anything non-obvious has one line of context
    where it's needed, not a manual elsewhere.

**Part 2 — visual tactics checklist** (Refactoring UI, symptom→fix):

11. **Hierarchy explicit** — one dominant element per view; importance
    readable at a glance (size, weight, color — not borders).
12. **De-emphasize to emphasize** — secondary chrome recedes
    (muted, smaller, less contrast) so the primary content advances.
13. **White space starts generous** — first pass errs spacious; density
    is added deliberately, never rescued from cramped.
14. **Grouping unambiguous** — spacing tells what belongs together;
    related controls read as one unit; ambiguous gaps are bugs.
15. **Fewer borders** — separation via space, background contrast or
    shadow before lines; a border is a last resort.
16. **Contrast-legible text on color** — no grey body text on colored
    backgrounds; theme slot pairs only (see D4's gate).
17. **Color never the only signal** — state pairs color with icon,
    label, or position.
18. **Empty/loading/error states designed** — blank is a design
    failure; each surface ships its three states (the favicon blank
    previews were a violation).
19. **Scale economy** — sizes come from the fixed scales (D9/D10);
    geometry constants are named and few.

*(Items 16 and the scale half of 19 are mechanically gated — D4 and
D3 — and should read "see gate result", not be re-judged by eye.)*

**Part 3 — mechanical gates cross-reference**

20. audit clean (D3) · contrast pairs pass (D4) · structural golden
    unchanged or diff-explained (D5) · pixel golden unchanged or
    baseline-PR'd (D6, Linux) · anchor named (D8).

A surface passes review when Parts 1–2 have no unaccepted *fail* and
Part 3 is all green — or every exception is written down in the PR.
