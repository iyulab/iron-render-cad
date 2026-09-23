# Caveats

What this renderer approximates, what it leaves out, and what has never been checked
against a reference rendering. Each item is a fact about the current code; when the code
changes, the item changes with it.

## Deliberate approximations

Several entity types are drawn as approximations rather than faithfully. Every one of
them is still drawn (not reported as unsupported), because a recognizable picture is more
useful than a gap -- but a reader should know which parts are approximate:

- **Curves as chords**: curved HATCH edges are drawn as polylines through sampled
  points. A SPLINE stored by control points is evaluated as the NURBS curve its degree,
  knots and weights define, and drawn through 16 points per knot span; a SPLINE stored
  by fit points only is drawn through its fit points. A spline whose knots or weights do
  not add up to a definition is drawn as its control polygon, which does not lie on the
  curve. (ARC and ELLIPSE, full or partial, are exact SVG arcs; a mirrored ellipse -- normal
  (0, 0, -1) -- runs the other way. An ELLIPSE on a tilted plane is drawn through 64 points
  of its outline seen from above.)
- **3DSOLID / REGION / POLYLINE_PFACE as isometric wireframes**: the model carries a
  solid's edges only; the renderer projects them isometrically and draws the lines.
- **VIEWPORT and WIPEOUT as outlines**: a viewport's frame, a wipeout's clip boundary;
  neither the viewport's contents nor the wipeout's masking are rendered.
- **TOLERANCE as plain text**: the feature-control-frame string is drawn as text, with its
  symbol escapes unstripped.
- **LIGHT as a marker**: a small marker at the position, and a dashed line to the target
  for a distant or spot light. A light whose file does not state its kind gets the marker
  alone.
- **LEADER arrowheads only where the file states one**: a leader whose file omits the
  arrowhead flag is drawn without an arrowhead.
