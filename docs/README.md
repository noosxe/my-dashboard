# Design Docs — `my-dashboard`

This directory contains the design documentation for **my-dashboard**, a native,
cross-platform, multi-modular universal personal dashboard built with Rust and
[gpui-component](https://github.com/longbridge/gpui-component) on top of [GPUI](https://github.com/zed-industries/zed).

## Conventions

- **Naming:** `NNNN-kebab-case-title.md`, zero-padded, monotonically increasing
  (e.g. `0001-bootstrap-hello-world.md`).
- **Status lifecycle:** each doc carries a `Status` header that moves through:
  `Draft` → `Accepted` → `Implemented` → (`Superseded` if replaced by a later doc).
- **Immutability:** once a doc reaches `Accepted`, changes go into a new doc that
  supersedes it, so history is preserved. `Implemented` docs describe what shipped.
- **Content:** design docs should be concrete enough to implement from: decisions,
  rationale, file layouts, code listings, acceptance criteria. Small decisions may be
  recorded inline; sweeping cross-cutting decisions may later be split into an `adr/`
  sub-directory if the volume justifies it.
- **Language:** "MUST / SHOULD / MAY" per RFC 2119.

## Index

| Doc | Title | Status |
| --- | --- | --- |
| [0001](0001-bootstrap-hello-world.md) | Phase 0 — Bootstrap: Hello World gpui-component Application | Implemented (PR #1) |

## Planned docs (roadmap, not yet written)

- 0002 — Application shell & window chrome (TitleBar, window sizing/centering, theme setup)
- 0003 — Multi-module architecture (module trait, registry, dynamic layout)
- 0004 — State management & persistence (settings, layout serialization)
- 0005 — Build, packaging & distribution per OS
