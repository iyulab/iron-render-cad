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
- **3DSOLID / REGION / POLYLINE_PFACE as wireframes**: the model carries a solid's edges
  only, and the renderer draws the lines. A body flat in a plane parallel to XY (a REGION
  is a closed 2D profile, so usually) is drawn in plan, where the file puts it; a body with
  depth is projected isometrically.
- **VIEWPORT and WIPEOUT as outlines**: a viewport's frame, a wipeout's clip boundary;
  neither the viewport's contents nor the wipeout's masking are rendered.
- **TOLERANCE as plain text**: the feature-control-frame string is drawn as text, with its
  symbol escapes unstripped. A frame whose file states no height is drawn at the text
  height (DIMTXT) of the dimension style it names, and at 1 when that states none either.
- **LIGHT as a marker**: a small marker at the position, and a dashed line to the target
  for a distant or spot light. A light whose file does not state its kind gets the marker
  alone.
- **LEADER arrowheads only where the file states one**: a leader whose file omits the
  arrowhead flag is drawn without an arrowhead.
- **Text codes are decoded to what they show, not drawn as formatting**: MTEXT paragraph
  breaks become lines (a blank paragraph keeps its line), a stacked fraction reads `3 1/2`
  rather than `31/2`, format codes (font, colour, height, width) are dropped, and the
  `%%c`/`%%d`/`%%p` symbol codes and `\U+XXXX` escapes in TEXT, ATTRIB and MTEXT become
  their characters. Everything is drawn in one plain face; underline and overline toggles
  draw nothing. A TEXT whose file stores height 0 ("the style's height", which the model
  does not carry) is drawn at height 1.
