# Caveats

What this renderer approximates, what it leaves out, and what has never been checked
against a reference rendering. Each item is a fact about the current code; when the code
changes, the item changes with it.

## Deliberate approximations

Several entity types are drawn as approximations rather than faithfully. Every one of
them is still drawn (not reported as unsupported), because a recognizable picture is more
useful than a gap -- but a reader should know which parts are approximate:

- **Curves as chords**: SPLINE, ELLIPSE arcs and curved HATCH edges are drawn as polylines
  through sampled points.
- **3DSOLID / REGION / POLYLINE_PFACE as isometric wireframes**: the model carries a
  solid's edges only; the renderer projects them isometrically and draws the lines.
- **VIEWPORT and WIPEOUT as outlines**: a viewport's frame, a wipeout's clip boundary;
  neither the viewport's contents nor the wipeout's masking are rendered.
- **TOLERANCE as plain text**: the feature-control-frame string is drawn as text, with its
  symbol escapes unstripped.
- **LIGHT as a marker**: a small marker at the position, a dashed line to the target when
  the light aims somewhere.
- **MTEXT formatting codes are stripped**, not interpreted: the text is drawn plain.

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

## Fonts

PNG rasterization loads the host's installed system fonts on every call and bundles none
of its own. A host with no matching font renders `<text>` elements (dimension and MTEXT
labels) as blank rather than failing -- an unresolved glyph is empty, not an error.

## No image comparison, by design

No test in this crate decides pass/fail by comparing rendered images, and the crate has no
image-comparison dependency. Determinism tests compare the SVG text byte for byte and the
PNG bytes exactly; a PNG's validity is checked from its header bytes. This is the second
principle in `docs/principles.md`, and the ecosystem's audit script checks it.
