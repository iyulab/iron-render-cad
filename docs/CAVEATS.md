# Caveats

What this renderer approximates, what it leaves out, and what has never been checked
against a reference rendering. Each item is a fact about the current code; when the code
changes, the item changes with it.

## Deliberate approximations

Several entity types are drawn as approximations rather than faithfully. Every one of
them is still drawn (not reported as unsupported), because a recognizable picture is more
useful than a gap -- but a reader should know which parts are approximate:

- **Curves as chords**: curved HATCH edges, and the bulged segments of a HATCH polyline
  boundary, are drawn as polylines through sampled points. A SPLINE stored by control points is evaluated as the NURBS curve its degree,
  knots and weights define, and drawn through 16 points per knot span; a SPLINE stored
  by fit points only is drawn through its fit points. A spline whose knots or weights do
  not add up to a definition is drawn as its control polygon, which does not lie on the
  curve. (ARC and ELLIPSE, full or partial, are exact SVG arcs, and so is a bulged segment of an
  LWPOLYLINE or 2D POLYLINE; a mirrored ellipse -- normal (0, 0, -1) -- runs the other way, and a mirrored CIRCLE, ARC, LWPOLYLINE, 2D POLYLINE, SOLID or TRACE -- written in its own coordinate system, extrusion (0, 0, -1) -- is taken to the world through the format's arbitrary axis algorithm and is still exact (a polyline's arcs then turn the other way). A CIRCLE or ARC on a tilted plane is drawn through 64 points per turn of its outline seen from above, and a polyline on one through its vertices and 12 points per arc segment. An ELLIPSE on a tilted plane is drawn through 64 points
  of its outline seen from above. A bulge so small that its arc's radius reaches 1e15 units
  is drawn as the straight segment it all but is. A HATCH written in its own plane -- mirrored
  or tilted -- is drawn as it is written, inside a group that takes that plane to the page seen
  from above; the map is affine, so its boundary, pattern lines and gradient all stay exact,
  though a tilted plane's foreshortening also narrows the strokes across it. A HATCH's fill
  style is not reproduced: every hatch is filled alternating from the outside in, which is the
  normal style; "outermost only" and "ignore islands" need to know which path is outermost,
  which the model does not carry. A TEXT or ATTRIB in its own plane is drawn the same way (a
  mirror copy's reads reversed, as it does in the world). A block reference in a mirror copy's
  plane is placed exactly; one on a tilted plane is placed in its plane and that plane seen
  from above.)
- **Polyline widths are not drawn**: an LWPOLYLINE or POLYLINE_2D is drawn as its centreline
  at the ordinary stroke, whatever its constant or per-vertex widths -- a wide border, a
  tapered arrow and a DONUT's ring all come out as thin lines. The widths are in the model;
  drawing a polyline as the filled outline its widths describe is not implemented.
- **3DSOLID / REGION / POLYLINE_PFACE as wireframes**: the model carries a solid's edges
  only, and the renderer draws the lines. A body flat in a plane parallel to XY (a REGION
  is a closed 2D profile, so usually) is drawn in plan, where the file puts it; a body with
  depth is projected isometrically.
- **VIEWPORT and WIPEOUT as outlines**: a viewport's frame, a wipeout's clip boundary;
  neither the viewport's contents nor the wipeout's masking are rendered by `to_svg` and
  `to_png`. A layout's sheet (`layout_to_svg`, `layout_to_png`) does show the model through
  its viewports; see below.
- **TOLERANCE as plain text**: the feature-control-frame string is drawn as text, with its
  symbol escapes unstripped. A frame whose file states no height is drawn at the text
  height (DIMTXT) of the dimension style it names, and at 1 when that states none either.
- **LIGHT as a marker**: a small marker at the position, and a dashed line to the target
  for a distant or spot light. A light whose file does not state its kind gets the marker
  alone.
- **LEADER arrowheads only where the file states one**: a leader whose file omits the
  arrowhead flag is drawn without an arrowhead.
- **Text codes are decoded to what they show, not drawn as formatting**: MTEXT paragraph
  breaks become lines (a blank paragraph keeps its line), stacked text is drawn inline as
  `top/bottom` and a stacked fraction reads `3 1/2` rather than `31/2`, format codes (font,
  colour, height, width) are dropped, and the `%%c`/`%%d`/`%%p` symbol codes and `\U+XXXX`
  escapes in TEXT, ATTRIB and MTEXT become their characters. A format code that is not closed
  by its `;`, or one the renderer does not know, is drawn as written. Everything is drawn in
  one plain face; underline and overline toggles draw nothing. A TEXT whose file stores height 0 ("the style's height", which the model
  does not carry) is drawn at height 1.
