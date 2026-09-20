# 0046 — Issue templates in the creation flow

Issue: #198 · Tier 3 polish (docs/0029 §3/§5). The new-issue creator
(docs/0024) ignores `.github/ISSUE_TEMPLATE`; repos with structured
templates expect them at creation time. This doc designs template
support for the **issue** creator and scopes out its siblings.

## 1  Upstream reality (verified, not remembered)

All verified against the pinned IDL (`/tmp/gh-schema.graphql`) and
live api.github.com calls:

- **GraphQL `Repository.issueTemplates: [IssueTemplate!]`** exists
  (IDL 55038). `IssueTemplate` carries `name: String!`,
  `filename: String!`, `title: String`, `about: String`,
  `body: String` (flat string — "The suggested issue body"), plus
  `labels`/`assignees` connections (unused by us in v1).
- **Markdown templates only.** A repo with YAML issue-form templates
  in `.github/ISSUE_TEMPLATE/*.yml` returns `[]` — verified on
  `vercel/next.js`, which ships `1.bug_report.yml` + `4.docs_report.yml`
  and yields zero GraphQL templates. The GraphQL field surfaces the
  legacy **Markdown** (`.md`) template family exclusively.
- **No REST endpoint** for issue templates: `GET /repos/{o}/{r}/issues/templates`
  and `/issue-templates` both 404 with unrouted fallback docs links
  (checked on repos with and without templates).
- **`blank_issues_enabled` is not in GraphQL** (no `blankIssue*`
  anywhere in the IDL). It lives only in
  `.github/ISSUE_TEMPLATE/config.yml`, reachable through the Contents
  API — verified: `contents/.github/ISSUE_TEMPLATE/config.yml` on
  next.js returns base64 `blank_issues_enabled: false`.

## 2  Scope valve

**In (v1):**

- Markdown templates via GraphQL (`issueTemplates`) — fetch, picker,
  prefill.
- `blank_issues_enabled` via the Contents API + a minimal parser.
- Full composition with draft persistence (docs/0045).

**Out (filed follow-ups, not this work):**

- **YAML issue-form templates.** GraphQL doesn't surface them; serving
  them faithfully means a form-renderer (dropdowns/checklists/textareas
  → GitHub's structured submit payload) — a sub-project, not a
  prefill. Repos with only forms see exactly today's behavior; no
  regression. Filed as a follow-up when bitten.
- **PR templates** (`PULL_REQUEST_TEMPLATE.md` → new-PR creator body).
  Different upstream mechanism (single markdown file, no picker),
  small but unrelated plumbing. Separate follow-up.
- Template-suggested labels/assignees auto-application — that is
  #276's territory (metadata at creation).

## 3  Decisions

### D1 — transport: one stateless GraphQL lane

`REPO_ISSUE_TEMPLATES_QUERY` (`repository(owner, name) { issueTemplates
{ name filename title about body } }`), a stateless lane in service.rs
(`Kind::RepoIssueTemplates`, the `Kind::RepoBranches` mirror — 0044
D1's reasoning applies verbatim: per-repo, only needed inside the
creator, never snapshot data). Fired once when the issue creator
opens, alongside the existing creator fetches; result lives in the
creator page entity, dies with it. No cache writes, no snapshot bump.

Errors log-and-swallow (0005 §4.7): a failed template fetch leaves the
creator exactly as today — blank-capable, never blocked.

### D2 — picker: a pill row on the creator

When templates exist, the creator gains a **template row** above the
title field (docs/0024 D3 surface): one small pill per template
(`name`), plus a **"Blank"** pill. Selected pill is highlighted;
selection is mutually exclusive — GitHub semantics, one template max.

`about` (if present) renders as muted tooltip text on the pill — same
treatment as the label-pill tooltips (docs/0022 D7 family).

### D3 — prefill: set_value, title and body

Selecting a template prefills:

- **body** ← `template.body` (flat string; includes the markdown
  comment scaffolding — that is the template's actual content);
- **title** ← `template.title`, only when non-empty (most `.md`
  templates ship an empty title; never overwrite with `""`).

Both via `set_value` (event-suppressed — docs/0045 §4: no save loop),
then a single `cx.notify()`.

### D4 — drafts win at open; explicit selection discards

Composition with docs/0045, the arc that needs spelling out:

- **At open, a restored draft beats the template default.** The draft
  is the user's own unsaved words; a template is scaffolding. No
  auto-prefill over a restored draft, and the "Draft restored" hint
  stays up.
- **Picking a template is an explicit overwrite.** The user clicked a
  named scaffolding: title/body get replaced (D3), the restored-draft
  hint retires, and the parked draft is **discarded** (the existing
  `DraftHandle::discard` — cancel + unlink, 0045 D5) so a template
  prefill can't be resurrected as "your draft" later. From then on the
  template content parks as the live draft like any other text.
- The "Blank" pill does the same discard-and-clear (it is the explicit
  "start over" affordance; identical semantics to clearing the text).

### D5 — blank_issues_enabled

- When templates exist, the creator also fetches
  `.github/ISSUE_TEMPLATE/config.yml` through the **Contents API**
  (existing REST lane machinery) and parses **one key**:
  `blank_issues_enabled`. Absent file, absent key, unparsable value →
  blank allowed (GitHub's documented default is enabled).
- `blank_issues_enabled: false` → the "Blank" pill hides and the
  **first template is pre-selected on open** (the creator never starts
  empty). GitHub's own behavior: with blank disabled and only contact
  links, blank issues are simply not offered.
- Parser: a table-tested ~20-line line scan for the single key — no
  `serde_yaml` dependency for one boolean (nothing YAML exists in the
  lockfile today; the file has exactly one well-known key we need).
- **No templates → config is ignored** (regardless of the flag):
  nothing to choose from, the creator stays as today. A forms-only
  repo with blank disabled cannot be honored without YAML forms (§2);
  filed with that follow-up.

### D6 — gate and failure shapes

- The template row renders only when the template fetch returned ≥ 1
  template. Fetch pending → no row (the creator is usable mid-fetch —
  the row appears when data lands, matching the picker-fetch feel).
- Fetch failure / empty → no row; creation unchanged.
- `blank_issues_enabled` fetch failure → blank stays allowed (the
  conservative default; a hidden Blank pill on a blank-allowed repo
  would be a wrong constraint, the reverse would only over-offer).

## 4  Plumbing sketch

- `gql.rs`: `REPO_ISSUE_TEMPLATES_QUERY` + `IssueTemplateNode` parse
  struct + `IssueTemplate` domain struct (`name, title, about, body`).
- `service.rs`: `Kind::RepoIssueTemplates` stateless lane
  (`run_graphql_fetch` mirror of `RepoBranches`), spec builder +
  `apply` no-op (page-consumed result, the 0044 D7 pattern).
- `new_issue_page.rs`: `templates: Fetch<Vec<IssueTemplate>>`,
  `templates_config: Fetch<bool>` (blank-allowed), pill row renderer,
  selection state, D3/D4 prefill + discard wiring.
- Probes: fixture repo with two templates + a config; pills render;
  select → prefill + hint retired + draft file gone; draft-restore
  beats auto-selection; blank-disabled hides Blank and preselects the
  first template; failed fetch → unchanged creator.

## 5  Docs touched

docs/0024 (D3 surface: template row; new D-note referencing this doc),
docs/0029 §3/§5 rows (shipped wording), docs/0044 untouched (PR
templates are a separate follow-up), README index row for this doc.

## 6  Follow-ups filed on merge

- YAML issue-form templates (form renderer; includes honoring
  blank-disabled on forms-only repos).
- PR template prefill in the new-PR creator.