- **MTEXT formatting codes are stripped**, not interpreted: the text is drawn plain.
- **Invisible entities are not drawn**: an entity the drawing marks invisible (a dynamic
  block's hidden visibility states) is left out, and does not count towards the extent. A
  block reference whose block holds only such entities is reported in `empty_blocks`.
- **MTEXT is placed by its attachment point**, one line per paragraph, with the line
  height taken as 1.2 times the text height times the spacing factor -- an estimate, since
  real line breaks and glyph metrics depend on the font. Where the drawing does not state
  the attachment, the insertion point is used as the first line's baseline.

What the renderer cannot draw at all (a type the model has no shape for arrives as
`Entity::Unknown`) is reported in `ToSvgResult::unsupported_types`, sorted by name. The
one type that will stay there for good is `ACAD_PROXY_ENTITY`: an opaque per-application
blob with no geometry.

## Colors: white becomes black

The renderer targets a plain white background. ACI index 7 (`0xFFFFFF`, "white/black")
is AutoCAD's own auto-invert-by-background special case, and a LAYER entry's color can
report as white unconditionally, so any resolved pure-white color is flipped to black --
otherwise it is silently invisible, white on white. Applied at every color-producing path,
not just literal index 7. There is no option for a dark background.

There is no single canonical ACI-to-RGB table: AutoCAD's displayed colors depend on the
drawing-area background. The palette the model carries is the long-published one; see
`uncad-model`'s `color` module.

## HATCH pattern fill

One SVG `<pattern>` element per definition line, accumulated into `<defs>` and emitted
once, with the HATCH boundary path filled via `fill="url(#...)"`. Clipping is delegated to
SVG's own fill mechanism rather than hand-rolled polygon clipping, so a boundary with
several loops or islands keeps working through the `fill-rule="evenodd"` path.

Two deliberate simplifications (see `render_pattern_line` in `src/svg/hatch.rs`): the
component of a definition line's `offset` parallel to the line direction, used for
brick-style staggering, is ignored and only the perpendicular spacing is honored; and the
line is drawn at the center of its tile rather than exactly on `base_point`, so
`<pattern>`'s default tile-edge clipping cannot cut it in half. Being half a tile out of
phase is immaterial for an infinitely repeating pattern.

**A bug the first implementation had, and the lesson.** Standard DXF documentation
describes the HATCH pattern angle and scale (groups 52/41) as applying on top of the
definition-line data (group 78), and the first version multiplied them back in. On a real
file no pattern appeared at all -- hatches inside small door and furniture symbols came
out empty -- because the parsed definition-line data *already* has 52/41 applied: a HATCH
with a 90-degree pattern angle had a definition line whose own angle was also exactly 90,
and a scale of 60 multiplied into a ~6.5-unit spacing gave ~390 units for a shape only ~90
units across, so not one line fell inside a tile. The model now states that the values it
carries are final, and this renderer uses them as they are. The lesson generalizes: do not
take the DXF specification at face value -- check what the parser actually delivers,
against a real file.

Unverified: whether `pattern_type` (user-defined / predefined / custom) should change how
definition-line data is read. Every HATCH in the spot-check files was predefined, so no
case has yet distinguished them.

## HATCH gradient fill (unverified)

No drawing checked so far contains a gradient-fill HATCH, so this rests on the DXF
reference rather than on comparison with an AutoCAD rendering. The gradient name collapses
to a binary choice in the model (`is_radial`): the two spherical names become an SVG
`radialGradient` (close to AutoCAD's center-out look), everything else -- including
`CURVED` and `CYLINDER`, which are directional but not radial in AutoCAD too -- a
`linearGradient`. The two color stops are drawn as the model gives them. The gradient
shift ("Centered") is not applied.

## Every number the renderer takes from the file is bounded

A count, a scale or a spacing in a drawing is whatever the file says, and a corrupt one
becomes an allocation size or a loop bound. So each is capped, far above anything a real
drawing reaches, and every render reports what a cap left out in `ToSvgResult::limits` /
`ToPngResult::limits` (a `LimitReport`: counts, and the IDs of the first 100 entities with
the cap that acted on each). The caps:

- Block references nest at most 20 deep and one render expands at most 100,000 of them;
  a reference past either is not followed.
- One render emits at most 64 MiB of drawing body; an entity whose turn comes after that is
  not drawn.
- One top-level entity emits at most 16 MiB; past that its block expansion stops at the
  next entity boundary, and what it drew is kept and reported as truncated.
- One entity is drawn with at most 100,000 points (a polyline's vertices, a spline's points,
  a hatch boundary's points times the paths drawn through them); a larger one is not drawn.
- A HATCH pattern's tile may be at most 16 times the boundary's diagonal: the tile is the
  rasterizer's pixmap, so a corrupt spacing of 1e12 over a ten-unit shape asks for a pixmap
  1e11 pixels on a side. The pattern is dropped and the outline kept.
- An entity drawn from a coordinate, size or angle that is not a real number (`NaN`,
  infinite) is not drawn: it names no place, and SVG has no way to write it.

A block holding one LINE and eight INSERTs of itself used to expand into a million
references and a 139 MB SVG before anything stopped it.

## PNG size is bounded

A PNG's pixel size is the viewBox's size in drawing units times `scale`, and the viewBox
comes from the drawing's own coordinates. Neither side may exceed `ToPngOptions::max_edge`
(default 8192 px; `svg_to_png` always applies the default): a larger request fails with
`PngError::TooLarge` before any pixel memory is allocated, because the pixmap's allocation
cannot fail gracefully -- a request the allocator refuses aborts the process. One corrupt
LibreDWG corpus file (`example_2000.dwg`) asked for 36 TB this way.

The rasterizer itself can panic: tiny-skia asserts instead of returning an error when a
path's coordinates overflow its fixed-point scan converter. The panic is caught and returned
as `PngError::RenderPanic` with its message; no image is produced for that call.

## Fonts

PNG rasterization loads the host's installed system fonts on every call and bundles none
of its own. A host with no matching font renders `<text>` elements (dimension and MTEXT
labels) as blank rather than failing -- an unresolved glyph is empty, not an error.

## No image comparison, by design

No test in this crate decides pass/fail by comparing rendered images, and the crate has no
image-comparison dependency. Determinism tests compare the SVG text byte for byte and the
PNG bytes exactly; a PNG's validity is checked from its header bytes. This is the second
principle in `docs/principles.md`, and the ecosystem's audit script checks it.