- **What the drawing hides is not drawn**: an entity the drawing marks invisible (a dynamic
  block's hidden visibility states), an attribute whose own invisible flag is set, anything on
  the `DEFPOINTS` layer (which AutoCAD never plots) and anything on a layer that is off, frozen
  or stated not to plot are left out and do not count towards the extent. A layer whose file
  does not say whether it plots is plotted. `ToSvgResult::hidden` counts them, and
  `ToSvgOptions::include_hidden` draws them at half opacity instead -- still outside the
  extent. Inside a block an entity on layer 0 is judged by the reference's layer, like its
  colour; a hidden block reference hides everything it draws, including contents on layers
  that are shown (AutoCAD shows those when the reference's layer is merely off rather than
  frozen). A block reference whose block holds only hidden entities is reported in
  `empty_blocks`.
- **TEXT and ATTRIB are placed by their justification**: a left/baseline text by its start
  point, every other one by its alignment point -- anchored at the start, the middle or the
  end of its baseline, and hung from its baseline, its middle, the top of its capitals or the
  bottom of its descenders. A bottom-justified text assumes descenders of 0.2 em, a Latin `p`
  in the common sans-serif faces. ALIGNED and FIT, which fill the baseline between their two
  points by a stretch only the font's metrics give, are centred between the points at their
  stated height and width factor instead. The width factor stretches the characters and the
  oblique angle slants them (through a `matrix()` on the `<text>`).
- **A text's extent is an estimated box**: a TEXT or ATTRIB counts its anchor point and the
  box its characters are estimated to fill -- 0.6 em per character, the height of its capitals
  -- placed, turned, stretched and slanted the way it is drawn. The glyphs themselves are laid
  out by the face the rasterizer picks, so a wide face runs past the box and a narrow one
  stops short of it.
- **MTEXT is placed by its attachment point**, one line per paragraph, with the lines 5/3
  of the text height apart times the spacing factor (AutoCAD's single spacing) -- real line
  breaks and glyph metrics depend on the font. Where the drawing does not state the
  attachment, the insertion point is used as the first line's baseline. A paragraph is not
  wrapped to the MTEXT's reference rectangle.
- **An MTEXT's extent is its block**: as wide and tall as the file states it (the extents,
  DXF 42/43, that the writing application measured), else as wide as its reference rectangle
  (DXF 41) and as tall as its lines are drawn, else estimated from its lines at 0.6 em per
  character -- hung from the insertion point by its attachment and turned by its rotation,
  the way the text is drawn.
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
  extent, not its whole circle's, an ELLIPSE arc the part that is drawn, and a polyline its
  vertices and each bulged segment's own arc; a curved entity
  inside a rotated block is measured through all four corners of its box, which contains it
  under any placement but is up to a factor of sqrt 2 larger at 45 degrees.

- **MLINE as its offset lines**: one polyline per line its MLINESTYLE defines, each offset
  from the centreline by the style's offset times the MLINE's own scale (DXF 40), all in the
  MLINE's colour; the lines are what count towards the extent. A model not given the scale
  draws the style's offsets as they are, and an MLINE whose style the model does not hold is
  drawn as its centreline.

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

## Planar entities are drawn where their plane puts them

CIRCLE, ARC, LWPOLYLINE and POLYLINE_2D, TEXT, ATTRIB, SOLID and TRACE state their
coordinates in the object coordinate system their `extrusion` defines, and the model carries
them as the file states them. The renderer takes them to the world by the DXF reference's
arbitrary axis algorithm and draws them seen from above. A normal within 1e-9 (relatively) of
the z axis is taken as the z axis; a normal of zero length is drawn as the z axis, the way the
model places a block reference.

- **Mirrored** (normal (0, 0, -1)): x changes sign and every curve stays exact -- an arc runs
  clockwise, a polyline's bulges turn the other way, and a text reads backwards, as a text
  seen from behind does.
- **Tilted**: seen from above a circle is an ellipse, so a circle or an arc is drawn through
  64 points per turn of its outline and each arc of a polyline through 12; a text is
  foreshortened with its plane; a SOLID's corners are exact.

An INSERT in a plane parallel to the world's is placed by the model's own
`InsertEntity::world_transform`, which applies its extrusion the same way; one on a tilted
plane is placed in its plane and that plane seen from above. A HATCH is drawn in its own
plane too, through the same map as a text.

## A paper layout's sheet

`layout_to_svg` and `layout_to_png` draw one paper layout as its sheet: the layout's own
entities (its paper space block), and the model shown through each of its viewports, clipped
to the viewport's frame. The model appears at the viewport's scale -- its frame's height over
its view's -- and turned by its twist, a model point `p` landing at
`C + s (R(twist) (p - T) - V)` on the paper (`C` the frame's centre, `T` the view's target,
`V` its centre in display coordinates). A positive twist turns the picture counter-clockwise,
the convention ezdxf follows; the sign has not been checked against a sheet AutoCAD plotted.
Strokes, POINT crosses and HATCH pattern lines inside a viewport are drawn at the sheet's
stroke width, and each viewport gets its own copy of the patterns it fills with.

- The sheet is the layout's limits when they span a rectangle -- AutoCAD keeps them equal to
  the paper's placement -- and otherwise the paper its plot settings describe: the paper's
  size (turned for a quarter-turned plot, divided by 25.4 for a layout drawn in inches) with
  the printable area's lower-left corner, moved by the plot origin, at the layout's origin.
  A layout that states neither is framed like a render of its paper space. No padding.
- A viewport that is off, or is the layout's overall viewport (numbered 1 in a DXF; in a DWG,
  which numbers none, the one whose view is its own frame), shows nothing. So does a viewport
  whose view the renderer cannot draw -- none stated (older than R2000), no positive height,
  or not a plan view (a 3D view) -- and it is reported in `undrawn_viewports`. Every
  viewport's frame is drawn as paper space draws it, and hidden when its layer is: a
  viewport on a layer that is off or not plotted still shows its view.
