# Principles

> This document describes the rules currently in force. A sentence here that is wrong is a bug.

## 1. Determinism is non-negotiable

The same model, options and change set produce the same output, byte for byte for SVG. The library never calls an inference endpoint, never embeds a model, and never guesses.

An entity the renderer cannot draw faithfully is **reported as unsupported**, not approximated in silence. A caller can always tell which parts of the picture are faithful and which are not.

*What this costs:* output with visible gaps and an "unsupported" list, where a best-effort renderer would have produced a complete-looking picture.

## 2. A render is not evidence

Pictures are for people, and for an agent's fallback view. Whether an edit is correct is established by a numeric diff, never by looking at a render. Nothing in this library compares images, and no test here decides pass/fail by comparing rendered images — snapshot tests compare the deterministic SVG text.

## 3. The original is drawn unchanged

In an overlay, the original geometry is rendered exactly as it would be alone. Changes live on a separate layer on top. A reader can always tell what is original and what is proposed.

## 4. Confidence is visible

The entity model carries a **provenance** and a **confidence** for every entity. The renderer can distinguish low-confidence and "unknown" content visually, and never draws it as if it were certain. It never raises a confidence.

## 5. The input is never modified

Models and change sets are read-only. Output is a new value.

## 6. Domain neutrality

The library knows how to draw CAD entities and change sets. It does not know what the drawing or the change is *for*. Labels, legends and wording that only one consumer needs belong in that consumer's adapter, including when they are dressed in generic-sounding names.

## 7. Trade-off order

When goals collide, the earlier one wins:

> API simplicity › coverage › development speed › backward compatibility

Determinism is not on this list because it is never traded. Licensing is not on this list because it is a constraint: the dependency tree of this crate is permissive-only (MIT / Apache-2.0 / BSD).

## 8. Compatibility

The crate is in 0.x. When a more correct design is found, a breaking change is the normal way to adopt it; it is not deferred for migration cost. Breaking changes bump the minor version. The major version is not bumped without an explicit maintainer decision.

## 9. Contributing

| Just do it | Propose first | Discuss before any work |
|---|---|---|
| Tests · bug fixes and refactors that leave the public API unchanged · docs · support for another entity type · a new field on a result the library returns | Public API changes · new dependencies · changes to the overlay's visual language | Anything that adds inference · anything in "What it is not" · copyleft dependencies · changes to how confidence is shown |

If it is unclear which column a change falls in, treat it as the stricter one.

Tests for things the library **must not do** — altering the original in an overlay, drawing low-confidence content as certain, dropping an entity without reporting it — are required, and their failure count is always zero.
