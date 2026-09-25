# Changelog

Notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versioning follows
[Semantic Versioning](https://semver.org/). While the crate is in 0.x, a breaking change
bumps the minor version.

## [Unreleased]

### Changed

- **Breaking:** builds against the `uncad-model` release that carries polyline bulges, entity
  planes, layouts and images (and an `iron-diff-cad` built on it). Upgrade both together; a
  `CadDatabase` from `uncad-model` 0.1 is no longer accepted.
- **Breaking:** `ToSvgOptions::outlier_trim` is replaced by `crop: Crop`: `Crop::Cluster` is
  `outlier_trim: true` (the default), `Crop::Everything` is `false`. `ToSvgOptions` also gains
  `cap_height` and `include_hidden`; struct literals must set them or use `..Default::default()`.
- **Breaking:** `ToPngOptions::scale` is replaced by `size: PngSize`; `PngSize::Scale(s)` is the
  old `scale: s`. `ToPngOptions` gains `stroke_px`, `max_edge`, `fonts` and `background`, and is
  no longer `Copy` (a custom font is owned bytes): clone it where it was copied.
- **Breaking:** `ToSvgResult` and `ToPngResult` gain fields (the reports under Added, and
  `origin`, `view_box`, `view`); code that builds or exhaustively destructures them must follow.
- `to_png` draws on opaque white by default; `Background::Transparent` gives the 0.1.0 output.
  `svg_to_png` stays transparent.
- Entities the drawing hides -- marked invisible, invisible attributes, the DEFPOINTS layer, a
  layer that is off, frozen or not plotted -- are left out and counted in `hidden`;
  `include_hidden` draws them faded instead.
- Text is written at `font-size = height / cap_height`, so capitals come out as tall as the CAD
  text height, and MTEXT lines are 5/3 of the height apart. Every `<text>` carries an `id` naming
  the path of reference IDs that drew it.
- A drawing more than 32768 units from the world origin is written relative to a point near its
  own middle (`ToSvgResult::origin`), so single-precision rasterizing does not quantize it.
- One render expands at most 100,000 block references (was 1,000,000), and a PNG side is at most
  `max_edge` pixels (default 8192, also applied by `svg_to_png`); a larger request fails with
  `PngError::TooLarge` before anything is allocated.
- The host's system fonts are scanned once per process and shared, not on every PNG call.
- The `regex` dependency is gone; `serde` and `iron-diff-cad` are new dependencies.

### Added

- `overlay_to_svg`: draws an `iron-diff-cad` change set on top of the original drawing -- each
  added, modified or removed entity in a proposal colour with a revision cloud -- and reports
  every change as marked or not marked with the reason (`OverlayOptions`, `OverlayFrame`).
- `layout_to_svg` and `layout_to_png`: a paper layout drawn as its sheet, the model shown
  through each viewport at its scale and twist, clipped to its frame, without the layers
  frozen in it (`LayoutError`, `SheetSource`, `ViewportReport`).
- `Scene`: render once and assemble many -- one `Part` per top-level entity with its world box,
  `Scene::svg` for any window and subset of parts, `Scene::png` for any `View`, and
  `Scene::text_boxes`, where each text's glyphs land in a given font.
- `View`, the pixel grid a PNG covers, with `world_to_px` and `px_to_world`; `ToPngResult::view`
  says which world point each pixel of a PNG shows.
- `Crop::Guarded`, which sets aside outliers far larger or farther than the rest of the drawing,
  and `Crop::Window`, an exact world rectangle; `ToSvgResult::crop` names what the frame leaves out.
- The `limits` module: every number from the file that becomes an allocation or a loop bound is
  capped, and what a cap left out is reported in `ToSvgResult::limits` (`LimitReport`).
- `ToSvgResult::unresolved_block_refs`: the block references whose block the model does not hold.
- `PngSize::FitLongEdge`, `ToPngOptions::stroke_px` (strokes in output pixels),
  `Fonts::Custom` (render with these fonts only) and `Background`.
- `PngError::TooLarge`, `PngError::RenderPanic` (a rasterizer panic returned as an error) and
  `PngError::Layout`.
- IMAGE is drawn as a dashed outline of its clip boundary or frame instead of being reported as
  unsupported.

### Fixed

- Curves: a partial ELLIPSE is drawn as its arc, not a whole ellipse; a SPLINE along the curve
  its knots and weights define, not its control polygon; polyline and HATCH bulges as arcs,
  not chords; a HATCH spline edge along its NURBS curve.
- Planes: CIRCLE, ARC, ELLIPSE, polylines, TEXT, ATTRIB, SOLID, TRACE, HATCH and INSERT are drawn
  where their extrusion puts them, so a mirrored part is no longer on the wrong side of the y
  axis; a clockwise HATCH arc or ellipse edge is no longer mirrored.
- Placement: a block's base point lands on the insertion point; geometry on layer 0 in a block
  takes the reference's layer; a nested block reference's attribute values are drawn, once.
- Text: TEXT and ATTRIB are placed by their justification, width factor and oblique
  angle, and MTEXT by its attachment point; `%%` codes, `\U+` escapes and MTEXT codes (stacks,
  blank paragraphs) are drawn as what they mean instead of stripped or shown raw.
- An MLINE is drawn at its own scale; a TOLERANCE with no height at its dimension style's; a flat
  REGION or 3D solid in plan, not isometrically; only the 3DFACE edges the file does not hide.
- A LEADER's arrowhead, and a LIGHT's line to its target, are drawn only where the file states
  them (a light aims when it is distant or spot).
- Extents: an ARC, a partial ELLIPSE and a rotated CIRCLE measure their own boxes, an MTEXT its
  block, and an MLINE its offset lines; a POINT is a cross sized in stroke widths, not a dot that
  vanishes below a pixel.
- RAY and XLINE are drawn only as far as the picture goes, instead of a segment long enough to
  overflow the rasterizer.
- An entity with non-numeric coordinates, or one reaching past 1e15 units, is left out and
  reported instead of distorting or breaking the picture.
- A character XML forbids is written as U+FFFD, and a label containing `@@` can no longer
  corrupt the document.

## [0.1.0] - 2026-09-22

Initial release. `to_svg` and `to_png` render a `uncad_model::CadDatabase` -- model space, paper
space or everything -- to SVG or PNG, byte for byte deterministic; `svg_to_png` rasterizes an
SVG already built. Each render reports the entity types it could not draw and the block
references that drew nothing. Options cover padding, stroke width, the space drawn, outlier
trimming and the PNG scale. The overlay (a change set drawn on top of the original) was not yet
implemented.