- Inside a viewport the model follows the hidden rules above, and the layers frozen in that
  viewport alone are hidden too (counted in `hidden`, faded with `include_hidden`). The model
  is walked once per viewport that shows it, so what it hides counts once per viewport.
- Only the model entities whose extent meets the frame are written into a viewport; a RAY or
  XLINE always is, and is cut at the sheet's edge.
- A perspective view's lens length is not applied: a plan view is parallel.
- The result says what each viewport of the layout shows (`viewports`): its frame, whether
  it is the overall viewport, and -- for one the model is drawn through -- the map from the
  model to the paper it was drawn with and the model window its frame shows (the frame's
  corners taken back into the model; a twisted view shows a turned rectangle). It also says
  where the sheet came from (`sheet`: the limits or the plot settings; `None` for a layout
  framed like a render of its paper space).

## What the picture shows

The viewBox frames a rectangle chosen from the extents the top-level entities measured
(`ToSvgOptions::crop`), padded. The default, `Crop::Cluster`, frames the dominant cluster of
entities whose corners touch. It scores a cluster by its count times its size, so a single
giant -- a block reference a corrupt file scales thousands of times -- can outscore the
drawing itself; when no cluster then holds a majority, nothing is trimmed and the picture is
the giant's. `Crop::Guarded` sets such outliers aside by rule instead: at most max(3, 1 %) of
the entities, each over 20 times the size of the rest or over 20 of the rest's diagonals
away from it, and never more than a fifth of the drawing. The thresholds are rules of thumb.
What it sets aside is not drawn at all, since a giant would cross the picture whatever the
viewBox. It can take an extent the caller states instead (a file header's), under the tests
in `Crop::Guarded`'s documentation. A construction line (RAY, XLINE) measures only its base
point and is never set aside.

Every result lists the entities its picture does not show (`crop.left_out`), with the
reason. One outside the viewBox, under any crop, is still in the SVG document, where a
viewer that pans can reach it.

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
LibreDWG corpus file (`example_2000.dwg`) asked for 36 TB this way. A picture a caller
sizes itself (`Scene::png` with its own `View`) is bounded the same way, by the default.

Every PNG says which pixels it drew (`ToPngResult::view`): its size, its scale and the world
point of its top-left corner, so a pixel can be taken back to the drawing. The scale it
states is the one the rasterizer drew at, which keeps it in single precision; the size is
the viewBox's at that scale rounded to whole pixels, so the last row and column cover up to
half a pixel more or less of the drawing than the viewBox does. `Scene::png` draws exactly
the view it is given instead.

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

While it walks the drawing the renderer has no fonts, so the box a text counts towards the
extent is an estimate: 0.6 em a character and a capital tall. A face wider or narrower than
that -- a Hangul syllable is about 0.9 em -- runs past it or falls short. `Scene::text_boxes`
lays every text of a scene out with the fonts it is given and returns both boxes; the
measured one is the box of the glyph outlines, taken through every enclosing placement, so
for a turned text it is the box of its turned outline box. Every `<text>` carries the path
of the entity that drew it as its `id` (`t` and the reference IDs, outermost block
reference first), which is how the two are matched.

## No image comparison, by design

No test in this crate decides pass/fail by comparing rendered images, and the crate has no
image-comparison dependency. Determinism tests compare the SVG text byte for byte and the
PNG bytes exactly; a PNG's validity is checked from its header bytes. This is the second
principle in `docs/principles.md`, and the ecosystem's audit script checks it.
