All fetchable sources are grounded, and the repo facts (clippy token lints, docs/0002 D5/D9/D10 token scales, docs/0025 JSON theme files, docs/0010/0055 debug bridge) are verified. Here is the synthesis.

---

# Systematic approaches for AI-agent UI quality — synthesis for a two-person Rust + gpui project

**Evidence labels used:** `[S]` = stated by a fetched source; `[S-vendor]` = stated by source but vendor/commercial self-interest; `[P]` = primary research (peer-reviewed venue); `[I]` = inference/reasoned composition by this synthesis — not claimed by any source.

## 0. Source status

| Declared source | Fetch result |
|---|---|
| NN/g, 10 Usability Heuristics | Fetched, full content `[S]` |
| Refactoring UI (landing page + full TOC) | Fetched — commercial book page; efficacy evidence is testimonials only `[S-vendor]` |
| DTCG Design Tokens Format | Fetched — **note: redirected to a preview draft** explicitly marked "Do not attempt to implement this version" (`/tr/drafts/format/`, 2025.10 edition); content below reflects what that draft says, not a frozen standard `[S]` |
| Playwright visual comparisons | Fetched, full content `[S]` |
| Chromatic visual tests | **HTTP 404 at the declared URL** (retried) — content unverified; only its declared purpose is used, marked `[I]` |
| WebAIM contrast | **HTTP 404 via fetcher** (retried) — contrast thresholds below are flagged as unverified general knowledge |
| axe-core (GitHub README) | Fetched `[S]`; effectiveness figure is Deque's own `[S-vendor]` |
| M3 design tokens overview | Fetched but the page is a JS shell with no text content ("Design tokens – Material Design 3") — unverified |
| Design2Code, arXiv:2403.03163 (NAACL 2025) | Fetched, abstract verified `[P]` |

**Overall evidence caveat:** none of the fetched sources measures *AI-agent-produced native-desktop UI* quality. The closest primary evidence (Design2Code) covers MLLMs generating **web (HTML)** code from screenshots. Everything about applying these mechanisms to a gpui app is `[I]`.

## 1. The distinct systematic approaches found

### A. Design-token constraint systems (colors / spacing / type scales)
- **What it is:** encode design decisions as named, typed, platform-agnostic values so emitted UI can only draw from an approved vocabulary. The DTCG draft defines a JSON format: a token is an object with `$value` (required) plus optional `$type` (color, dimension, fontFamily, fontWeight, duration, cubicBezier, number), grouped hierarchically; aliases reference other tokens (`{group.token}`); composite tokens bundle related properties (typography, border, shadow, transition); files are `.tokens.json` with MIME `application/design-tokens+json` `[S]`. The spec's stated goal is tool **interoperability**: a single token source feeding "translation tools" (it names Style Dictionary, Terrazzo) that emit platform-specific code `[S]`. M3's page (token system as a machine-readable layer) was unrenderable — unverified.
- **Evidence it improves AI-produced UI:** none of the fetched sources measures this for AI agents. The prior-decomposition claim in your context ("decent composition when constrained to tokens and component vocabularies") is consistent with the mechanism but is not evidenced by these sources `[I]`.
- **Cost/machinery:** low-to-medium. You already run the enforceable version: `clippy.toml` bans `gpui::rgb/rgba/hsla`, `text_size`, `line_height`; docs/0002 D9 fixes a type scale (`text_xs`–`text_xl` = 12–20px), D10 puts spacing on a macro-generated 0.25rem grid. Adopting actual DTCG files would be **additional** machinery with interop payoff only if tokens flow between tools — for one Rust app the payoff is mostly discipline + machine-checkability of the token file itself `[I]`.

### B. Visual-design tactic canon (rubric for *how it looks*)
- **What it is:** Refactoring UI packages developer-aimed, checkable tactics rather than "principles": its TOC includes "Limit your choices," "Establish a spacing and sizing system," "Establish a type scale," "Hierarchy is everything," "De-emphasize to emphasize," "Start with too much white space," "Avoid ambiguous spacing," "Ditch hex for HSL," "Define your shades up front," "Use fewer borders" (use shadow/contrast/space instead), "Don't rely on color alone"; plus a 200+-item component gallery and pre-built 10-shade palettes as reference exemplars `[S]`. These are exactly the shape of a critique checklist: symptom → fix.
- **Evidence:** anecdotal only — testimonials; it is a $99–149 commercial product (free chapters exist) `[S-vendor]`. The tactics largely overlap with what your tokens already enforce, which makes them cheap to convert into review prompts/checks `[I]`.
- **Cost/machinery:** trivial (checklist text); the gallery/palettes cost money and are web-styled — style-transfer risk for a native desktop app `[I]`.

