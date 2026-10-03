I have everything I need. Two declared sources (Chromatic, WebAIM) were persistently unreachable (HTTP 404 on repeated attempts, both markdown and text extraction), and the Material 3 tokens page returned only an empty SPA shell ("Design tokens – Material Design 3") — I flag those below rather than fabricating their content. The other six sources fetched fully, and I verified the repo's actual rails (`clippy.toml`, `AGENTS.md`, `crates/debug/*`, docs index) to ground the adoption shortlist.

---

# Systematic approaches for making AI agents produce high-quality UI — synthesis for a two-person Rust/gpui project

## 0. Source status and evidence map

| Source | Status | Evidence class |
|---|---|---|
| NN/g, 10 Usability Heuristics | Fetched | Durable — heuristics derived 1990–1994, refined via factor analysis of 249 usability problems (per the article's "Note from Jakob") |
| Refactoring UI (sales page) | Fetched | Vendor product page; chapter TOC is concrete, but efficacy evidence is anecdotal (testimonials/tweets only) |
| DTCG Design Tokens Format (tr.designtokens.org → designtokens.org draft) | Fetched (truncated) | Spec precedent — but explicitly a **draft "preview" Community Group report, "not a W3C Standard"**, carrying a "do not attempt to implement this version" warning |
| Playwright visual comparisons | Fetched | Durable tool documentation; the golden-image mechanics are normative within Playwright |
| Chromatic visual tests | **Unreachable (404, 2 attempts)** | No content verified; claims below limited to the source's declared purpose |
| WebAIM contrast | **Unreachable (404, 2 attempts)** | No content verified; contrast thresholds could not be re-checked this run |
| axe-core (GitHub) | Fetched | Vendor-documented; coverage figure ("on average 57% of WCAG issues automatically") is a vendor claim |
| Material 3 design tokens | **Empty SPA shell — no content** | Unverifiable beyond page title |
| Design2Code (arXiv 2403.03163, NAACL 2025) | Fetched (abstract) | **Measured** — 484-webpage benchmark, automatic metrics validated by human evaluation |

Everything not directly traceable to a fetched source is tagged **[inference]**.

---

## 1. Constraint systems: design tokens, scales, component vocabularies

**What it is.** A machine-readable "single source of truth" for colors, dimensions, typography, etc., from which platform code is generated. The DTCG format defines JSON files where a token is an object with a required `$value` plus optional `$type` (color, dimension, font family/weight, duration, cubic-bézier, number), `$description`, `$deprecated`, and vendor `$extensions`; tokens nest in groups, can alias each other (`{color.text.primary}`), and composite tokens bundle related properties (typography, shadow, border, transition, gradient). The spec's stated workflow: design tools export tokens → **translation tools** (it names Style Dictionary and Terrazzo) generate platform-specific source → documentation tools display names instead of raw values. Refactoring UI's TOC is the design-side mirror: "Establish a spacing and sizing system," "Establish a type scale," "Define your shades up front," "Limit your choices," "Ditch hex for HSL" — i.e., pre-computed palettes and scales so no ad-hoc value is ever chosen at build time.

**Evidence it improves AI-produced UI.** None of the fetched sources *measures* this for AI agents — tag: **[inference]**, though a strong one. The mechanism is structural: tokens shrink the action space, and the request's own decomposition ("decent composition when constrained to tokens and component vocabularies") is consistent with Design2Code's finding that models lag specifically at layout/visual-element decisions — precisely the decisions tokens and vocabulary remove. Note also axe-core's architecture is the same idea applied to rules: shared machine-readable constraint vocabularies across tools.

**Cost/machinery.** Low-to-moderate: one JSON file + a small codegen step (the spec exists precisely so you don't invent a bespoke format; but since it's a moving draft, adopting its *stable subset* — groups, `$value`/`$type`, aliases — is the safe play **[inference]**). Enforcement is the cheap half in this repo: the clippy `disallowed-methods` gate already does this for colors (`gpui::rgb/rgba/hsla`) and typography (`text_size`, `line_height`) per `clippy.toml` (docs/0002 D5/D9/D10).

---

## 2. Visual feedback loops: screenshot capture + vision critique, iterate-until-pass

**What it is.** Render the UI, feed the screenshot (plus structure) to a multimodal model, critique against a rubric or reference, fix, repeat until passing.

**Evidence.** The only *measured* evidence in the source set: **Design2Code** built exactly the evaluation half — 484 real-world webpages, automatic metrics assessing "how well current multimodal LLMs can generate code implementations that directly render into the given reference webpages, given the screenshots as input," with human evaluations validating the automatic metric rankings, across GPT-4V/GPT-4o/Gemini/Claude. Two findings matter here: (a) MLLM judgment of *rendered-vs-target* is real and rankable; (b) the fine-grained breakdown shows models "mostly lag in recalling visual elements from the input webpages and generating correct layout designs" — i.e., pixel-only critique under-detects structural/layout errors. Caveats: it is a web front-end benchmark, not native/gpui, and it evaluates one-shot generation, not an iterate-until-pass protocol — the loop framing itself is **[inference]**.

**Cost/machinery.** The expensive dependency is *deterministic rendering*; Playwright's docs are blunt that screenshots vary "based on the host OS, version, settings, hardware, power source… headless mode," and instruct running tests "in the same environment where the baseline screenshots were generated." This repo already owns that piece: headless sway sessions with a fixed `--size WxH` and grim capture (`screenshot [--name]`, docs/0010 D10; docs/0055).

---

## 3. Rubric-based critique: heuristics and visual-design principles as checkable protocols

**What it is.** Convert expert-review checklists into a fixed per-change review protocol the agent (and the owner) walk item by item.

**Evidence.** NN/g: the 10 heuristics are explicitly "broad rules of thumb," stable since 1994, derived from factor analysis of 249 documented usability problems — durable *for human heuristic evaluation*; no fetched source measures them as an LLM-critic prompt (**[inference]**). The most UI-relevant for this project: #4 Consistency and Standards (internal consistency = the token system's whole point; "Jakob's Law" — users spend most time in *other* apps, so platform conventions matter, directly countering the "web-centric aesthetics" failure mode), #8 Aesthetic and Minimalist Design ("every extra unit of information… diminishes [the] relative visibility" of relevant ones), #6 Recognition Rather than Recall, #1 Visibility of System Status, #9 error messages "in plain language (no error codes)… constructively suggest a solution." Refactoring UI supplies the *visual* rubric as chapter titles: hierarchy ("Not all elements are equal," "De-emphasize to emphasize"), "Don't use grey text on colored backgrounds," "Start with too much white space," "Avoid ambiguous spacing," "Use fewer borders" (prefer shadows/contrast/space), "Don't overlook empty states," "Accessible doesn't have to mean ugly." Its efficacy evidence is **anecdotal** (testimonials); the book is $99–$149 one-time with free sample chapters — inside the stated "a little" budget. Material 3 was meant to anchor the token-system-as-rubric idea but **could not be verified**.

**Cost/machinery.** Cheapest of all: a checklist file plus a prompt/CI step. The owner-veto pattern ("check the failing boxes" instead of articulating taste) is **[inference]** but directly addresses "owner cannot articulate visual fixes."

---

## 4. Automated objective checkers: contrast, spacing, accessibility rules, lint-style enforcement

**What it is.** Deterministic pass/fail rules computed from rendered or structural data, run like tests/lints.

**Evidence.** axe-core is the precedent: a rules engine for "WCAG 2.0, 2.1, 2.2 on level A, AA and AAA as well as… best practices," designed to "seamlessly integrate with any existing test environment… alongside your regular functional testing," claiming zero false positives and — the key honest caveat — finding "**on average 57% of WCAG issues automatically**," returning "incomplete" where certainty is impossible so manual review targets the remainder. That bounded-coverage framing (vendor-claimed) is the right mental model for every objective checker. Contrast as a computable standard is the declared WebAIM purpose but the page was **unreachable (404)** — treat specific ratio thresholds as unverified this run; the general approach (relative-luminance ratio between foreground/background pairs) is implementable from your HSLA token values, and axe-core's own docs confirm a `color-contrast` rule exists in such engines. That objective gates *work* for machine-emitted UI is **[inference]** from lint precedent, not measured.

**Cost/machinery.** Moderate; the notable point for this repo is that axe-core is HTML/DOM/web-only — the *pattern* transfers to a gpui/accesskit tree, the tool does not.

---

## 5. Golden-image / visual regression review

**What it is.** Commit reference screenshots; every run re-renders and pixel-diffs; changes are reviewed like code diffs.

**Evidence.** Playwright docs (durable, first-party): first run *writes* the golden ("A snapshot doesn't exist… writing actual"); goldens live in a per-test `-snapshots` directory that "you should commit… to your version control (e.g., git), and review any changes to it"; comparison uses **pixelmatch** with a `maxDiffPixels` tolerance; snapshot names embed browser+platform because rendering differs; `--update-snapshots` is the deliberate refresh path; `stylePath` injects CSS to hide volatile elements "improving the screenshot determinism." Evidence this improves *AI-produced* UI: **[inference]** — it gates *regression*, not quality; its role for agents is as the "unit tests for looks" that stop good states from silently degrading. Chromatic (declared as the PR-gated review-at-scale exemplar) was **unreachable**, so nothing about its workflow could be verified.

**Cost/machinery.** Low if you already have deterministic capture — this repo does, on Linux only (grim inside the sway session; tree-backed features are hard-gated on macOS because the a11y tree is never populated there — `crates/debug/src/gate.rs`, #401).

---

## 6. Reference-anchored generation (exemplars, style imitation, galleries)

**What it is.** Give the agent a visual target or exemplar set rather than a verbal goal.

**Evidence.** Design2Code is the measured core: multimodal prompting on screenshot references, with automatic + human evaluation — and its negative result is the actionable part: models lag at **element recall and layout**, so screenshot references alone under-constrain structure. Combining a visual reference *with* a structural description is the obvious compensation (**[inference]**). Refactoring UI's component gallery (200+ component/layout ideas, "medium-fidelity mockups… just enough fidelity to be useful," explicitly *without* CSS "so the gallery [doesn't] encourage you to make every project look the same") is the curated-exemplar pattern in commercial form — anecdotal evidence, part of the paid package.

**Cost/machinery.** A `refs/` folder of screenshots of native apps the owner likes + (optionally) the $99 book package.

---

## 7. Multi-agent generator/critic loops

**What it is.** One agent builds UI, a separate critic agent (possibly with different tools: tree dump, screenshot, rubric) reviews and rejects until satisfied.

**Evidence.** **None of the fetched sources describes or measures multi-agent UI loops.** The nearest verified components: Design2Code's automatic-evaluation machinery (metrics validated against humans) and axe's automated-checks-inside-tests integration. Treat the generator/critic *pattern* as **[inference]** built from measured parts — worth piloting, not worth citing as established.

---

## 8. Combinations the sources actually report working together

Only these pairings are source-backed; the full-stack combination for agents is my synthesis:

1. **Tokens → translation → single source of truth** (DTCG's own stated pipeline: one token file feeding code generation and docs, eliminating per-tool "glue" code).
2. **Goldens + committed artifacts + deterministic environment + volatility masking + explicit update path** (Playwright's complete recipe; each element exists because the others fail without it).
3. **Automated checks inside existing functional tests + explicit manual-review remainder** (axe's model: automate the ~57%, route "incomplete" to humans).
4. **Automatic metrics validated by human evaluation** (Design2Code's methodology — machine judgment calibrated against owner judgment).

**[Inference] stack for this repo:** tokens bound the *emission* space (clippy) → objective tree/pixel checks bound the *acceptance* space (CI) → rubric + reference critique fills what automation can't see (per-PR protocol) → goldens prevent regressions. The four source-backed pairings slot in exactly those slots; no fetched source tests the whole stack on AI agents.

---

## 9. Shortlist: what this Rust+gpui repo can adopt cheaply

Grounded in what's actually in the tree: clippy token-discipline gate (`clippy.toml`: `gpui::rgb/rgba/hsla`, `Styled::text_size/line_height` banned, docs/0002 D5/D9/D10); the debug bridge (`crates/debug`, docs/0010/0055) with `tree [--json] [--snapshot | --diff a b | --tab-stops]`, `screenshot [--name]` via grim, scripted receipts (`crates/debug/verify/*.txt`: `snapshot`/`diff`/`assert-diff-label`/`assert-label`/`click`/`key`/`wait-frame`), fixed-size headless sway sessions, dev builds adding source locations to dumps; file-based themes (docs/0025, 0056); spacing via the `gap_N` scale (docs/0002 D9 "spacing goes through the scale"); PR discipline in AGENTS.md (gates, "exercise the change through the debug bridge first," owner human-tests).

**Adopt now (days, ~$0):**
1. **One `tokens.json` (DTCG-subset: color/dimension/font-family groups + aliases) as source of truth** → small codegen into the theme module; file-based themes (0025) become a *view* of it. Kills ad-hoc values at the source rather than at lint time.
2. **Tree-based objective gate** — a CI receipt script that runs `tree --json` in a session and fails on: interactive nodes without role+label (already an AGENTS.md D10 rule — make it mechanical), spacing values off the `gap_N` scale, controls below minimum size, tab-stop anomalies (the numbered `--tab-stops` walk), duplicate labels. This is the axe-core pattern re-implemented on the accesskit tree **[inference]**; all inputs already exist in the dump (roles, labels, bounds, focus).
3. **Token-pair contrast check** at codegen/test time for every text/background token combination; thresholds unverified this run (WebAIM 404) — re-check the standard before hardcoding numbers.
4. **Golden screenshots on Linux CI**, Playwright-style: committed `screenshots/` goldens keyed by size/theme (the fixed `--size WxH` session is your "same environment" guarantee), pixel-diff with a small tolerance (pixelmatch is Playwright's precedent engine), masking of volatile regions, explicit update command. Rust on macOS keeps working; the visual gate runs only where grim works.
5. **Rubric protocol in PR bodies** — encode NN/g's ten (with #4, #8, #6, #1, #9 called out) plus Refactoring UI's chapter titles as a fixed checklist; the agent attaches the session screenshot + tree excerpt per item; the owner marks pass/fail instead of writing prose. AGENTS.md already requires bridge exercise "and say so in the PR body" — this upgrades that line from ritual to review artifact.

**Adopt next (a week or two):**
6. **Screenshot-in-the-loop critique**: receipt script renders a view, saves screenshot + `tree` snapshot, an MLLM pass critiques screenshot-vs-reference against the rubric, iterate-until-pass. Measured basis: Design2Code; loop framing **[inference]**. Include the *tree diff*, not just pixels — models lag at layout, and structure is what your bridge sees best.
7. **Reference set + mini-gallery**: `refs/` of owner-approved native-app screenshots; per-view receipt scripts + screenshots become a poor-man's Storybook **[inference]** — a regression corpus *and* the "compose from these first" vocabulary for new views.

**Defer:** Chromatic-style SaaS (unreachable source, PR-gating infra overkill for two people); multi-agent loops (no source evidence); macOS tree/screenshot support (already tracked as #401 / in-app frame capture follow-up — don't block on it; the Linux session is the canonical environment).

**Sequencing:** 1→2→3 (one PR: tokens + gates), then 4, then 5, then 6–7. Optional spend: Refactoring UI $99–149 (anecdotal but cheap, one-time).

---

## Honest limitations

- Two declared sources (Chromatic, WebAIM) returned persistent 404s and one (Material 3) yielded no content; all three are excluded from claims above. Contrast thresholds in particular need re-verification.
- No fetched source measures any of these approaches *for AI-agent-authored UI* or *for native/gpui*; Design2Code (webpages, one-shot generation) is the only measured study, and its result cuts both ways — vision evaluation works, layout recall is the weak spot.
- The DTCG spec is an unstable draft that says "do not implement this version"; adopt its concepts, not version-specific details.
- The multi-stack combination and the owner-veto checklist mechanics are inferences from the sources plus this repository's actual tooling, not findings.