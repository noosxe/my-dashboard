# Systematic approaches for making AI agents produce high-quality UI — synthesis for a two-person Rust + gpui project

**Evidence labels used throughout:** **[measured]** primary research; **[spec]** normative or draft-spec text; **[canon]** widely-cited practitioner material; **[vendor]** first-party claim about a product; **[anecdotal]** testimonials; **[repo]** verified repository fact; **[inference]** reasoning or composition by this synthesis, not a claim any fetched source makes.

## 0. Source status — what could actually be verified

| Source | Status | Evidence class |
|---|---|---|
| NN/g, 10 Usability Heuristics | Fetched in full | [canon] — heuristics refined in 1994 via factor analysis of 249 usability problems; stable since |
| Refactoring UI (sales/TOC page) | Fetched in full | [canon] for content; efficacy supported only by testimonials — [anecdotal]; web-styled examples |
| DTCG Design Tokens Format | Fetched — but it is a **preview draft** explicitly marked "Do not attempt to implement this version" | [spec] in status only; treat version details as unstable |
| Playwright — Visual comparisons | Fetched in full | Tool documentation; first-party, durable |
| Chromatic — Visual tests | **HTTP 404 at the declared URL (retried)** | Unverified; content excluded below |
| WebAIM — Contrast | **HTTP 404 at the declared URL (retried)** | Unverified; **no contrast ratio numbers in this document are source-verified** |
| axe-core (GitHub README) | Fetched in full | [vendor] where it cites its own detection rate; web/HTML-scoped |
| Material 3 — Design tokens | Fetch returned only the page title; JS shell, no extractable content | Unverified; excluded |
| Design2Code (arXiv 2403.03163, NAACL 2025) | Fetched (abstract) | **[measured]** — the only measured study in the set |

## 1. Verified starting position [repo]

Every recommendation below builds on rails already in the tree:

- **Constraint gate:** `clippy.toml` `disallowed-methods` bans `gpui::rgb/rgba/hsla`, `Styled::text_size`, `line_height` — colors and type must come from tokens (docs/0002 D5/D9/D10), enforced by a zero-warnings clippy gate in CI.
- **Token vocabularies:** `text_xs`–`text_xl` (12–20px) type scale; Tailwind-style spacing scale (`0p5` = 2px … `8` = 32px); geometry deliberately exempt from the lint, expected to use named constants.
- **Machine-readable theme:** `crates/core/assets/themes/aurora.json` with semantic slots (`accent.background/foreground`, `list.hover.background`, `muted.*`, `chart_1..5`, …), embedded via `include_str!`.
- **Debug bridge (`crates/debug`):** `tree [--json]` (roles/labels/bounds/focus; dev builds add source locations), `tree --snapshot/--diff`, `tree --tab-stops` (numbered walk, 100-stop cap), `click/focus/key/type/mouse`, `wait-frame`, `session start --name --size WxH` (pinned headless sway, owned a11y bus), `screenshot` (grim, Linux-only, verified 1280×800 PNGs), `run` receipt scripts (`crates/debug/verify/*.txt` with `snapshot`/`diff`/`assert-label`/`click`/`key`/`wait-frame` steps), `doctor`.
- **Determinism assets:** fixed window sizes, frame-accurate `wait-frame`, pinned fonts in CI for font parity, sway+grim pinned via nix.
- **PR discipline:** AGENTS.md requires UI changes be exercised through the bridge first and PR bodies to carry a human-check list; a missing tree node is treated as an element bug.
- **Known gap:** macOS runs receipts degraded — tree-backed and screenshot steps fail (#401); in-app frame capture is the planned follow-up (docs/0010).

---

## 2. The distinct systematic approaches

### 2.1 Design-token constraint systems

- **What it is.** Encode color, dimension, typography, shadow, etc. as named, typed, referenced values so emitted UI can only draw from an approved vocabulary. The DTCG draft defines tokens as objects with a required `$value` plus optional `$type` (color, dimension, fontFamily, fontWeight, duration, cubicBezier, number), `$description`, `$deprecated`; hierarchical groups; aliases (`{group.token}`); composite types (typography, border, shadow, transition); and "translation tools" (it names Style Dictionary and Terrazzo) that convert one token source into platform-specific code **[spec]**. Refactoring UI is the practitioner mirror — "Limit your choices," "Establish a spacing and sizing system," "Establish a type scale," "Define your shades up front" **[canon]**.
- **Evidence it improves AI-produced UI.** None of the fetched sources measures this for agents — that link is **[inference]**. What exists: the spec's own "common vocabulary" rationale, and the repo's gate proving the mechanism is *mechanically enforceable* — a disallowed-methods warning means a styling rule was broken, not a taste judgment **[repo]**.
- **Cost/machinery.** Minimal, because you already run it. **Recommendation: do not adopt the DTCG format.** It is a self-declared preview draft marked "do not attempt to implement this version" **[spec]**, and a second token pipeline would create dual-source-of-truth risk against the existing machine-readable `aurora.json`, which plus the clippy gate already delivers the constraint mechanism **[repo + inference]**. Borrow DTCG *concepts* — groups, types, aliases — as naming discipline inside the existing theme file if useful.

### 2.2 Rubric / heuristic-based review (two distinct inputs)

- **What it is.** Convert diffuse taste verdicts into a fixed, checkable protocol. Two complementary inputs:
  - **NN/g's ten usability heuristics** — explicitly "broad rules of thumb, not specific usability guidelines," refined in 1994 by factor analysis of 249 usability problems, unchanged since **[canon]**. Most relevant here: #1 Visibility of System Status, #4 Consistency and Standards (platform conventions — a direct counter to web-centric aesthetics), #6 Recognition Rather Than Recall, #8 Aesthetic and Minimalist Design ("interfaces should not contain information that is irrelevant"), #9 error messages in plain language that constructively suggest solutions.
  - **Refactoring UI's visual tactics** — symptom→fix checklists: "Hierarchy is Everything," "De-emphasize to emphasize," "Start with too much white space," "Avoid ambiguous spacing," "Use fewer borders" (prefer shadow/contrast/space), "Don't use grey text on colored backgrounds," "Don't rely on color alone," "Don't overlook empty states" **[canon]**. Its premise — "tactics, not talent" — is exactly the shape an agent-consumable rubric needs, but its efficacy evidence is testimonials only **[anecdotal]**; the book is $99–149 (free sample chapters), inside budget.
- **Evidence.** Durable as a *human* expert-inspection method (30+ years); no fetched source measures an LLM applying these heuristics — that adaptation is **[inference]**. The key property for this project is different: the rubric converts the owner's "not good at all" into named per-item verdicts, which is the only form an agent can act on and a non-designer owner can render **[inference]**.
- **Cost/machinery.** Lowest of all approaches: one committed rubric document plus a review step. Several items already map to bridge-verifiable observables — labels present in the tree dump (recognition-not-recall), tab order via `--tab-stops`, status feedback visible in the tree **[inference on the mapping, repo on the observables]**.

### 2.3 Automated objective checkers (contrast, structure, a11y rules)

- **What it is.** Deterministic pass/fail rules over machine-readable UI state, run like tests/lints. axe-core is the precedent: a rule engine for WCAG 2.0/2.1/2.2 (A/AA/AAA) plus best practices, "built to seamlessly integrate with any existing test environment… alongside your regular functional testing," designed to return zero false positives, and marking uncertain results "incomplete" so manual review targets the remainder **[vendor, concretely falsifiable in its rule list]**. Its "on average 57% of WCAG issues automatically" figure is first-party **[vendor]** — but the bounded-coverage framing is the right mental model for every objective checker. Contrast is a member of this family: computable from color values; axe-core ships a `color-contrast` rule **[fetched]**. **Specific ratio thresholds are unverified in this source set** (WebAIM 404'd) — see the shortlist for handling.
- **Evidence.** No fetched source measures this for agents; the case is that it converts a quality dimension into a gate the way unit tests gate logic — **[inference]** consistent with axe's own philosophy.
- **Cost/machinery.** axe-core itself is HTML/DOM-scoped and not runnable against gpui; the *pattern* transfers: rules over a structured UI description **[inference]**. Your `tree --json` (roles, labels, bounds, focus) and theme JSON are exactly such inputs **[repo]**.

### 2.4 Golden-image / visual regression testing

- **What it is.** Commit reference screenshots; every run re-renders and pixel-diffs; baseline changes are reviewed like code. Playwright documents the complete recipe **[tool docs]**: first run *writes* the baseline ("A snapshot doesn't exist… writing actual"); comparison uses the pixelmatch algorithm with a configurable `maxDiffPixels` tolerance; baselines are named per browser+platform (`…-chromium-darwin.png`) because rendering varies; snapshot directories are committed to VCS and **"review any changes to it"**; `--update-snapshots` is the deliberate refresh path; a custom stylesheet (`stylePath`) hides volatile elements for determinism; and the central physics warning — rendering varies by host OS, version, settings, hardware, power source, headless mode — so "run tests in the same environment where the baseline screenshots were generated." Notable trick: Playwright "took a bunch of screenshots until two consecutive screenshots matched" before saving a baseline.
- **Evidence.** Established *testing* mechanics, not an intervention study; the function here is regression *prevention* — keeping a once-approved look from silently degrading — not initial quality **[inference]**. (Chromatic, the PR-gated at-scale reference, 404'd and nothing from it is used here.)
- **Cost/machinery.** Small for this repo: capture exists, and the `image` 0.25 crate already in the tree (PNG feature) is enough to implement a pixelmatch-style differ **[repo + inference]**. Your pinned headless-sway session with fixed `--size` is exactly the "same environment" Playwright demands; `wait-frame` is a strong determinism primitive — arguably stronger than two-consecutive-shots, since the frame counter advances only on change **[repo; the comparison is inference]**.

### 2.5 Screenshot-in-the-loop visual critique

- **What it is.** Render the UI, feed the screenshot to a multimodal model, critique against a rubric or reference, fix, repeat.
- **Evidence.** The only measured anchor: **Design2Code** curated 484 real-world webpages and built automatic evaluation metrics assessing "how well current multimodal LLMs can generate code implementations that directly render into the given reference webpages, given the screenshots as input," with comprehensive human evaluations validating the metric rankings, across GPT-4V/GPT-4o/Gemini/Claude **[measured]**. Two findings matter: (a) MLLM judgment of rendered-vs-target is real and rankable — a machine gate can substitute for most human passes; (b) models "mostly lag in recalling visual elements from the input webpages and generating correct layout designs" — **pixel-only critique under-detects structural/layout errors**, so every critique input should include the tree snapshot/diff, not just pixels **[measured finding; the pairing is inference]**. Caveats: it is a web front-end benchmark of *one-shot* generation — the iterate-until-pass loop itself is **[inference]**.
- **Cost/machinery.** Moderate: capture plumbing (Linux-only today) + a multimodal model call per iteration + the rubric. Include an explicit iteration cap to bound cost **[inference]**.

### 2.6 Reference-anchored generation

- **What it is.** Anchor generation to concrete exemplars rather than a verbal goal. Two forms: *screenshot-as-spec* (Design2Code's entire task) and *exemplar libraries* (Refactoring UI's 200+ component gallery, pre-built 10-shade palettes — "medium-fidelity… just enough fidelity to be useful," deliberately without CSS) **[measured / canon-commercial]**.
- **Evidence.** Measured part: frontier MLLMs can approximately reproduce a reference screenshot in code, with the known element-recall/layout weakness **[measured]**. For a non-designer owner, "pick the reference you like" transfers taste without vocabulary — **[inference]**, and it directly addresses "owner cannot articulate visual fixes."
- **Cost/machinery.** Low: a handful of committed, owner-approved reference screenshots. Caveat: Refactoring UI's examples are web-styled and the gallery risks homogenizing; curate **native-desktop** exemplars **[inference]**.

### 2.7 Multi-agent generator/critic loops (gated review)

- **What it is.** One agent builds UI; a critic (with different inputs — tree dump, screenshot, rubric) reviews and rejects until satisfied; the owner is the final approver in the PR.
- **Evidence.** **No fetched source describes or measures multi-agent UI loops.** The nearest verified components are Design2Code's metrics+human pairing and axe's checks-inside-tests. Treat the pattern as **[inference]** assembled from measured parts — worth piloting as a *gated PR pipeline* (generate → objective checks → golden diff → vision critique vs. rubric → owner approves), not worth citing as established.
- **Cost/machinery.** Process cost plus one CI job; every input the pipeline needs (sessions, receipts, PR-body artifacts) already exists **[repo]**.

---

## 3. Combinations actually reported to work together

Only these four pairings are source-reported; everything beyond them is synthesis:

1. **Tokens → translation → single source of truth** — DTCG's stated pipeline (one token file feeding codegen and docs) **[spec; concept-level, draft-caveated]**.
2. **Committed baselines + deterministic environment + tolerance + volatility masking + explicit update path + review of baseline changes** — Playwright's pattern is only sound as a package **[tool docs]**.
3. **Automated checks inside existing functional tests + explicit manual-review remainder** — axe's model: automate the bounded subset, route "incomplete" to humans **[vendor]**.
4. **Automatic metrics validated by human evaluation** — Design2Code's methodology: machine judgment calibrated against human judgment **[measured]**.

**Synthesized stack [inference]:** tokens bound what code can *express* at write time → objective tree/pixel checks bound the *acceptance* space in CI → rubric + reference critique addresses first-pass quality, the layer automation can't see → goldens hold the line afterward. The layers are complementary because they catch disjoint failure classes: goldens catch regressions but say nothing the first time a surface exists; vision critique addresses first-pass quality but is measurably weakest at layout — precisely what the tree audit measures mechanically. Design2Code's layout-recall finding is the standing argument for never trusting the vision layer alone.

---

## 4. Shortlist — adoption plan for this repo

**Phase 1 — immediate, macOS-safe, deterministic, ~$0:**

1. **Committed rubric (`docs/design-review-rubric.md`, a day).** NN/g's ten restated as per-surface checkable items plus a Refactoring UI tactics checklist (hierarchy explicit, white space generous, spacing on scale, borders minimized, color not sole signal). Every UI PR's body carries it as a pass/fail checklist; the agent attaches the session screenshot and tree excerpt per item; the owner marks boxes instead of writing prose. This upgrades AGENTS.md's existing "exercise the bridge and say so" from ritual to review artifact — the direct cure for "owner cannot articulate visual fixes" **[canon + inference on mechanics]**.
2. **Contrast gate over `aurora.json` (half a day).** The theme is already data with semantic fg/bg slot pairs; a unit test computes the ratio for each declared pair and fails below threshold. **Make thresholds configurable and leave them unverified** — the WebAIM source was unreachable; re-source exact ratios from primary WCAG/W3C text before fixing defaults. The *mechanism* (contrast as a computable gate over token values) is established by axe's `color-contrast` rule **[fetched; numbers unverified]**.
3. **`audit` verb over `tree --json` (a few days).** Pure Rust over existing output; starter rules: every focusable node has role+label (AGENTS.md already mandates this — make it mechanical), no zero/negative-sized bounds, sibling gaps ∈ the spacing scale, no overlapping bounds, tab-stop traps via the `--tab-stops` walk, duplicate labels. Adopt axe's design virtues: deterministic, zero-false-positive bias, and an "incomplete → owner checklist" escape rather than hard fails **[vendor pattern; rule set is inference]**. Wire into receipt scripts so one command produces the audit receipt.
4. **`tree --snapshot/--diff` as the cross-platform structural golden** — works on macOS today, gates structural regressions until pixel capture does.

**Phase 2 — in parallel, Linux CI:**

5. **Golden screenshots, Playwright semantics (2–3 days for the core; 1–2 weeks for the full matrix).** Core: committed `screenshots/` baselines keyed by size/theme, pixel-diff with a small tolerance (the in-tree `image` 0.25 crate covers the differ), explicit `--update-baselines` verb, PR receipts attach the diff. Follow-on: multi-theme/multi-size matrices, double-capture-until-stable, volatility masking. Follow the same-environment rule strictly: baselines live and die on the Linux runner; never compare macOS captures against them.
6. **Lightweight screenshot-critique ritual per UI PR (hours to start).** `session start --size 1280x800` → drive → `screenshot` **plus tree snapshot/diff** → multimodal critique against the rubric → fix named findings → re-shoot; **cap iterations**; the final before/after pair goes to the owner. Screenshots are proven model input **[measured]**; the loop wrapper is **[inference]**. Never pixels-only for layout.

**Phase 3 — reference anchoring (owner: ~an hour):**

7. **3–6 owner-approved native-desktop exemplars in-repo.** The agent composes gpui-component widgets + Aurora tokens toward the reference instead of toward "good." Expect layout drift — the measured weak spot — to be caught by items 3/5, not by the model. Optional: Refactoring UI (~$99) for galleries/palettes, with the web-aesthetic caveat **[canon/anecdotal]**.

**Roadmap priority — close the macOS eyes:**

8. **In-app frame capture (docs/0010 follow-up, #401).** Until it lands, every visual gate optimizes a rendering the owner never sees. Until then, the deterministic gates above (items 1–4) are chosen precisely because they run everywhere; treat this follow-up as a first-class priority so the critique loop eventually runs on the owner's actual platform **[repo + inference]**.

**Deliberately skip:** full DTCG implementation (self-declared draft saying "do not implement this version"; your theme JSON + clippy gate already fill the role) **[spec + repo]**; Chromatic-style SaaS (source unverified, budget-constrained, and its PR-gating workflow is reproducible locally via item 5); multi-agent loops as a standalone bet (no source evidence — item 2.7's gated pipeline is the defensible version); expecting rubric+vision alone to fix layout precision (measured weakness — keep the mechanical layers).

**Sequencing rationale [inference]:** Phase 1's deterministic gates make every later vision pass cheaper (fewer false starts); Phase 2 holds the line while Phase 3/6 drive first-pass quality upward; the macOS work closes the loop where the owner actually lives.

---

## 5. Honest limitations

- Chromatic, WebAIM, and Material 3 were unreachable or empty; their content is excluded, and **contrast thresholds remain unverified** within this source set.
- **No fetched source measures any of these approaches for AI-agent-authored UI or for native/gpui.** Design2Code — web front-ends, one-shot generation — is the only measured study, and its result cuts both ways: vision evaluation works, layout recall is the weak spot.
- The DTCG document is an unstable draft that warns against implementation; only its concepts are borrowed here.
- The multi-layer stack, the owner-veto checklist mechanics, and all effort-to-quality claims for this repo are inferences from the sources plus verified tooling — not findings.

That last point is also the opportunity: nothing in this evidence base measures agent-authored native-desktop UI quality, so the receipts your gates produce — rubric outcomes, audit results, golden diffs, critique iterations — become that evidence for your own project.