### C. Rubric/heuristic-based review (usability layer)
- **What it is:** Nielsen's 10 heuristics (visibility of system status; match with real world; user control & freedom; consistency & standards; error prevention; recognition over recall; flexibility & efficiency; aesthetic & minimalist design; error recognition/recovery; help & documentation), used as an *inspection* rubric — "broad rules of thumb, not specific usability guidelines" `[S]`. Origin: heuristic evaluation (Molich & Nielsen 1990), refined 1994 by factor analysis of 249 usability problems `[S]`.
- **Evidence:** durable as a human expert-inspection method (30+ years, per the page); **no evidence in these sources that an LLM applying them improves UI** — that adaptation is `[I]`. For your situation the key property is different: the rubric converts an owner's "not good at all" into named, per-item verdicts, which is the mechanism an agent can act on `[I]`.
- **Cost/machinery:** low — a checklist applied per-PR against tree dumps + screenshots; several items (status visibility, error messages, undo, tab order) map to observable facts your bridge already exposes `[I]`.

### D. Automated objective checks (contrast, structure, a11y rules)
- **What it is:** lint-style gates computed from values/structure. axe-core is the precedent: a rule engine for WCAG 2.0/2.1/2.2 (A/AA/AAA) plus best practices, run inside existing functional tests; claims to find "on average 57% of WCAG issues automatically," returns zero false positives by design, and marks uncertain results "incomplete" for manual review `[S, S-vendor for the 57%]`. Contrast is computable from color values (WCAG ratio thresholds; the WebAIM page that would pin exact numbers was inaccessible, so treat specific ratios as unverified general knowledge). Important: axe-core is built for **HTML-based UIs** — not directly runnable against gpui; what transfers is the *pattern*: a small deterministic rule set over a machine-readable tree, run in CI, with an "incomplete → human" escape `[I]`.
- **Evidence:** the 57%-subset claim is vendor-measured `[S-vendor]`; the deeper point it supports — automation catches a bounded subset and must be paired with review — is explicitly stated in the README `[S]`.
- **Cost/machinery:** low. Your `tree --json` (roles, labels, bounds, focus, source locations) plus theme JSON are sufficient inputs for a home-grown rule subset `[I]`.

### E. Golden-image / visual regression testing
- **What it is (verified from Playwright docs):** `toHaveScreenshot()` — first run **generates** the baseline ("A snapshot doesn't exist… writing actual"); later runs compare via the pixelmatch algorithm with a tolerance (`maxDiffPixels`); baselines are named per browser+platform (`example-test-1-chromium-darwin.png`) because rendering differs across them; explicit update flow (`--update-snapshots`); `stylePath` injects CSS to hide volatile elements for determinism; a strong warning that rendering varies by host OS, version, settings, hardware, power source, and headless mode — "run tests in the same environment where the baseline screenshots were generated"; snapshot directories are committed to VCS and **changes to them are reviewed** `[S]`. A notable determinism trick: on first capture Playwright "took a bunch of screenshots until two consecutive screenshots matched" before saving the golden `[S]`. Non-image (`toMatchSnapshot`) covers text/structure too `[S]`.
- **Evidence:** these are established testing mechanics, not an intervention study; the claim they support is regression *detection*, not aesthetic *improvement* `[I]`.
- **Cost/machinery:** low–medium. You already have: grim PNG receipts in pinned headless sway (docs/0055 D10, grim 1.5.0 via nix), fixed `--size`, `wait-frame`, replayable receipt scripts. Missing piece: a pixel-diff with threshold + baseline naming + update verb. The repo already depends on `image` 0.25 (PNG feature) — enough to implement a small pixelmatch-style differ `[I]`.

