# Audits and research notes

Records kept because a later change will want the reasoning that produced it, not
because they describe the current code. Each entry states what it settles and
whether it can still be trusted as written.

| Document | Status | What it settles |
| --- | --- | --- |
| [2026-09-24-slint-workflow-implementation.md](2026-09-24-slint-workflow-implementation.md) | Current | How the Slint workflow, models, and callback wiring are meant to be layered |
| [2026-09-24-slint-ui-aviutl-research.zh-CN.md](2026-09-24-slint-ui-aviutl-research.zh-CN.md) | Historical research | AviUtl's menu, dock, and shortcut behavior, and what the Slint port could take from it |
| [2026-10-22-slint-desktop-acceptance.md](2026-10-22-slint-desktop-acceptance.md) | Partly superseded | The first end-to-end desktop acceptance run. Its menu-bar finding was fixed in 0.7.0-rc.2 |
| [2026-10-23-slint-ui-parity-gate.md](2026-10-23-slint-ui-parity-gate.md) | Authoritative | The behavior the Slint frontend was held to before Qt was removed; still the reference when a parity question comes up |
| [2026-10-26-qt-retirement.md](2026-10-26-qt-retirement.md) | Authoritative | What was migrated off Qt, what deliberately changed, and what is still open |
| [2026-11-05-aviutl2-ui-analysis.zh-CN.md](2026-11-05-aviutl2-ui-analysis.zh-CN.md) | Active design input | AviUtl2's UI, and which of its conventions this project follows |

## Removed, with their conclusions kept here

Five records were deleted in the 0.9 quality pass because a shipped change
superseded them and nothing linked to them. Their lasting conclusions:

- **2026-09-code-audit.md** — audited the Qt frontend and the C ABI. Both are gone;
  the migration record above covers what matters.
- **2026-09-rust-dependency-boundary.md** — proposed keeping the Qt frontend
  alongside Rust and keeping the C ABI. 0.8 rejected both: `freya-nle-core`
  exposes Rust APIs only.
- **2026-09-slint-ui-parity.md** — the September parity burn-down, every row
  closed. Superseded by the parity gate.
- **2026-10-21-slint-ui-polish.md** — the polish pass that followed it, every item
  fixed.
- **2026-11-08-authoritative-run-justification.md** — why the Rust path became the
  only build. The 0.8.0 CHANGELOG section records the decision.

One conclusion from the deleted parity burn-down is worth keeping, because it is
easy to re-learn the hard way: the pinned Slint version does render `MenuItem`
icons and shortcuts, so the earlier "menus must be drawn by hand" finding no
longer applies.