- **Invisible entities are not drawn**: an entity the drawing marks invisible (a dynamic
  block's hidden visibility states) is left out, and does not count towards the extent. A
  block reference whose block holds only such entities is reported in `empty_blocks`.
- **MTEXT is placed by its attachment point**, one line per paragraph, with the lines 5/3
  of the text height apart times the spacing factor (AutoCAD's single spacing) -- real line
  breaks and glyph metrics depend on the font. Where the drawing does not state the
  attachment, the insertion point is used as the first line's baseline.
- **Text height is the height of the capitals, in an assumed face**: a text of height `h`
  is written at `font-size = h / cap_height`, where `ToSvgOptions::cap_height` is the
  capital height of the face it will be drawn in as a fraction of the em -- 0.7 by default,
  typical of sans-serif faces (Arial 0.716, Segoe UI 0.700; Times New Roman, usvg's default
  family, 0.662). The SVG names no font, so in a face whose ratio differs the capitals are
  that much taller or shorter than the drawing says; a caller that draws with a face of its
  own states its ratio.
- **RAY and XLINE end at the edge of the picture**: a construction line has no end, so only
  its base point counts towards the extent, and the line is drawn dashed from edge to edge
  of the viewBox (grown by a percent of its diagonal) -- or not at all where it misses it.

- **Extents are the drawn shape's own box**: an ARC counts its own box towards the picture's
  extent, not its whole circle's, and an ELLIPSE arc the part that is drawn; a curved entity
  inside a rotated block is measured through all four corners of its box, which contains it
  under any placement but is up to a factor of sqrt 2 larger at 45 degrees.

- **POINT as a cross**: a POINT has no size of its own, so it is drawn as a cross four
  stroke widths across, whatever the drawing's units -- a half-unit dot was sub-pixel
  wherever a unit is under two pixels. Only the point itself counts towards the extent.

What the renderer cannot draw at all (a type the model has no shape for arrives as
`Entity::Unknown`) is reported in `ToSvgResult::unsupported_types`, sorted by name. The
one type that will stay there for good is `ACAD_PROXY_ENTITY`: an opaque per-application
blob with no geometry.

A block reference (INSERT, ACAD_TABLE, DIMENSION) whose block the model does not hold -- a
reference that never resolved, one that points at nothing, or a name with no definition --
draws nothing and is reported in `ToSvgResult::unresolved_block_refs` by its ID; one whose
block is there but draws nothing is reported in `empty_blocks` by the block's name.

## A far-away drawing is written about its own middle

usvg and tiny-skia keep coordinates in `f32`, which at 2.5e8 -- a plan in millimetres at
projected map coordinates -- cannot tell two points 16 units apart. A drawing whose
entities lie more than 32768 units from the world origin (by the median of one reference
point per entity) is therefore written relative to that median, rounded to whole units, and
`ToSvgResult::origin` says which point that is: an SVG unit `(u, v)` is the world point
`(origin.x + u, origin.y - v)`. A block's interior is written about the point its placement
sends to that origin, so a DIMENSION's block, whose children already hold world
coordinates, is shifted too. A drawing nearer the origin is written in world units, as
before. An isometric wireframe of a body with depth (see above) is projected before it is
shifted, so a far-away 3D body can still land at large coordinates.

## Characters XML forbids

XML 1.0 forbids the C0 control characters other than tab, line feed and carriage return,
and U+FFFE/U+FFFF, anywhere in a document. A corrupt or oddly encoded file can leave one in
a text string, and one is enough to make the whole SVG unparseable -- and with it the PNG,
which is rendered by parsing the SVG. The renderer writes U+FFFD in its place; the model
keeps what the file said.

## Colors: white becomes black

The renderer targets a plain white background. ACI index 7 (`0xFFFFFF`, "white/black")
is AutoCAD's own auto-invert-by-background special case, and a LAYER entry's color can
report as white unconditionally, so any resolved pure-white color is flipped to black --
otherwise it is silently invisible, white on white. Applied at every color-producing path,
not just literal index 7. There is no option for a dark background. A PNG is drawn on
opaque white by default (`Background::White`), so a viewer that shows it on a dark page
does not lose the black lines; `Background::Transparent`, and `svg_to_png`, leave the
background transparent. The PNG is written as 8-bit RGBA either way.

Inside a block reference, an entity on layer 0 is drawn on the reference's layer -- the
standard way a symbol drawn on layer 0 takes the color of the layer it is inserted on --
so a BYLAYER color there resolves through the reference's layer (for a nested reference
itself on layer 0, the outermost one's). Every other layer is used as the model states it.
This includes a DIMENSION's block: its layer-0 lines and text take the dimension's layer.

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
  infinite) is not drawn: it names no place, and SVG has no way to write it. Nor is an ARC
  or ELLIPSE whose angle is beyond a million radians, where an `f64` step is coarser than
  1e-10 radians and the value names no direction.
- An entity whose extent reaches more than 1e15 drawing units from the origin is not drawn
  and does not count towards the extent: the viewBox, the stroke width, the padding and
  every dash length are derived from the extent, so one entity at 1e150 would take all of
  them with it. (The Earth's circumference in micrometres is 4e13.)

A block holding one LINE and eight INSERTs of itself used to expand into a million
references and a 139 MB SVG before anything stopped it.

## PNG size is bounded

A PNG's pixel size is the viewBox's size in drawing units times a scale: the one asked for
(`PngSize::Scale`, one pixel a unit by default), or the one that makes the longer side a
given number of pixels (`PngSize::FitLongEdge`). The viewBox comes from the drawing's own
coordinates, so at a fixed scale the file decides the size. Neither side may exceed
`ToPngOptions::max_edge`
(default 8192 px; `svg_to_png` always applies the default): a larger request fails with
`PngError::TooLarge` before any pixel memory is allocated, because the pixmap's allocation
cannot fail gracefully -- a request the allocator refuses aborts the process. One corrupt
LibreDWG corpus file (`example_2000.dwg`) asked for 36 TB this way.

Strokes are about 1/6000th of the viewBox diagonal wide by default, which is below a pixel at
most sizes; `ToPngOptions::stroke_px` sets them in output pixels instead.

The rasterizer itself can panic: tiny-skia asserts instead of returning an error when a
path's coordinates overflow its fixed-point scan converter. The panic is caught and returned
as `PngError::RenderPanic` with its message; no image is produced for that call.

## Fonts

The crate bundles no font. By default (`Fonts::System`) PNG rasterization draws text with
the host's installed fonts, scanned once per process and shared by every later call, so
the picture depends on the host; a host with no matching font renders `<text>` elements
(dimension and MTEXT labels) as blank rather than failing -- an unresolved glyph is empty,
not an error. A caller can hand over font files instead (`Fonts::Custom`): then those are
the only fonts, text is drawn in the family of the first face, and the picture no longer
depends on the host. The caller then also states that face's capital height
(`ToSvgOptions::cap_height`).

## No image comparison, by design

No test in this crate decides pass/fail by comparing rendered images, and the crate has no
image-comparison dependency. Determinism tests compare the SVG text byte for byte and the
PNG bytes exactly; a PNG's validity is checked from its header bytes. This is the second
principle in `docs/principles.md`, and the ecosystem's audit script checks it.