### F. PR-gated visual review workflow
- **What it is:** screenshot diffs surfaced at review time so a human approves/rejects visual changes (Chromatic's declared purpose). **The declared page was inaccessible (404), so this entry is `[I]` only** — treat the concept as unverified from this source set; the reviewable-committed-baselines pattern it rests on is, however, verified via Playwright ("review any changes to it") `[S]`.
- **Fit:** paid SaaS, web-centric — poor fit; the *workflow* (diff artifacts attached to PRs) you already approximate with receipt scripts `[I]`.

### G. Reference-anchored generation + vision-in-the-loop critique
- **What it is:** give a model a rendered/visual reference and have it generate or evaluate against it. Design2Code `[P]`: 484 curated real webpages as a benchmark; MLLMs (GPT-4o, GPT-4V, Gemini, Claude) given screenshots as input to generate code that re-renders them; **automatic evaluation metrics** were built and validated by comprehensive human evaluations; headline finding: "models mostly lag in recalling visual elements from the input webpages and generating correct layout designs" `[S/P]`.
- **What it evidences:** (1) MLLMs can meaningfully consume screenshots and produce visual-output judgments — the premise of a screenshot-critique loop; (2) automatic visual metrics can rank outcomes well enough that humans validated the ranking — i.e., a machine gate can stand in for most human passes; (3) **layout and element recall are the weak spots** — so a critique loop fed only pixels will underperform one fed pixels *plus structured geometry* (your tree dump: bounds, roles, labels) `[I on the transfer, S on the finding]`.
- **Cost/machinery:** medium. Needs a vision-capable model pass per iteration + the screenshot path (Linux-only today). Iterate-until-pass with a small iteration cap; the "pass" criteria come from B/C/D `[I].

## 2. Combinations the sources actually support

1. **Committed baselines + deterministic environment + human review of diffs** — verified end-to-end by Playwright `[S]`: same-env generation, tolerance thresholds, explicit updates, VCS review. This is the strongest "works together" finding in the set.
2. **Automatic metrics + human evaluation as complements** — Design2Code validated its automatic metrics with human evals `[P]`. Analog for you: machine gates do the iterating; the owner only approves final before/after pairs `[I]`.
3. **Automated subset + earlier linting + manual remainder** — axe-core's own pipeline recommendation (rules in functional tests, linter earlier, guided/manual for the rest) `[S]`.
4. **Tokens as single source of truth feeding translation into platform code** — DTCG's stated interop model `[S]`; you already enforce the consumer side via clippy.
5. **Rubric + objective facts** — NN/g heuristics are deliberately broad; they need concrete observables (your tree dump) to become checkable `[S for breadth, I for the pairing]`.
6. **Pixels + structured geometry for critique** — motivated by Design2Code's layout-recall finding `[P→I]`.

## 3. Shortlist for this repo (cheapest-first, grounded in verified repo facts)

1. **Contrast gate over theme JSON — no rendering, macOS-safe, ~days.** Themes are already JSON (`ThemeSet` files; `Default Light` 99 color keys / `Default Dark` 94, per docs/0025). Enumerate the fg/bg token pairs your UI actually composes, compute WCAG-style ratios, fail CI. (Threshold values unverified from sources — pin them from WCAG directly.)
2. **`audit` verb over `tree --json` — alignment/spacing/density/rhythm checks, no rendering, macOS-safe.** Bounds exist per node; spacing is token-gridded (0.25rem, docs/0002 D10), so off-grid gaps, misaligned siblings, and inconsistent padding are computable. Precedent: axe-style rule engine; the specific rules are your design `[I]`.
3. **Structural a11y rules over the same dump:** unlabeled interactive nodes, missing roles/ids (AGENTS.md already treats a missing node as a bug), tab-stop sanity via `tree --tab-stops` (docs/0055 D8). Catches a documented axe-style subset; "incomplete" cases stay on the owner's checklist `[I]`.
4. **Golden screenshots on Linux CI, Playwright semantics — ~1–2 weeks.** Your pinned headless-sway session is exactly the "same environment" Playwright demands; add: double-capture-until-stable, per-theme/per-size baseline names, `maxDiffPixels`-style tolerance, explicit update verb, and PR receipts that attach the diff. Use `tree --snapshot/--diff` as the platform-independent golden until in-app frame capture lands for macOS (known follow-up).
5. **Bounded vision-critique loop — medium, highest upside.** Per UI PR: screenshot + tree JSON + a rubric built from the NN/g 10 `[S]` and Refactoring UI's tactic list `[S-vendor]` → model critique naming violated items → fix → re-screenshot; cap iterations; final pair goes to the owner. This directly answers "owner can't articulate fixes": the rubric supplies the vocabulary, the loop supplies the iterations.
6. **Reference anchoring:** commit a small set of owner-approved screenshots per surface as exemplars; critiques diff against them (the Refactoring UI gallery/palettes are the paid analog — optional, web-styled, use with care for a native look).
7. **Skip for now:** DTCG file migration (medium cost, interop payoff you don't need yet — your ThemeSet JSON is already machine-readable and clippy already enforces consumption) and any cloud visual-review SaaS.

**Sequencing `[I]`:** 1–3 are deterministic gates that make every later vision pass cheaper (fewer false starts); 4 prevents the vision-driven refactors from silently regressing; 5–6 are where the "not good at all" verdict becomes actionable. The combined effect on agent-authored gpui UIs is unmeasured by any fetched source — your receipts will generate that evidence.