//! SVG rendering of a parsed [`CadDatabase`].
//!
//! Entity coverage matches [`uncad_model::model::Entity`]'s variants; every other
//! type arrives as `Entity::Unknown`, whether or not it has geometry. Anything
//! this renderer cannot draw is reported through
//! [`ToSvgResult::unsupported_types`] rather than dropped silently. The one
//! type that will stay there for good is `ACAD_PROXY_ENTITY`: an opaque
//! per-app serialized blob with no geometry to draw at all.
//!
//! Several types render as deliberate approximations -- curves as chords, 3D
//! solids as isometric wireframes, VIEWPORT and WIPEOUT as outlines only. See
//! `docs/CAVEATS.md` for the full picture.
//!
//! Layout of this module: options and results, the block transform, the
//! rendering context, per-entity rendering, then [`to_svg`] itself.
//! Submodules hold the parts that stand on their own -- [`format`] (number and
//! string formatting), [`hatch`] (HATCH fills), [`spline`] (SPLINE curves),
//! [`infinite`] (RAY and XLINE, cut to the picture once the viewBox is
//! known) and [`bounds`] (viewBox and outlier trim).

mod bounds;
mod format;
mod hatch;
mod infinite;
mod spline;

use crate::color::{effective_layer, resolve_color, DEFAULT_COLOR};
use crate::limits::{
    Cap, LimitReport, MAX_BLOCK_REFS, MAX_BLOCK_REF_DEPTH, MAX_ENTITY_POINTS, MAX_ENTITY_SVG_BYTES,
    MAX_SVG_BODY_BYTES, MAX_WORLD_COORDINATE,
};
use crate::text::{decode_mtext, decode_text};
use bounds::{bbox_of, dominant_cluster_box, Box2D};
use format::{clean, escape_xml, neg, rotate_transform_attr, xy, Frame};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use uncad_model::model::{
    EllipseEntity, Entity, EntityCommon, EntityId, HatchBoundaryPath, HatchEdge, LightType,
    MLineVertex, MTextAttachment, Point2D, Point3D, Ref,
};
use uncad_model::tables::Tables;
use uncad_model::{Affine2, CadDatabase};

/// Which of a drawing's spaces to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Space {
    /// The drawing itself.
    Model,
    /// Sheet layouts: borders, title blocks.
    Paper,
    /// Everything, in one document.
    All,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToSvgOptions {
    pub padding: f64,
    /// `None` = auto-scaled to the computed viewBox (see [`to_svg`]).
    pub stroke_width: Option<f64>,
    pub space: Space,
    pub outlier_trim: bool,
    /// How tall a capital letter is in the face the text will be drawn with,
    /// as a fraction of its em (the font's OS/2 `sCapHeight` over its units
    /// per em). A CAD text height is the height of the capitals, so a text of
    /// height `h` is written at `font-size = h / cap_height`, and its
    /// capitals come out `h` tall in that face. Default
    /// [`DEFAULT_CAP_HEIGHT`]; a value that is not a positive number is
    /// taken as the default.
    pub cap_height: f64,
}

/// [`ToSvgOptions::cap_height`]'s default, 0.7: sans-serif faces measure
/// 0.70 to 0.73 (Segoe UI 0.700, Arial 0.716, Malgun Gothic 0.718, Verdana
/// 0.727, read from their OS/2 tables), a serif face less (Times New Roman,
/// usvg's default family, 0.662).
pub const DEFAULT_CAP_HEIGHT: f64 = 0.7;

impl Default for ToSvgOptions {
    fn default() -> Self {
        ToSvgOptions {
            padding: 5.0,
            stroke_width: None,
            space: Space::Model,
            outlier_trim: true,
            cap_height: DEFAULT_CAP_HEIGHT,
        }
    }
}

/// AutoCAD's single MTEXT line spacing: 5/3 of the text height from one
/// baseline to the next, multiplied by the MTEXT's line spacing factor.
const MTEXT_LINE_SPACING: f64 = 5.0 / 3.0;

pub struct ToSvgResult {
    pub svg: String,
    /// DXF names of entity types this renderer had nothing to draw for,
    /// sorted by name so the same input always reports them in the same order.
    pub unsupported_types: Vec<String>,
    /// Names of blocks that an INSERT referenced but that contributed nothing
    /// to the image -- the block definition is empty, or every entity in it
    /// was left out. Sorted, one entry per block. Without this, a drawing
    /// whose INSERTs all resolved to nothing looks the same as one whose
    /// INSERTs drew.
    pub empty_blocks: Vec<String>,
    /// The block references -- INSERT, ACAD_TABLE, DIMENSION -- that drew
    /// nothing because the model holds no block for them: the reference is
    /// [`Ref::Unresolved`] or [`Ref::Absent`], or it names a block
    /// `tables.block_records` does not have. By reference ID, sorted, each
    /// once; the entity's own `block_name` says which of the three it was.
    /// A block that exists but draws nothing is in
    /// [`empty_blocks`](Self::empty_blocks) instead.
    pub unresolved_block_refs: Vec<EntityId>,
    /// What the renderer's bounds on numbers from the file left out of the
    /// picture -- empty for every well-formed drawing. See
    /// [`crate::limits`].
    pub limits: LimitReport,
    /// The world point the SVG's coordinates are written relative to: an
    /// SVG user unit at `(u, v)` is the world point `(origin.x + u,
    /// origin.y - v)`, the viewBox included. `(0, 0)` -- the SVG reads in
    /// world units, y flipped -- unless the drawing lies more than 32768
    /// units from the world origin: the rasterizer keeps coordinates in
    /// `f32`, which at 2.5e8 cannot tell two points 16 units apart, so a
    /// far-away drawing is written about a whole-unit point near its own
    /// middle instead.
    pub origin: Point2D,
}

// --- block transform ---------------------------------------------------

/// The SVG `matrix(a b c d e f)` of a placement, composed with the
/// renderer's CAD-y-up to SVG-y-down flip on both sides: a point the child
/// writes as `child.x(p), child.y(p)` in its own frame lands where the
/// parent writes the placed point in its frame. Conjugating the linear part
/// by the flip negates the two off-diagonal entries; the translation is
/// wherever the placement puts the child frame's origin, written in the
/// parent's frame -- exactly zero when the child frame is the pullback of
/// the parent's, which [`render_block_ref`] chooses whenever a render origin
/// is in use.
fn svg_matrix(t: &Affine2, parent: Frame, child: Frame) -> [f64; 6] {
    let o = t.apply(Point2D {
        x: child.ox,
        y: child.oy,
    });
    [
        clean(t.a),
        neg(t.b),
        neg(t.c),
        clean(t.d),
        parent.x(o.x),
        parent.y(o.y),
    ]
}

/// The local point `t` sends to `p`: [`Affine2::apply`] run backwards.
/// `None` when the placement is singular (a zero scale flattens the block)
/// or the answer is not a usable number.
fn invert_point(t: &Affine2, p: Point2D) -> Option<Point2D> {
    let det = t.determinant();
    if !det.is_finite() || det == 0.0 {
        return None;
    }
    let (dx, dy) = (p.x - t.e, p.y - t.f);
    let local = Point2D {
        x: (t.d * dx - t.c * dy) / det,
        y: (t.a * dy - t.b * dx) / det,
    };
    (local.x.is_finite() && local.y.is_finite()).then_some(local)
}

// --- render context ----------------------------------------------------

struct Ctx<'a> {
    ent_min_x: f64,
    ent_max_x: f64,
    ent_min_y: f64,
    ent_max_y: f64,
    // Copied straight into the public result, in this set's (sorted) order.
    unsupported: BTreeSet<String>,
    /// Block names whose reference rendered to nothing (see
    /// `ToSvgResult::empty_blocks`); a set so a block referenced many times
    /// is reported once.
    empty_blocks: BTreeSet<String>,
    /// Block references whose block the model does not hold (see
    /// `ToSvgResult::unresolved_block_refs`).
    unresolved_block_refs: BTreeSet<EntityId>,
    tables: &'a Tables,
    depth: u32,
    scale: f64,
    inherited_color: String,
    /// The effective layer of the innermost enclosing block reference,
    /// `None` at the top level: what a child on layer 0 resolves its
    /// BYLAYER color against (see [`effective_layer`]). Already effective,
    /// so a layer-0 reference nested in a layer-0 reference ends at the
    /// outermost reference's layer.
    inherited_layer: Option<String>,
    /// Local (inside the block being rendered) -> world, composed across
    /// nested block references through the model's placement arithmetic.
    transform: Affine2,
    /// The origin the coordinates written now are relative to: the render's
    /// origin at the top level, and inside a block reference the local point
    /// its placement sends there (see [`render_block_ref`]). `transform`
    /// and the bounds stay in world units.
    frame: Frame,
    /// The enclosing `<g transform>` matrices composed: what takes a
    /// coordinate written now to the document's own. Identity at the top
    /// level. Only an element that cannot be finished until the viewBox is
    /// known needs it -- see [`infinite`].
    svg_matrix: infinite::Matrix,
    /// `<defs>` entries accumulated by HATCH rendering, emitted once into a
    /// top-level `<defs>` by [`to_svg`]. Persists across `render_block_ref`'s
    /// transform save/restore, since a HATCH can appear inside a block too.
    defs: Vec<String>,
    next_def_id: u32,
    /// Remaining budget for `render_block_ref` calls across the whole render
    /// pass, decremented once per call and never restored. The depth cap alone
    /// bounds nesting but not *breadth*: a crafted file with many INSERTs per
    /// block at every level can still fan out combinatorially before the depth
    /// cap is ever reached.
    block_ref_budget: u32,
    /// Bytes of drawing body emitted so far, kept equal to the length of the
    /// strings [`render_entity`] has handed back. Once it reaches
    /// [`MAX_SVG_BODY_BYTES`] nothing further is drawn.
    emitted: usize,
    /// [`emitted`](Self::emitted) when the current top-level entity started,
    /// so one part can be bounded by [`MAX_ENTITY_SVG_BYTES`] as well.
    entity_start: usize,
    /// Whether [`MAX_ENTITY_SVG_BYTES`] cut the current top-level entity's
    /// block expansion short. Reset for each top-level entity.
    part_truncated: bool,
    /// What the caps in [`crate::limits`] took away from this render.
    limits: LimitReport,
    /// [`ToSvgOptions::cap_height`], checked: what a text height is divided
    /// by to give the `font-size` it is written at.
    cap_height: f64,
}

impl<'a> Ctx<'a> {
    fn new(tables: &'a Tables) -> Self {
        Ctx {
            ent_min_x: f64::INFINITY,
            ent_max_x: f64::NEG_INFINITY,
            ent_min_y: f64::INFINITY,
            ent_max_y: f64::NEG_INFINITY,
            unsupported: BTreeSet::new(),
            empty_blocks: BTreeSet::new(),
            unresolved_block_refs: BTreeSet::new(),
            tables,
            depth: 0,
            scale: 1.0,
            inherited_color: DEFAULT_COLOR.to_string(),
            inherited_layer: None,
            transform: Affine2::IDENTITY,
            frame: Frame::default(),
            svg_matrix: infinite::IDENTITY,
            defs: Vec::new(),
            next_def_id: 0,
            block_ref_budget: MAX_BLOCK_REFS,
            emitted: 0,
            entity_start: 0,
            part_truncated: false,
            limits: LimitReport::default(),
            cap_height: DEFAULT_CAP_HEIGHT,
        }
    }

    fn reset_entity_bounds(&mut self) {
        self.ent_min_x = f64::INFINITY;
        self.ent_max_x = f64::NEG_INFINITY;
        self.ent_min_y = f64::INFINITY;
        self.ent_max_y = f64::NEG_INFINITY;
    }

    fn entity_box(&self) -> Option<Box2D> {
        self.ent_min_x.is_finite().then_some(Box2D {
            min_x: self.ent_min_x,
            max_x: self.ent_max_x,
            min_y: self.ent_min_y,
            max_y: self.ent_max_y,
        })
    }

    /// Records one *local* coordinate pair, applying the current (possibly
    /// block-nested) transform first.
    ///
    /// Non-finite results (from a malformed source file or a degenerate
    /// transform) are dropped rather than recorded: letting `Infinity` into the
    /// running bounds can pin both the min and the max to `Infinity` (the
    /// min-side update never fires because `Infinity < Infinity` is false),
    /// and the box's diagonal then computes as `NaN`, which panics the
    /// `partial_cmp(..).unwrap()` calls in [`bounds`] instead of just rendering
    /// a degenerate point.
    fn consider(&mut self, local_x: f64, local_y: f64) {
        let Point2D { x, y } = self.transform.apply(Point2D {
            x: local_x,
            y: local_y,
        });
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        if x < self.ent_min_x {
            self.ent_min_x = x;
        }
        if x > self.ent_max_x {
            self.ent_max_x = x;
        }
        if y < self.ent_min_y {
            self.ent_min_y = y;
        }
        if y > self.ent_max_y {
            self.ent_max_y = y;
        }
    }

    /// Records a local axis-aligned box through all four of its corners, so
    /// the world box still contains it under a rotated or sheared block
    /// placement: two diagonal corners bound it only for rotations by
    /// multiples of 90 degrees, and at 45 degrees they land on one vertical
    /// line.
    fn consider_box(&mut self, b: &Box2D) {
        self.consider(b.min_x, b.min_y);
        self.consider(b.max_x, b.min_y);
        self.consider(b.max_x, b.max_y);
        self.consider(b.min_x, b.max_y);
    }

    fn consider_all(&mut self, points: &[Point2D]) {
        for p in points {
            self.consider(p.x, p.y);
        }
    }

    fn consider_all_3d(&mut self, points: &[Point3D]) {
        for p in points {
            self.consider(p.x, p.y);
        }
    }

    /// A document-unique id for a `<defs>` entry, e.g. `"hp3"`.
    fn next_def_id(&mut self, prefix: &str) -> String {
        let id = format!("{prefix}{}", self.next_def_id);
        self.next_def_id += 1;
        id
    }
}

// --- element helpers ---------------------------------------------------

/// `stroke-width` cannot be resolved while rendering -- it is derived from the
/// final viewBox, which is only known once every entity has been walked. Block
/// references emit this placeholder carrying their cumulative scale instead,
/// and [`resolve_stroke_widths`] substitutes the real value in one pass at the
/// end.
fn stroke_width_placeholder(cumulative_scale: f64) -> String {
    format!("@@SW@@{cumulative_scale}@@")
}

/// Resolves every `@@SW@@<cumulative scale>@@` placeholder to
/// `effective_stroke_width / scale`, so a stroke keeps the same visual weight
/// however deeply nested or scaled its block reference is.
fn resolve_stroke_widths(body: &str, effective_stroke_width: f64) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    loop {
        let Some(start) = rest.find("@@SW@@") else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..start]);
        let after = &rest[start + "@@SW@@".len()..];
        let Some(end) = after.find("@@") else {
            // Malformed placeholder (shouldn't happen) -- emit verbatim.
            out.push_str(&rest[start..]);
            break;
        };
        let scale: f64 = after[..end].parse().unwrap_or(1.0);
        let _ = write!(out, "{}", effective_stroke_width / scale);
        rest = &after[end + "@@".len()..];
    }
    out
}

/// Points an ELLIPSE on a tilted plane is drawn through.
const ELLIPSE_SAMPLES: usize = 64;

/// How far an ELLIPSE runs, in its own parameter: `TAU` for a full ellipse,
/// otherwise the counter-clockwise span from `start` to `end` in (0, TAU).
/// DXF 41/42 are parameters of the ellipse, not polar angles, and the curve
/// always runs counter-clockwise from the first to the second.
fn ellipse_sweep(start: f64, end: f64) -> f64 {
    let span = end - start;
    if span.abs() < 1e-12 || (span.abs() - std::f64::consts::TAU).abs() < 1e-9 {
        return std::f64::consts::TAU;
    }
    let sweep = span.rem_euclid(std::f64::consts::TAU);
    if sweep < 1e-12 {
        std::f64::consts::TAU
    } else {
        sweep
    }
}

/// The point of an ELLIPSE at parameter `t`: `center + cos t * major + sin t
/// * minor`, the minor axis being the major one turned a quarter turn
/// counter-clockwise and scaled by `axis_ratio`.
fn ellipse_point(el: &EllipseEntity, t: f64) -> Point2D {
    let m = el.major_axis_endpoint;
    let n = ellipse_minor_axis(el);
    Point2D {
        x: el.center.x + t.cos() * m.x + t.sin() * n.x,
        y: el.center.y + t.cos() * m.y + t.sin() * n.y,
    }
}

/// The minor axis as a world vector: the unit normal crossed with the major
/// axis, scaled by `axis_ratio` -- the direction the parameters turn
/// towards. With the default normal (0, 0, 1) that is the major axis turned
/// a quarter turn counter-clockwise; a mirrored ellipse's (0, 0, -1) turns
/// it the other way.
fn ellipse_minor_axis(el: &EllipseEntity) -> Point3D {
    let (m, e) = (el.major_axis_endpoint, el.extrusion);
    let len = (e.x * e.x + e.y * e.y + e.z * e.z).sqrt();
    let (nx, ny, nz) = if len > 0.0 && len.is_finite() {
        (e.x / len, e.y / len, e.z / len)
    } else {
        (0.0, 0.0, 1.0)
    };
    Point3D {
        x: (ny * m.z - nz * m.y) * el.axis_ratio,
        y: (nz * m.x - nx * m.z) * el.axis_ratio,
        z: (nx * m.y - ny * m.x) * el.axis_ratio,
    }
}

/// A `<polyline>`, or a `<polygon>` when `closed` -- the shape every polyline
/// entity renders to.
fn polyline_element(pts: &[Point2D], closed: bool, color: &str, frame: Frame) -> String {
    let tag = if closed { "polygon" } else { "polyline" };
    format!(
        "<{tag} points=\"{}\" fill=\"none\" stroke=\"{color}\"/>",
        frame.points(pts)
    )
}

/// A dashed outline, used for the shapes this renderer draws as an indication
/// rather than as real geometry (VIEWPORT frames, WIPEOUT boundaries).
fn dashed_outline(pts: &[Point2D], color: &str, dash: &str, frame: Frame) -> String {
    format!(
        "<polygon points=\"{}\" fill=\"none\" stroke-dasharray=\"{dash}\" stroke=\"{color}\"/>",
        frame.points(pts)
    )
}

/// The height a text is drawn at: the stored one, or 1 when the file stores
/// 0 (which means "the style's height", a style this model does not carry)
/// or less. `font-size="0"` would make the text vanish without a trace.
fn effective_text_height(stored: f64) -> f64 {
    if stored.is_finite() && stored > 0.0 {
        stored
    } else {
        1.0
    }
}

/// A single-line `<text>` at `at` -- TEXT, ATTRIB and TOLERANCE all render
/// to this. `text` is what is shown, already decoded; `height` is the CAD
/// height, the height of the capitals, written as the `font-size` that
/// makes the capitals that tall in a face whose capitals are `cap_height`
/// of the em.
fn text_element(
    at: Point2D,
    height: f64,
    rotation: f64,
    color: &str,
    text: &str,
    frame: Frame,
    cap_height: f64,
) -> String {
    let font_size = effective_text_height(height) / cap_height;
    let (x, y) = (frame.x(at.x), frame.y(at.y));
    format!(
        "<text x=\"{x}\" y=\"{y}\" font-size=\"{font_size}\" fill=\"{color}\" stroke=\"none\"{}>{}</text>",
        rotate_transform_attr(rotation, x, y),
        escape_xml(text)
    )
}

/// Where an MTEXT block goes relative to its insertion point: the SVG
/// `text-anchor` and the y of the first line's baseline, in SVG coordinates
/// (y down; `y` is the insertion point's). The block is `text_height` for its
/// first line plus `line_height` for each further line; the attachment point
/// says which of its nine points the insertion point is. A file that does
/// not state the attachment keeps the insertion point as the first
/// baseline, left-anchored -- no placement is guessed.
fn mtext_placement(
    attachment: Option<MTextAttachment>,
    y: f64,
    text_height: f64,
    line_height: f64,
    lines: usize,
) -> (&'static str, f64) {
    use MTextAttachment as A;
    let Some(a) = attachment else {
        return ("start", y);
    };
    let anchor = match a {
        A::TopLeft | A::MiddleLeft | A::BottomLeft => "start",
        A::TopCenter | A::MiddleCenter | A::BottomCenter => "middle",
        A::TopRight | A::MiddleRight | A::BottomRight => "end",
    };
    let block = text_height + line_height * lines.saturating_sub(1) as f64;
    let top = match a {
        A::TopLeft | A::TopCenter | A::TopRight => y,
        A::MiddleLeft | A::MiddleCenter | A::MiddleRight => y - block / 2.0,
        A::BottomLeft | A::BottomCenter | A::BottomRight => y - block,
    };
    (anchor, top + text_height)
}

/// A small filled triangle at `tip`, pointing away from `from` -- LEADER and
/// MULTILEADER arrowheads.
fn arrowhead_element(
    tip: &Point2D,
    from: &Point2D,
    size: f64,
    color: &str,
    frame: Frame,
) -> String {
    let (dx, dy) = (tip.x - from.x, tip.y - from.y);
    let len = dx.hypot(dy);
    let len = if len == 0.0 { 1.0 } else { len };
    let (ux, uy) = (dx / len, dy / len);
    let (px, py) = (-uy, ux);
    let (back_x, back_y) = (tip.x - ux * size, tip.y - uy * size);
    let (p2x, p2y) = (back_x + px * size * 0.35, back_y + py * size * 0.35);
    let (p3x, p3y) = (back_x - px * size * 0.35, back_y - py * size * 0.35);
    format!(
        "<polygon points=\"{},{} {},{} {},{}\" fill=\"{color}\" stroke=\"none\"/>",
        frame.x(tip.x),
        frame.y(tip.y),
        frame.x(p2x),
        frame.y(p2y),
        frame.x(p3x),
        frame.y(p3y)
    )
}

/// Half the arm length of the cross a POINT draws, in stroke widths.
const POINT_CROSS_ARMS: f64 = 2.0;

/// The marker a POINT draws: a cross whose size is set in stroke widths, not
/// in drawing units.
///
/// A POINT has no size of its own, so whatever it is drawn at is a choice.
/// A half-unit dot is sub-pixel wherever a unit is under two pixels, so on
/// an ordinary plan the point antialiased away to nothing. The stroke width
/// is the one quantity here that is kept constant on the page, so the cross
/// is written at [`POINT_CROSS_ARMS`] local units and scaled by one stroke
/// width, through the same placeholder the strokes use: a POINT inside a
/// scaled block comes out the same size as one outside it. Only the point
/// itself counts towards the extent -- the cross is a page-sized
/// decoration, and the extent must not depend on the stroke.
fn point_cross_element(at: Point2D, color: &str, frame: Frame, scale: f64) -> String {
    let a = POINT_CROSS_ARMS;
    format!(
        "<path d=\"M -{a} 0 L {a} 0 M 0 -{a} L 0 {a}\" transform=\"translate({} {}) scale({})\" fill=\"none\" stroke=\"{color}\" stroke-width=\"1\"/>",
        frame.x(at.x),
        frame.y(at.y),
        stroke_width_placeholder(scale)
    )
}

/// The arrowhead size LEADER and MULTILEADER draw at. Not read from the file:
/// neither the geometry-only MULTILEADER shim nor LEADER's model carries an
/// arrow size, so one fixed value keeps the two consistent.
const ARROWHEAD_SIZE: f64 = 2.5;

/// Fixed engineering-isometric projection (30 degrees) for the wireframe
/// renderer. Not configurable -- a best-effort view, not a real camera.
fn project_isometric(p: &Point3D) -> (f64, f64) {
    let cos30 = (std::f64::consts::PI / 6.0).cos();
    let sin30 = (std::f64::consts::PI / 6.0).sin();
    ((p.x - p.z) * cos30, p.y + (p.x + p.z) * sin30)
}

/// One `<line>` per edge, isometrically projected -- shared by 3DSOLID, REGION
/// and POLYLINE_PFACE, which all reduce to a set of 3D edges.
fn wireframe_element(edges: &[[Point3D; 2]], color: &str, ctx: &mut Ctx) -> String {
    edges
        .iter()
        .map(|[a, b]| {
            let (x1, y1) = project_isometric(a);
            let (x2, y2) = project_isometric(b);
            ctx.consider(x1, y1);
            ctx.consider(x2, y2);
            let frame = ctx.frame;
            format!(
                "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"{color}\"/>",
                frame.x(x1),
                frame.y(y1),
                frame.x(x2),
                frame.y(y2)
            )
        })
        .collect::<Vec<_>>()
        .join("\n  ")
}

/// Projects an MLINE's centerline vertices onto the parallel line `offset`
/// away from it. `offset == 0.0` reproduces the centerline itself, which is
/// why the caller uses this for both the real-offset and the
/// no-resolvable-MLINESTYLE fallback case.
fn mline_offset_points(vertices: &[MLineVertex], offset: f64) -> Vec<Point2D> {
    vertices
        .iter()
        .map(|v| Point2D {
            x: v.point.x + v.miter_direction.x * offset,
            y: v.point.y + v.miter_direction.y * offset,
        })
        .collect()
}

fn resolve_entity_color(common: &EntityCommon, ctx: &Ctx) -> String {
    resolve_color(
        common.color_index,
        common.true_color,
        // An absent or unresolved layer has no layer color to look up; the
        // fallback below is the renderer's own, and the model still says
        // which of the two it was. Inside a block reference, layer 0 is the
        // reference's layer.
        effective_layer(common.layer.name(), ctx.inherited_layer.as_deref()),
        ctx.tables,
        &ctx.inherited_color,
    )
}

// --- entity rendering --------------------------------------------------

/// Looks `block_name` up in `ctx.tables.block_records` and recursively renders
/// its entities under a nested transform, wrapped in a
/// `<g transform="matrix(...)">`.
///
/// ATTDEF children are skipped: an attribute *template* is not drawn. A
/// top-level INSERT's attribute values are separate top-level ATTRIB
/// entities, drawn on their own; a *nested* INSERT's are drawn here, beside
/// it, from its own `attribs` -- once, even when the block also lists the
/// same ATTRIB among its children.
///
/// `owner` is the entity doing the referencing (INSERT, ACAD_TABLE,
/// DIMENSION) and `placement` the map it applies: a reference the caps in
/// [`crate::limits`] stop is reported under the owner's ID.
fn render_block_ref(
    owner: &Entity,
    block_name: &Ref<String>,
    placement: Affine2,
    color: &str,
    ctx: &mut Ctx,
) -> String {
    let Some((block_name, block)) = block_name.resolved().and_then(|name| {
        ctx.tables
            .block_records
            .get(name)
            .map(|block| (name.as_str(), block))
    }) else {
        // Nothing to draw, and it has to be said: the file points at a
        // block the model does not hold.
        ctx.unresolved_block_refs.insert(owner.common().id);
        return String::new();
    };
    if block.entities.is_empty() {
        ctx.empty_blocks.insert(block_name.to_string());
        return String::new();
    }
    // How deep references nest and how many there are both come from the
    // file, and a block that references itself makes both unbounded.
    if ctx.depth > MAX_BLOCK_REF_DEPTH || ctx.block_ref_budget == 0 {
        ctx.limits.block_refs_dropped += 1;
        ctx.limits
            .note(Cap::BlockRefs, owner.common().id, owner.type_name());
        return String::new();
    }
    ctx.block_ref_budget -= 1;

    let child_transform = placement;
    // Compose: local (within the block) -> world, via this block's own
    // placement followed by the parent's already-established one. The
    // parent's own state is restored afterwards.
    let parent_transform = ctx.transform;
    let parent_depth = ctx.depth;
    let parent_scale = ctx.scale;
    let parent_inherited = std::mem::replace(&mut ctx.inherited_color, color.to_string());
    let reference_layer =
        effective_layer(owner.common().layer.name(), ctx.inherited_layer.as_deref()).to_string();
    let parent_inherited_layer = ctx.inherited_layer.replace(reference_layer);

    // Which point of the block's own space its interior is written about.
    // For a drawing near the origin (the parent frame is the default) it is
    // (0, 0): the interior is written in the block's own coordinates and the
    // placement goes into the group's matrix. When a render origin is in use
    // that is wrong -- a DIMENSION's block is placed through an identity
    // precisely because its children already hold world coordinates, and
    // they would be written at full world magnitude, where the rasterizer's
    // f32 quantizes them away. So the interior is then written about the
    // point this placement sends to the parent frame's origin: every
    // number stays near zero, and the group's own translation is zero.
    let parent_frame = ctx.frame;
    let child_frame = if parent_frame == Frame::default() {
        Frame::default()
    } else {
        match invert_point(
            &child_transform,
            Point2D {
                x: parent_frame.ox,
                y: parent_frame.oy,
            },
        ) {
            Some(o) => Frame { ox: o.x, oy: o.y },
            // A singular placement flattens the block whatever the frame.
            None => Frame::default(),
        }
    };
    ctx.frame = child_frame;
    // The group this call emits, needed before the children are drawn: an
    // infinite line among them is cut in the document's frame and has to
    // know what gets it there.
    let group_matrix = svg_matrix(&child_transform, parent_frame, child_frame);
    let parent_svg_matrix = ctx.svg_matrix;
    ctx.svg_matrix = infinite::compose(parent_svg_matrix, group_matrix);
    ctx.transform = child_transform.then(&parent_transform);
    // How much this block's contents are scaled, for stroke widths: the
    // area scale of the composed placement. Derived from the placement
    // itself rather than tracked beside it, so the two cannot drift; for a
    // chain of placements this is the same number multiplying each level's
    // `sqrt(|sx * sy|)` gave. A degenerate placement (a zero or infinite
    // scale factor) keeps the parent's.
    let area_scale = ctx.transform.determinant().abs().sqrt();
    let cumulative_scale = if area_scale.is_finite() && area_scale > 0.0 {
        area_scale
    } else {
        parent_scale
    };
    ctx.depth = parent_depth + 1;
    ctx.scale = cumulative_scale;

    // An ATTRIB the block lists among its children is drawn by the loop
    // below as it is met; the same ATTRIB may also hang off its INSERT's
    // attribute list, and must not be drawn twice.
    let attrib_children: BTreeSet<EntityId> = block
        .entities
        .iter()
        .filter_map(|e| match e {
            Entity::Attrib(a) => Some(a.common.id),
            _ => None,
        })
        .collect();
    let mut body_parts = Vec::new();
    for child in &block.entities {
        if matches!(child, Entity::Attdef(_)) {
            continue;
        }
        if let Some(svg) = render_entity(child, ctx) {
            body_parts.push(svg);
        }
        // A nested INSERT's attribute values live on the INSERT, where
        // nothing else picks them up: only a top-level INSERT's reach the
        // model's top-level entities. Their coordinates are this block's,
        // like the INSERT's own insertion point, so they are drawn here and
        // not inside the reference.
        if let Entity::Insert(insert) = child {
            for a in &insert.attribs {
                if attrib_children.contains(&a.common.id) {
                    continue;
                }
                if let Some(svg) = render_entity(&Entity::Attrib(a.clone()), ctx) {
                    body_parts.push(svg);
                }
            }
        }
    }

    ctx.transform = parent_transform;
    ctx.frame = parent_frame;
    ctx.svg_matrix = parent_svg_matrix;
    ctx.depth = parent_depth;
    ctx.scale = parent_scale;
    ctx.inherited_color = parent_inherited;
    ctx.inherited_layer = parent_inherited_layer;

    if body_parts.is_empty() {
        ctx.empty_blocks.insert(block_name.to_string());
        return String::new();
    }

    // The parent transform is baked into ctx.transform for *bounds* purposes
    // (world-space consider()), but the emitted matrix is only this block's own
    // local transform -- nesting is expressed by nested <g> elements.
    let [a, b, c, d, e, f] = group_matrix;
    format!(
        "<g transform=\"matrix({a} {b} {c} {d} {e} {f})\" stroke-width=\"{}\">\n  {}\n</g>",
        stroke_width_placeholder(cumulative_scale),
        body_parts.join("\n  ")
    )
}

/// Renders one entity, or returns `None` if there is nothing to draw (with the
/// type recorded in `ctx.unsupported` when that is because the renderer has no
/// support for it). Bounds are extended through `ctx.consider*` as it goes.
///
/// All coordinates in the emitted SVG stay in *local* space -- the enclosing
/// `<g transform>` from [`render_block_ref`] does the visual repositioning,
/// while `consider` separately tracks world-space bounds through that same
/// transform.
fn render_entity(e: &Entity, ctx: &mut Ctx) -> Option<String> {
    // The file hides it: not drawn, and not part of the drawing's extent.
    if e.common().invisible {
        return None;
    }
    // The caps between a malformed file and an unbounded allocation (see
    // [`crate::limits`]), checked before any work is done for this entity,
    // so exhausting a budget unwinds the whole walk however deep inside
    // nested block references it happens.
    if ctx.emitted >= MAX_SVG_BODY_BYTES {
        ctx.limits.entities_dropped += 1;
        ctx.limits
            .note(Cap::DocumentBytes, e.common().id, e.type_name());
        return None;
    }
    if ctx.emitted - ctx.entity_start >= MAX_ENTITY_SVG_BYTES {
        // Inside a top-level entity that has already drawn as much as one
        // may. What it drew is kept and [`to_svg`] reports the part as
        // truncated; because the walk stops at an entity boundary, every
        // enclosing `<g>` still closes.
        ctx.part_truncated = true;
        return None;
    }
    if drawn_point_count(e, ctx.tables) > MAX_ENTITY_POINTS {
        ctx.limits.oversized_entities += 1;
        ctx.limits
            .note(Cap::EntityPoints, e.common().id, e.type_name());
        return None;
    }
    // A coordinate that is not a number names no place: nothing drawn from
    // it would be where the file meant, and `NaN`/`inf` are not in SVG's
    // `<number>` grammar at all. The entity is left out and counted.
    if !numbers_are_real(e) {
        ctx.limits.unreadable_entities += 1;
        ctx.limits
            .note(Cap::NotANumber, e.common().id, e.type_name());
        return None;
    }
    let before = ctx.emitted;
    let svg = draw_entity(e, ctx);
    // The string handed back contains everything the children below this
    // call already charged, so the total is set to its length rather than
    // incremented by it: nothing is counted twice.
    if let Some(svg) = &svg {
        ctx.emitted = before + svg.len();
    }
    svg
}

/// How many points from the file this entity would be drawn with -- the
/// count [`MAX_ENTITY_POINTS`] bounds. Only the arrays a file can make
/// arbitrarily long are counted: a fixed-shape entity is 0, and so is a
/// block reference, whose expansion the other caps bound.
fn drawn_point_count(e: &Entity, tables: &Tables) -> usize {
    match e {
        Entity::LwPolyline(p) | Entity::Polyline2D(p) => p.vertices.len(),
        Entity::Polyline3D(p) => p.vertices.len(),
        Entity::Spline(s) => s.fit_points.len().saturating_add(s.control_points.len()),
        Entity::Leader(l) => l.vertices.len(),
        Entity::MultiLeader(m) => m.lines.iter().map(Vec::len).sum(),
        Entity::MLine(l) => {
            // One polyline per offset the style defines.
            let lines = l
                .mlinestyle_name
                .resolved()
                .and_then(|n| tables.mlinestyles.get(n))
                .map_or(1, |offsets| offsets.len().max(1));
            l.vertices.len().saturating_mul(lines)
        }
        Entity::Wipeout(w) => w.boundary.len(),
        Entity::Solid3D(s) | Entity::Region(s) | Entity::PolylinePFace(s) => {
            s.wireframe_edges.len()
        }
        Entity::Hatch(h) => {
            let boundary: usize = h
                .boundary_paths
                .iter()
                .map(|path| match path {
                    HatchBoundaryPath::Polyline(v) => v.len(),
                    HatchBoundaryPath::Edges(edges) => edges
                        .iter()
                        .map(|edge| match edge {
                            HatchEdge::Line { .. } => 1,
                            HatchEdge::Arc { .. } => hatch::ARC_SEGMENTS,
                            HatchEdge::Ellipse { .. } => hatch::ELLIPSE_SEGMENTS,
                            HatchEdge::Spline { control_points } => control_points.len(),
                        })
                        .sum(),
                })
                .sum();
            // The boundary is written once for the outline and once more
            // for every pattern line that tiles it.
            let paths = if h.gradient.is_some() || h.solid_fill {
                1
            } else {
                1 + h.pattern_lines.len()
            };
            boundary.saturating_mul(paths)
        }
        _ => 0,
    }
}

/// Whether every number the renderer draws this entity from is a real
/// number (not `NaN`, not infinite). A block reference's own placement is
/// checked here; what its block holds is checked entity by entity as it is
/// drawn. Values the renderer does not draw from (a 3D point's `z` in plan
/// view, a spline's knots, which fall back to the control polygon when they
/// do not define a curve) are not screened.
fn numbers_are_real(e: &Entity) -> bool {
    fn real(values: &[f64]) -> bool {
        values.iter().all(|v| v.is_finite())
    }
    fn p2(p: &Point2D) -> bool {
        real(&[p.x, p.y])
    }
    fn p3(p: &Point3D) -> bool {
        real(&[p.x, p.y])
    }
    fn xyz(p: &Point3D) -> bool {
        real(&[p.x, p.y, p.z])
    }
    match e {
        Entity::Line(l) => p3(&l.start_point) && p3(&l.end_point),
        Entity::Circle(c) => p3(&c.center) && real(&[c.radius]),
        Entity::Arc(a) => {
            p3(&a.center)
                && real(&[a.radius])
                && is_sane_angle(a.start_angle)
                && is_sane_angle(a.end_angle)
        }
        Entity::Ellipse(el) => {
            p3(&el.center)
                && xyz(&el.major_axis_endpoint)
                && xyz(&el.extrusion)
                && real(&[el.axis_ratio])
                && is_sane_angle(el.start_angle)
                && is_sane_angle(el.end_angle)
        }
        Entity::LwPolyline(p) | Entity::Polyline2D(p) => p.vertices.iter().all(p2),
        Entity::Polyline3D(p) => p.vertices.iter().all(p3),
        Entity::Text(t) => p2(&t.start_point) && real(&[t.text_height, t.rotation]),
        Entity::Attrib(a) => p2(&a.start_point) && real(&[a.text_height, a.rotation]),
        Entity::Tolerance(t) => {
            p3(&t.insertion_point) && t.text_height.is_none_or(|h| h.is_finite())
        }
        Entity::MText(m) => {
            p3(&m.insertion_point) && real(&[m.text_height, m.rotation, m.line_spacing_factor])
        }
        Entity::Point(p) => p3(&p.position),
        Entity::Solid(s) | Entity::Trace(s) => {
            [s.corner1, s.corner2, s.corner3, s.corner4].iter().all(p2)
        }
        Entity::Face3D(f) => [f.corner1, f.corner2, f.corner3, f.corner4].iter().all(p3),
        Entity::Ray(r) | Entity::XLine(r) => p3(&r.point) && p3(&r.vector),
        Entity::Insert(i) => p3(&i.insertion_point) && real(&[i.scale.x, i.scale.y, i.rotation]),
        Entity::AcadTable(a) => p3(&a.insertion_point) && real(&[a.scale.x, a.scale.y, a.rotation]),
        Entity::Viewport(v) => p3(&v.center) && real(&[v.width, v.height]),
        Entity::Wipeout(w) => w.boundary.iter().all(p2),
        Entity::Spline(s) => s.fit_points.iter().all(p3) && s.control_points.iter().all(p3),
        Entity::Solid3D(s) | Entity::Region(s) | Entity::PolylinePFace(s) => {
            s.wireframe_edges.iter().all(|[a, b]| xyz(a) && xyz(b))
        }
        Entity::Hatch(h) => h.boundary_paths.iter().all(|path| match path {
            HatchBoundaryPath::Polyline(v) => v.iter().all(p2),
            HatchBoundaryPath::Edges(edges) => edges.iter().all(|edge| match edge {
                HatchEdge::Line { start } => p2(start),
                HatchEdge::Arc {
                    center,
                    radius,
                    start_angle,
                    end_angle,
                    ..
                } => p2(center) && real(&[*radius, *start_angle, *end_angle]),
                HatchEdge::Ellipse {
                    center,
                    end,
                    minor_major_ratio,
                    start_angle,
                    end_angle,
                    ..
                } => p2(center) && p2(end) && real(&[*minor_major_ratio, *start_angle, *end_angle]),
                HatchEdge::Spline { control_points } => control_points.iter().all(p2),
            }),
        }),
        Entity::Leader(l) => l.vertices.iter().all(p3),
        Entity::MultiLeader(m) => m.lines.iter().flatten().all(p3),
        Entity::MLine(l) => l
            .vertices
            .iter()
            .all(|v| p3(&v.point) && p3(&v.miter_direction)),
        Entity::Light(l) => p3(&l.position) && p3(&l.target),
        Entity::Dimension(_) | Entity::Attdef(_) | Entity::Unknown { .. } => true,
    }
}

/// The largest magnitude, in radians, a stored angle (or ellipse parameter)
/// is read at. A file stores an angle in `0..2 pi`; a corrupt one can hold
/// 1e20 or 1e247. Past a million radians an `f64` step is coarser than 1e-10
/// radians and the value names no direction any more, so what it would
/// describe is noise, not geometry.
const MAX_ANGLE: f64 = 1.0e6;

/// Whether `a` can be read as an angle at all: finite and within
/// [`MAX_ANGLE`].
fn is_sane_angle(a: f64) -> bool {
    a.is_finite() && a.abs() <= MAX_ANGLE
}

/// The extent of the circular arc about `center` of radius `r` from
/// `start` running `sweep` radians counter-clockwise (`0 <= sweep <= 2 pi`):
/// its two ends and every point in between where it crosses an axis
/// direction -- the arc's own box, not the whole circle's.
///
/// The crossings are at most four: a fifth would repeat the first one's
/// direction. That bound is by construction rather than by trusting the
/// angles, so a stored angle of any size cannot make this loop long.
fn arc_extent(center: Point2D, r: f64, start: f64, sweep: f64) -> Box2D {
    let mut b = Box2D {
        min_x: f64::INFINITY,
        max_x: f64::NEG_INFINITY,
        min_y: f64::INFINITY,
        max_y: f64::NEG_INFINITY,
    };
    let mut take = |angle: f64| {
        let (x, y) = (center.x + r * angle.cos(), center.y + r * angle.sin());
        b.min_x = b.min_x.min(x);
        b.max_x = b.max_x.max(x);
        b.min_y = b.min_y.min(y);
        b.max_y = b.max_y.max(y);
    };
    take(start);
    take(start + sweep);
    let quarter = std::f64::consts::FRAC_PI_2;
    let first = (start / quarter).ceil();
    for i in 0..4 {
        let angle = (first + f64::from(i)) * quarter;
        if angle.is_nan() || angle > start + sweep + 1e-12 {
            break;
        }
        take(angle);
    }
    b
}

/// The extent of the elliptical arc from parameter `start` running `sweep`
/// (`0 < sweep < 2 pi`), for an ellipse in a plane parallel to XY: its two
/// ends plus whichever of the four points where `dx/dt` or `dy/dt` vanishes
/// fall inside the sweep. `x(t) = cx + Mx cos t + nx sin t` is stationary
/// where `tan t = nx / Mx`, i.e. at `atan2(nx, Mx)` and half a turn later;
/// `y` likewise.
fn ellipse_arc_extent(el: &EllipseEntity, start: f64, sweep: f64) -> Box2D {
    let mut b = Box2D {
        min_x: f64::INFINITY,
        max_x: f64::NEG_INFINITY,
        min_y: f64::INFINITY,
        max_y: f64::NEG_INFINITY,
    };
    let mut take = |p: Point2D| {
        b.min_x = b.min_x.min(p.x);
        b.max_x = b.max_x.max(p.x);
        b.min_y = b.min_y.min(p.y);
        b.max_y = b.max_y.max(p.y);
    };
    take(ellipse_point(el, start));
    take(ellipse_point(el, start + sweep));
    let (m, n) = (el.major_axis_endpoint, ellipse_minor_axis(el));
    for base in [n.x.atan2(m.x), n.y.atan2(m.y)] {
        for half_turn in [0.0, std::f64::consts::PI] {
            let offset = (base + half_turn - start).rem_euclid(std::f64::consts::TAU);
            if offset <= sweep + 1e-12 {
                take(ellipse_point(el, start + offset));
            }
        }
    }
    b
}

/// [`render_entity`] once the caps have let the entity through.
fn draw_entity(e: &Entity, ctx: &mut Ctx) -> Option<String> {
    let color = resolve_entity_color(e.common(), ctx);
    let frame = ctx.frame;
    match e {
        Entity::Line(l) => {
            ctx.consider(l.start_point.x, l.start_point.y);
            ctx.consider(l.end_point.x, l.end_point.y);
            Some(format!(
                "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"{color}\"/>",
                frame.x(l.start_point.x),
                frame.y(l.start_point.y),
                frame.x(l.end_point.x),
                frame.y(l.end_point.y)
            ))
        }
        Entity::Circle(c) => {
            ctx.consider_box(&Box2D {
                min_x: c.center.x - c.radius,
                max_x: c.center.x + c.radius,
                min_y: c.center.y - c.radius,
                max_y: c.center.y + c.radius,
            });
            Some(format!(
                "<circle cx=\"{}\" cy=\"{}\" r=\"{}\" fill=\"none\" stroke=\"{color}\"/>",
                frame.x(c.center.x),
                frame.y(c.center.y),
                clean(c.radius)
            ))
        }
        Entity::Arc(a) => {
            let (x, y, r) = (a.center.x, a.center.y, a.radius);
            let (x1, y1) = (x + r * a.start_angle.cos(), y + r * a.start_angle.sin());
            let (x2, y2) = (x + r * a.end_angle.cos(), y + r * a.end_angle.sin());
            let mut sweep = a.end_angle - a.start_angle;
            if sweep < 0.0 {
                sweep += 2.0 * std::f64::consts::PI;
            }
            // The arc's own extent, not the whole circle's: a large-radius
            // fillet must not stretch the picture to its centre.
            ctx.consider_box(&arc_extent(Point2D { x, y }, r, a.start_angle, sweep));
            let large = if sweep > std::f64::consts::PI { 1 } else { 0 };
            let r = clean(r);
            Some(format!(
                "<path d=\"M {} {} A {r} {r} 0 {large} 0 {} {}\" fill=\"none\" stroke=\"{color}\"/>",
                frame.x(x1),
                frame.y(y1),
                frame.x(x2),
                frame.y(y2)
            ))
        }
        Entity::Ellipse(el) => {
            let rx = el.major_axis_endpoint.x.hypot(el.major_axis_endpoint.y);
            let ry = rx * el.axis_ratio;
            let theta = el.major_axis_endpoint.y.atan2(el.major_axis_endpoint.x);
            let rot = theta.to_degrees();
            let (cx, cy) = (frame.x(el.center.x), frame.y(el.center.y));
            let sweep = ellipse_sweep(el.start_angle, el.end_angle);
            if sweep >= std::f64::consts::TAU {
                // The box of the ellipse this element draws: semi-axes rx
                // and ry turned by the major axis's angle.
                let (cos, sin) = (theta.cos(), theta.sin());
                let hx = (rx * cos).hypot(ry * sin);
                let hy = (rx * sin).hypot(ry * cos);
                ctx.consider_box(&Box2D {
                    min_x: el.center.x - hx,
                    max_x: el.center.x + hx,
                    min_y: el.center.y - hy,
                    max_y: el.center.y + hy,
                });
                let (rx, ry) = (clean(rx), clean(ry.abs()));
                return Some(format!(
                    "<ellipse cx=\"{cx}\" cy=\"{cy}\" rx=\"{rx}\" ry=\"{ry}\" transform=\"rotate({} {cx} {cy})\" fill=\"none\" stroke=\"{color}\"/>",
                    neg(rot)
                ));
            }
            let e = el.extrusion;
            // Files write the Z axis with rounding noise in the other two
            // components; a plane that far from level is still level.
            let flat = e.x.abs().max(e.y.abs()) <= 1e-9 * e.z.abs();
            if !flat {
                // A tilted plane: its outline seen from above is not an
                // ellipse with these axes, so it is drawn through points.
                let points: Vec<Point2D> = (0..=ELLIPSE_SAMPLES)
                    .map(|i| {
                        ellipse_point(
                            el,
                            el.start_angle + sweep * (i as f64 / ELLIPSE_SAMPLES as f64),
                        )
                    })
                    .collect();
                ctx.consider_all(&points);
                return Some(polyline_element(&points, false, &color, frame));
            }
            // The arc's own extent, not the whole ellipse's.
            ctx.consider_box(&ellipse_arc_extent(el, el.start_angle, sweep));
            let (rx, ry) = (clean(rx), clean(ry.abs()));
            // A partial ellipse: the same exact arc command ARC uses, with
            // the axes and rotation of the ellipse. Counter-clockwise in the
            // drawing is clockwise once y is flipped, hence sweep-flag 0 --
            // and 1 for a mirrored ellipse, whose parameters run clockwise.
            let p1 = ellipse_point(el, el.start_angle);
            let p2 = ellipse_point(el, el.start_angle + sweep);
            let large = if sweep > std::f64::consts::PI { 1 } else { 0 };
            let sweep_flag = if e.z < 0.0 { 1 } else { 0 };
            Some(format!(
                "<path d=\"M {} {} A {rx} {ry} {} {large} {sweep_flag} {} {}\" fill=\"none\" stroke=\"{color}\"/>",
                frame.x(p1.x),
                frame.y(p1.y),
                neg(rot),
                frame.x(p2.x),
                frame.y(p2.y)
            ))
        }
        Entity::LwPolyline(p) | Entity::Polyline2D(p) => {
            ctx.consider_all(&p.vertices);
            Some(polyline_element(&p.vertices, p.closed, &color, frame))
        }
        Entity::Polyline3D(p) => {
            if p.vertices.is_empty() {
                return None;
            }
            ctx.consider_all_3d(&p.vertices);
            Some(polyline_element(&xy(&p.vertices), p.closed, &color, frame))
        }
        Entity::Text(t) => {
            ctx.consider(t.start_point.x, t.start_point.y);
            Some(text_element(
                t.start_point,
                t.text_height,
                t.rotation,
                &color,
                &decode_text(&t.text),
                frame,
                ctx.cap_height,
            ))
        }
        Entity::Attrib(a) => {
            ctx.consider(a.start_point.x, a.start_point.y);
            if a.text.is_empty() {
                return Some(String::new());
            }
            Some(text_element(
                a.start_point,
                a.text_height,
                a.rotation,
                &color,
                &decode_text(&a.text),
                frame,
                ctx.cap_height,
            ))
        }
        Entity::Tolerance(t) => {
            ctx.consider(t.insertion_point.x, t.insertion_point.y);
            if t.text_value.is_empty() {
                return Some(String::new());
            }
            Some(text_element(
                Point2D {
                    x: t.insertion_point.x,
                    y: t.insertion_point.y,
                },
                // A frame whose file never stated a height still has to be
                // drawn at some size; this is the renderer's choice, which
                // is why the model does not make it.
                t.text_height.unwrap_or(1.0),
                0.0,
                &color,
                &t.text_value,
                frame,
                ctx.cap_height,
            ))
        }
        Entity::MText(m) => {
            ctx.consider(m.insertion_point.x, m.insertion_point.y);
            let decoded = decode_mtext(&m.text);
            // An empty line is a real line: `\P\P` is how a note spaces its
            // paragraphs, and it takes up its line height.
            let lines: Vec<&str> = decoded
                .split('\n')
                .map(|l| l.strip_suffix('\r').unwrap_or(l))
                .collect();
            if lines.iter().all(|l| l.is_empty()) {
                return Some(String::new());
            }
            // A stored 0 means "unset" at render time (the parsed value is
            // legitimately 0 in real files), not at parse time.
            let text_height = effective_text_height(m.text_height);
            let line_spacing_factor = if m.line_spacing_factor == 0.0 {
                1.0
            } else {
                m.line_spacing_factor
            };
            let line_height = text_height * line_spacing_factor * MTEXT_LINE_SPACING;
            let font_size = text_height / ctx.cap_height;
            let (x, y) = (frame.x(m.insertion_point.x), frame.y(m.insertion_point.y));
            let (anchor, first_baseline) =
                mtext_placement(m.attachment, y, text_height, line_height, lines.len());
            let mut tspans = String::new();
            // An empty line has no characters, so no `<tspan>` of its own:
            // SVG applies a `dy` to the characters that follow it, and an
            // empty element has none, so the shift would be lost. Its line
            // height goes into the next drawn line's `dy` instead.
            let mut dy = 0.0;
            for (i, line) in lines.iter().enumerate() {
                if i > 0 {
                    dy += line_height;
                }
                if line.is_empty() {
                    continue;
                }
                let _ = write!(
                    tspans,
                    "<tspan x=\"{x}\" dy=\"{}\">{}</tspan>",
                    clean(dy),
                    escape_xml(line)
                );
                dy = 0.0;
            }
            Some(format!(
                "<text x=\"{x}\" y=\"{first_baseline}\" font-size=\"{font_size}\" text-anchor=\"{anchor}\" fill=\"{color}\" stroke=\"none\" transform=\"rotate({} {x} {y})\">{tspans}</text>",
                neg(m.rotation.to_degrees())
            ))
        }
        Entity::Point(p) => {
            ctx.consider(p.position.x, p.position.y);
            Some(point_cross_element(
                Point2D {
                    x: p.position.x,
                    y: p.position.y,
                },
                &color,
                frame,
                ctx.scale,
            ))
        }
        Entity::Solid(s) | Entity::Trace(s) => {
            // Classic AutoCAD SOLID/TRACE vertex order is 1-2-4-3, not 1-2-3-4.
            let pts = [s.corner1, s.corner2, s.corner4, s.corner3];
            ctx.consider_all(&pts);
            Some(format!(
                "<polygon points=\"{}\" fill=\"{color}\" fill-opacity=\"0.6\" stroke=\"none\"/>",
                frame.points(&pts)
            ))
        }
        Entity::Face3D(f) => {
            // Unlike SOLID, 3DFACE's 4 corners are already sequential.
            // Edge-visibility flag bits are ignored; all 4 edges always draw.
            let pts = xy(&[f.corner1, f.corner2, f.corner3, f.corner4]);
            ctx.consider_all(&pts);
            Some(format!(
                "<polygon points=\"{}\" fill=\"none\" stroke=\"{color}\"/>",
                frame.points(&pts)
            ))
        }
        Entity::Ray(r) | Entity::XLine(r) => {
            // A construction line has no end, so only its base point counts
            // towards the extent: a viewBox that had to contain the line
            // would show nothing else. Where the line stops is the edge of
            // the picture, known only once every entity has been walked, so
            // the element is a placeholder cut to the viewBox at the end
            // (see [`infinite`]).
            ctx.consider(r.point.x, r.point.y);
            // The direction in the element's own frame: y flipped, like
            // every coordinate written here.
            let (dx, dy) = (r.vector.x, -r.vector.y);
            let len = dx.hypot(dy);
            if !(len.is_finite() && len > 0.0) {
                // No direction in plan: nothing to draw.
                return None;
            }
            Some(infinite::placeholder(
                &infinite::InfiniteLine {
                    matrix: ctx.svg_matrix,
                    base: (frame.x(r.point.x), frame.y(r.point.y)),
                    dir: (dx / len, dy / len),
                    both_ways: matches!(e, Entity::XLine(_)),
                },
                &color,
            ))
        }
        Entity::Insert(i) => Some(render_block_ref(
            e,
            &i.block_name,
            Affine2::from_insert(i),
            &color,
            ctx,
        )),
        Entity::AcadTable(a) => Some(render_block_ref(
            e,
            &a.block_name,
            Affine2::placement(
                Point2D {
                    x: a.insertion_point.x,
                    y: a.insertion_point.y,
                },
                a.scale.x,
                a.scale.y,
                a.rotation,
            ),
            &color,
            ctx,
        )),
        Entity::Dimension(d) => {
            // The cached geometry block is already in final world coordinates,
            // so it is drawn with an identity transform -- which is also what
            // keeps its interior in the render's own frame (see
            // [`render_block_ref`]).
            let svg = render_block_ref(e, &d.block_name, Affine2::IDENTITY, &color, ctx);
            if svg.is_empty() {
                ctx.unsupported.insert("DIMENSION".to_string());
                return None;
            }
            Some(svg)
        }
        Entity::Viewport(v) => {
            // Not real drawing geometry (it is a window onto model space), but
            // drawing its frame is a reasonable, honest representation.
            let (cx, cy) = (v.center.x, v.center.y);
            let (hw, hh) = (v.width / 2.0, v.height / 2.0);
            let corners = [
                Point2D {
                    x: cx - hw,
                    y: cy - hh,
                },
                Point2D {
                    x: cx + hw,
                    y: cy - hh,
                },
                Point2D {
                    x: cx + hw,
                    y: cy + hh,
                },
                Point2D {
                    x: cx - hw,
                    y: cy + hh,
                },
            ];
            ctx.consider_all(&corners);
            Some(dashed_outline(&corners, &color, "2,2", frame))
        }
        Entity::Wipeout(w) => {
            // Outline only, not filled: a filled shape would mask whatever is
            // drawn under it. That is arguably WIPEOUT's real effect, but this
            // renderer's simple in-order painting cannot be trusted to
            // reproduce it, and an unexpectedly opaque box is a worse failure
            // than a merely incomplete outline.
            if w.boundary.len() < 2 {
                ctx.unsupported.insert("WIPEOUT".to_string());
                return None;
            }
            ctx.consider_all(&w.boundary);
            Some(dashed_outline(&w.boundary, &color, "2,2", frame))
        }
        Entity::Spline(s) => {
            let pts = spline::spline_points(s);
            if pts.len() < 2 {
                return None;
            }
            ctx.consider_all(&pts);
            Some(polyline_element(&pts, false, &color, frame))
        }
        Entity::Solid3D(s) => render_wireframe_entity(&s.wireframe_edges, "3DSOLID", &color, ctx),
        Entity::Region(r) => render_wireframe_entity(&r.wireframe_edges, "REGION", &color, ctx),
        Entity::PolylinePFace(p) => {
            render_wireframe_entity(&p.wireframe_edges, "POLYLINE_PFACE", &color, ctx)
        }
        Entity::Hatch(h) => hatch::render_hatch(h, h.common.id, &color, ctx),
        Entity::Leader(l) => {
            if l.vertices.is_empty() {
                return None;
            }
            ctx.consider_all_3d(&l.vertices);
            let pts = xy(&l.vertices);
            let line = polyline_element(&pts, false, &color, frame);
            // A leader whose file does not state the flag draws none: an
            // arrowhead is a claim about the drawing, and nothing made it.
            let arrow = if l.has_arrowhead == Some(true) && pts.len() >= 2 {
                arrowhead_element(&pts[0], &pts[1], ARROWHEAD_SIZE, &color, frame)
            } else {
                String::new()
            };
            Some(line + &arrow)
        }
        Entity::MultiLeader(m) => {
            // An arrowhead is drawn on every line, unconditionally: the real
            // per-line visibility flag lives in LEADER_Line.flags, which the
            // geometry-only shim this reads from does not extract.
            let mut parts = Vec::new();
            for line in &m.lines {
                if line.len() < 2 {
                    continue;
                }
                ctx.consider_all_3d(line);
                let pts = xy(line);
                parts.push(polyline_element(&pts, false, &color, frame));
                let n = pts.len();
                parts.push(arrowhead_element(
                    &pts[n - 1],
                    &pts[n - 2],
                    ARROWHEAD_SIZE,
                    &color,
                    frame,
                ));
            }
            (!parts.is_empty()).then(|| parts.join("\n  "))
        }
        Entity::MLine(l) => {
            if l.vertices.is_empty() {
                return None;
            }
            for v in &l.vertices {
                ctx.consider(v.point.x, v.point.y);
            }
            // A single 0.0 offset is exactly the centerline-only fallback:
            // point + miter_direction * 0.0 is just point.
            let offsets = match l
                .mlinestyle_name
                .resolved()
                .and_then(|n| ctx.tables.mlinestyles.get(n))
            {
                Some(offsets) if !offsets.is_empty() => offsets.clone(),
                _ => vec![0.0],
            };
            let lines: Vec<String> = offsets
                .iter()
                .map(|&offset| {
                    polyline_element(
                        &mline_offset_points(&l.vertices, offset),
                        l.closed,
                        &color,
                        frame,
                    )
                })
                .collect();
            Some(lines.join("\n  "))
        }
        Entity::Light(l) => {
            ctx.consider(l.position.x, l.position.y);
            let marker = format!(
                "<circle cx=\"{}\" cy=\"{}\" r=\"0.5\" fill=\"none\" stroke=\"{color}\"/>",
                frame.x(l.position.x),
                frame.y(l.position.y)
            );
            // Distant and spot lights aim at their target; a point light,
            // or a light whose file does not say what kind it is, gets the
            // marker alone rather than a direction nobody stated.
            let aims = matches!(l.light_type, Some(LightType::Distant | LightType::Spot))
                && l.target != l.position;
            if !aims {
                return Some(marker);
            }
            ctx.consider(l.target.x, l.target.y);
            let line = format!(
                "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke-dasharray=\"1,1\" stroke=\"{color}\"/>",
                frame.x(l.position.x),
                frame.y(l.position.y),
                frame.x(l.target.x),
                frame.y(l.target.y)
            );
            Some(format!("{marker}\n  {line}"))
        }
        // Reaching here means an ATTDEF outside a block reference -- a
        // template with no instance, so there is nothing to draw.
        Entity::Attdef(_) => {
            ctx.unsupported.insert("ATTDEF".to_string());
            None
        }
        Entity::Unknown { type_name, .. } => {
            ctx.unsupported.insert(type_name.clone());
            None
        }
    }
}

/// Whether a measured world box lies within [`MAX_WORLD_COORDINATE`] of the
/// origin on both axes -- whether a viewBox can be built from it.
fn within_world(b: &Box2D) -> bool {
    [b.min_x, b.max_x, b.min_y, b.max_y]
        .iter()
        .all(|v| v.abs() < MAX_WORLD_COORDINATE)
}

/// Wireframe entities share one shape: draw the edges, or report the type as
/// unsupported when extraction produced none.
fn render_wireframe_entity(
    edges: &[[Point3D; 2]],
    type_name: &str,
    color: &str,
    ctx: &mut Ctx,
) -> Option<String> {
    if edges.is_empty() {
        ctx.unsupported.insert(type_name.to_string());
        return None;
    }
    Some(wireframe_element(edges, color, ctx))
}

/// `db.entities` only ever contains model and paper space (see `convert.rs`),
/// so [`Space::All`] returns everything; the other two narrow by block name.
fn select_entities_for_space(db: &CadDatabase, space: Space) -> Vec<&Entity> {
    if space == Space::All {
        return db.entities.iter().collect();
    }
    let mut ids: BTreeSet<EntityId> = BTreeSet::new();
    for (name, record) in &db.tables.block_records {
        let upper = name.to_uppercase();
        let matches = match space {
            Space::Model => upper == "*MODEL_SPACE",
            Space::Paper => upper.starts_with("*PAPER_SPACE"),
            Space::All => unreachable!(),
        };
        if !matches {
            continue;
        }
        for e in &record.entities {
            ids.insert(e.common().id);
            if let Entity::Insert(insert) = e {
                for a in &insert.attribs {
                    ids.insert(a.common.id);
                }
            }
        }
    }
    db.entities
        .iter()
        .filter(|e| ids.contains(&e.common().id))
        .collect()
}

/// Coordinates this far from the world origin (in drawing units) get the
/// render written about a local origin instead: below it an `f32` still
/// resolves better than 1/250 of a unit, far finer than any stroke, so a
/// drawing near the origin keeps its world-unit SVG byte for byte.
const ORIGIN_SHIFT_THRESHOLD: f64 = 32768.0;

/// One cheap reference point per entity -- an end, a centre, an insertion
/// point -- for [`choose_origin`]'s median.
fn reference_point(e: &Entity) -> Option<Point2D> {
    let p3 = |p: &Point3D| Point2D { x: p.x, y: p.y };
    Some(match e {
        Entity::Line(l) => p3(&l.start_point),
        Entity::Circle(c) => p3(&c.center),
        Entity::Arc(a) => p3(&a.center),
        Entity::Ellipse(el) => p3(&el.center),
        Entity::LwPolyline(p) | Entity::Polyline2D(p) => *p.vertices.first()?,
        Entity::Polyline3D(p) => p3(p.vertices.first()?),
        Entity::Text(t) => t.start_point,
        Entity::Attrib(a) => a.start_point,
        Entity::Attdef(a) => a.start_point,
        Entity::Tolerance(t) => p3(&t.insertion_point),
        Entity::MText(m) => p3(&m.insertion_point),
        Entity::Point(p) => p3(&p.position),
        Entity::Solid(s) | Entity::Trace(s) => s.corner1,
        Entity::Face3D(f) => p3(&f.corner1),
        Entity::Ray(r) | Entity::XLine(r) => p3(&r.point),
        Entity::Insert(i) => p3(&i.insertion_point),
        Entity::AcadTable(a) => p3(&a.insertion_point),
        Entity::Dimension(d) => d.text_midpoint,
        Entity::Viewport(v) => p3(&v.center),
        Entity::Wipeout(w) => *w.boundary.first()?,
        Entity::Spline(s) => p3(s.fit_points.first().or(s.control_points.first())?),
        Entity::Solid3D(s) | Entity::Region(s) | Entity::PolylinePFace(s) => {
            p3(&s.wireframe_edges.first()?[0])
        }
        Entity::Hatch(h) => match h.boundary_paths.first()? {
            HatchBoundaryPath::Polyline(v) => *v.first()?,
            HatchBoundaryPath::Edges(edges) => match edges.first()? {
                HatchEdge::Line { start } => *start,
                HatchEdge::Arc { center, .. } | HatchEdge::Ellipse { center, .. } => *center,
                HatchEdge::Spline { control_points } => *control_points.first()?,
            },
        },
        Entity::Leader(l) => p3(l.vertices.first()?),
        Entity::MultiLeader(m) => p3(m.lines.first()?.first()?),
        Entity::MLine(l) => p3(&l.vertices.first()?.point),
        Entity::Light(l) => p3(&l.position),
        Entity::Unknown { .. } => return None,
    })
}

/// The origin a render of `selected` is written relative to: the per-axis
/// median of the entities' reference points, rounded to whole units, when it
/// lies more than [`ORIGIN_SHIFT_THRESHOLD`] from the world origin on either
/// axis; `(0, 0)` otherwise. The median rather than the extent's corner, so
/// the choice is settled before anything is drawn and one far-away outlier
/// does not move it.
fn choose_origin(selected: &[&Entity]) -> Point2D {
    let mut xs: Vec<f64> = Vec::with_capacity(selected.len());
    let mut ys: Vec<f64> = Vec::with_capacity(selected.len());
    for p in selected.iter().filter_map(|e| reference_point(e)) {
        if p.x.is_finite() && p.y.is_finite() {
            xs.push(p.x);
            ys.push(p.y);
        }
    }
    let origin = Point2D { x: 0.0, y: 0.0 };
    if xs.is_empty() {
        return origin;
    }
    let median = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.total_cmp(b));
        v[v.len() / 2].round()
    };
    let (mx, my) = (median(&mut xs), median(&mut ys));
    if mx.abs().max(my.abs()) > ORIGIN_SHIFT_THRESHOLD {
        Point2D { x: mx, y: my }
    } else {
        origin
    }
}

// --- top level ---------------------------------------------------------

/// Renders a parsed [`CadDatabase`] to an SVG string.
///
/// `outlier_trim` (default `true`) computes the viewBox from the dominant
/// spatially-connected cluster of entities instead of the raw min/max -- see
/// [`bounds`] for why.
///
/// `stroke_width` defaults to ~1/6000th of the computed viewBox diagonal
/// rather than a fixed value; see [`stroke_width_placeholder`] for how nested
/// block references keep a constant visual weight.
pub fn to_svg(db: &CadDatabase, options: ToSvgOptions) -> ToSvgResult {
    let mut entity_boxes: Vec<Box2D> = Vec::new();
    let mut body: Vec<String> = Vec::new();

    let selected = select_entities_for_space(db, options.space);
    let origin = choose_origin(&selected);
    let mut ctx = Ctx::new(&db.tables);
    ctx.frame = Frame {
        ox: origin.x,
        oy: origin.y,
    };
    if options.cap_height.is_finite() && options.cap_height > 0.0 {
        ctx.cap_height = options.cap_height;
    }
    for e in selected {
        ctx.reset_entity_bounds();
        ctx.entity_start = ctx.emitted;
        ctx.part_truncated = false;
        let svg = render_entity(e, &mut ctx);
        let entity_box = ctx.entity_box();
        if entity_box.is_some_and(|b| !within_world(&b)) {
            // Past what a viewBox -- and the stroke width, padding and dash
            // lengths derived from it -- can be built from.
            ctx.limits.out_of_range_entities += 1;
            ctx.limits
                .note(Cap::OutOfRange, e.common().id, e.type_name());
            continue;
        }
        if let Some(svg) = svg {
            if ctx.part_truncated {
                // What the entity drew before its budget ran out is kept;
                // the report says the part is incomplete.
                ctx.limits.truncated_parts += 1;
                ctx.limits
                    .note(Cap::EntityBytes, e.common().id, e.type_name());
            }
            if !svg.is_empty() {
                body.push(svg);
            }
        }
        if let Some(b) = entity_box {
            entity_boxes.push(b);
        }
    }

    // Every point measured belongs to exactly one top-level entity's box,
    // so the boxes' own extent is the extent of every point.
    let raw_bounds = || bbox_of(&entity_boxes);
    let bounds = if entity_boxes.is_empty() {
        Box2D {
            min_x: 0.0,
            max_x: 0.0,
            min_y: 0.0,
            max_y: 0.0,
        }
    } else if options.outlier_trim && entity_boxes.len() > 2 {
        dominant_cluster_box(&entity_boxes).unwrap_or_else(raw_bounds)
    } else {
        raw_bounds()
    };

    // The viewBox in world units, then written in the render's frame like
    // every coordinate inside it.
    let x = bounds.min_x - options.padding - origin.x;
    let y = -bounds.max_y - options.padding + origin.y;
    let width = (bounds.max_x - bounds.min_x) + options.padding * 2.0;
    let height = (bounds.max_y - bounds.min_y) + options.padding * 2.0;
    let effective_stroke_width = options
        .stroke_width
        .unwrap_or_else(|| (width.hypot(height) / 6000.0).max(0.01));
    // A degenerate (zero-size) extent still gets a 1 x 1 canvas.
    let width = if width != 0.0 { width } else { 1.0 };
    let height = if height != 0.0 { height } else { 1.0 };

    // Construction lines are cut to the picture now that it is known.
    let resolved_body = infinite::resolve(
        resolve_stroke_widths(&body.join("\n  "), effective_stroke_width),
        infinite::window(x, y, width, height, effective_stroke_width),
    );
    // HATCH pattern defs carry stroke-width placeholders too. Kept separate
    // from the body only so an empty defs list emits no <defs> block at all.
    let defs_block = if ctx.defs.is_empty() {
        String::new()
    } else {
        let resolved_defs = resolve_stroke_widths(&ctx.defs.join("\n  "), effective_stroke_width);
        format!("<defs>\n  {resolved_defs}\n</defs>\n  ")
    };

    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{x} {y} {width} {height}\" stroke=\"black\" stroke-width=\"{effective_stroke_width}\">\n  {defs_block}{resolved_body}\n</svg>"
    );

    ToSvgResult {
        svg,
        unsupported_types: ctx.unsupported.into_iter().collect(),
        empty_blocks: ctx.empty_blocks.into_iter().collect(),
        unresolved_block_refs: ctx.unresolved_block_refs.into_iter().collect(),
        limits: ctx.limits,
        origin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bounds::diag;

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "expected {a} ~= {b}");
    }

    #[test]
    fn render_block_ref_budget_caps_combinatorial_blowup_from_self_referencing_blocks() {
        use std::collections::BTreeMap;
        use uncad_model::model::{InsertEntity, Ref};
        use uncad_model::tables::BlockRecord;

        // Block "R" contains 5 INSERTs of itself. The depth cap alone bounds
        // recursion depth, but a full 5-ary tree 20 levels deep is 5^20
        // (~9.5e13) block instantiations, which would never finish.
        let common = EntityCommon {
            id: uncad_model::model::EntityId::new(1),
            origin: uncad_model::model::Origin::Vector,
            confidence: uncad_model::model::Confidence::High,
            source_handle: Ref::Absent,
            layer: Ref::Absent,
            color_index: 0,
            true_color: None,
            invisible: false,
        };
        let children: Vec<Entity> = (0..5)
            .map(|i| {
                Entity::Insert(InsertEntity {
                    common: common.clone(),
                    block_name: Ref::Resolved("R".to_string()),
                    insertion_point: Point3D {
                        x: i as f64,
                        y: 0.0,
                        z: 0.0,
                    },
                    scale: Point3D {
                        x: 1.0,
                        y: 1.0,
                        z: 1.0,
                    },
                    rotation: 0.0,
                    attribs: Vec::new(),
                })
            })
            .collect();
        let mut block_records = BTreeMap::new();
        block_records.insert(
            "R".to_string(),
            BlockRecord {
                name: "R".to_string(),
                entities: children,
            },
        );
        let tables = Tables {
            block_records,
            ..Default::default()
        };

        let mut ctx = Ctx::new(&tables);
        let owner = Entity::Insert(InsertEntity {
            common: common.clone(),
            block_name: Ref::Resolved("R".to_string()),
            insertion_point: Point3D {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            scale: Point3D {
                x: 1.0,
                y: 1.0,
                z: 1.0,
            },
            rotation: 0.0,
            attribs: Vec::new(),
        });
        let svg = render_block_ref(
            &owner,
            &Ref::Resolved("R".to_string()),
            Affine2::IDENTITY,
            DEFAULT_COLOR,
            &mut ctx,
        );
        assert!(
            !svg.is_empty(),
            "the shallow levels within budget should still render something"
        );
        assert_eq!(
            ctx.block_ref_budget, 0,
            "the budget, not the depth cap, should be what stopped this combinatorial blowup"
        );
    }

    #[test]
    fn consider_drops_non_finite_coordinates_instead_of_recording_them() {
        let tables = Tables::default();
        let mut ctx = Ctx::new(&tables);
        ctx.consider(1.0, 2.0);
        ctx.consider(3.0, f64::INFINITY);
        ctx.consider(f64::NAN, 4.0);
        // Only the fully-finite point should have been recorded: a box
        // corrupted by a non-finite coordinate used to leave min == max ==
        // Infinity, whose diagonal is NaN, which panics the comparisons in
        // `bounds`.
        let b = ctx.entity_box().expect("one finite point was recorded");
        close(b.min_x, 1.0);
        close(b.max_x, 1.0);
        close(b.min_y, 2.0);
        close(b.max_y, 2.0);
        assert!(diag(&b).is_finite());
    }

    #[test]
    fn resolve_stroke_widths_divides_by_the_embedded_scale() {
        let out = resolve_stroke_widths("prefix @@SW@@2@@ middle @@SW@@0.5@@ suffix", 10.0);
        assert_eq!(out, "prefix 5 middle 20 suffix");
    }

    #[test]
    fn resolve_stroke_widths_emits_a_malformed_placeholder_verbatim() {
        let out = resolve_stroke_widths("before @@SW@@2 no closing marker", 10.0);
        assert_eq!(out, "before @@SW@@2 no closing marker");
    }

    #[test]
    fn a_placement_scales_rotates_then_translates_through_the_models_map() {
        let t = Affine2::placement(Point2D { x: 10.0, y: 20.0 }, 2.0, 2.0, 0.0);
        let p = t.apply(Point2D { x: 1.0, y: 1.0 });
        close(p.x, 12.0);
        close(p.y, 22.0);
    }

    #[test]
    fn a_child_placement_composes_within_its_parents_space() {
        let parent = Affine2::placement(Point2D { x: 10.0, y: 0.0 }, 1.0, 1.0, 0.0);
        let child = Affine2::placement(Point2D { x: 1.0, y: 1.0 }, 2.0, 2.0, 0.0);
        let composed = child.then(&parent);
        close(composed.e, 11.0);
        close(composed.f, 1.0);
        close(composed.a, 2.0);
        close(composed.d, 2.0);
        let p = composed.apply(Point2D { x: 1.0, y: 0.0 });
        close(p.x, 13.0);
        close(p.y, 1.0);
    }

    #[test]
    fn the_area_scale_of_a_composed_placement_is_the_product_of_each_levels() {
        // What stroke widths are divided by: the same number the old
        // per-level `sqrt(|sx * sy|)` product gave, now read off the
        // composed placement so the two cannot drift.
        let a = Affine2::placement(Point2D { x: 3.0, y: 0.0 }, 2.0, 2.0, 0.7);
        let b = Affine2::placement(Point2D { x: 1.0, y: 1.0 }, 1.5, 4.0, -0.2);
        let composed = b.then(&a);
        close(
            composed.determinant().abs().sqrt(),
            (2.0f64 * 2.0).abs().sqrt() * (1.5f64 * 4.0).abs().sqrt(),
        );
        // A mirrored placement scales by the same amount it would unmirrored.
        let m = Affine2::placement(Point2D { x: 0.0, y: 0.0 }, -3.0, 3.0, 0.0);
        close(m.determinant().abs().sqrt(), 3.0);
    }

    #[test]
    fn the_svg_matrix_is_the_placement_conjugated_by_the_y_flip() {
        // A quarter turn, scale 2, at (10, 20): in CAD space local (1, 0)
        // goes to (10, 22); in SVG space y is down, so it goes to (10, -22).
        let t = Affine2::placement(
            Point2D { x: 10.0, y: 20.0 },
            2.0,
            2.0,
            std::f64::consts::FRAC_PI_2,
        );
        let [a, b, c, d, e, f] = svg_matrix(&t, Frame::default(), Frame::default());
        let (x, y) = (a * 1.0 + c * 0.0 + e, b * 1.0 + d * 0.0 + f);
        close(x, 10.0);
        close(y, -22.0);
    }

    #[test]
    fn a_point_is_a_cross_sized_in_stroke_widths_not_in_drawing_units() {
        // Resolved at a stroke of 0.25 units, the arms end 2 * 0.25 = 0.5
        // units from the point, and the cross is drawn one stroke wide.
        let at = Point2D { x: 7.0, y: 9.0 };
        let svg = point_cross_element(at, "#000000", Frame::default(), 1.0);
        assert!(svg.contains("M -2 0 L 2 0 M 0 -2 L 0 2"), "{svg}");
        assert!(svg.contains("translate(7 -9)"), "{svg}");
        assert!(svg.contains("stroke-width=\"1\""), "{svg}");
        let resolved = resolve_stroke_widths(&svg, 0.25);
        assert!(resolved.contains("scale(0.25)"), "{resolved}");
        // Inside a block scaled 4x the placeholder carries that scale, so
        // the cross still comes out one stroke width wide on the page.
        let nested = resolve_stroke_widths(
            &point_cross_element(at, "#000000", Frame::default(), 4.0),
            0.25,
        );
        assert!(nested.contains("scale(0.0625)"), "{nested}");
    }

    #[test]
    fn mline_offset_points_zero_offset_is_the_centerline() {
        let verts = vec![
            MLineVertex {
                point: Point3D {
                    x: 1.0,
                    y: 2.0,
                    z: 0.0,
                },
                miter_direction: Point3D {
                    x: 1.0,
                    y: 0.0,
                    z: 0.0,
                },
            },
            MLineVertex {
                point: Point3D {
                    x: 3.0,
                    y: 4.0,
                    z: 0.0,
                },
                miter_direction: Point3D {
                    x: 0.0,
                    y: 1.0,
                    z: 0.0,
                },
            },
        ];
        let pts = mline_offset_points(&verts, 0.0);
        close(pts[0].x, 1.0);
        close(pts[0].y, 2.0);
        close(pts[1].x, 3.0);
        close(pts[1].y, 4.0);
    }

    #[test]
    fn mline_offset_points_displaces_along_miter_direction() {
        let verts = vec![MLineVertex {
            point: Point3D {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            miter_direction: Point3D {
                x: 0.0,
                y: 1.0,
                z: 0.0,
            },
        }];
        let pts = mline_offset_points(&verts, 5.0);
        close(pts[0].x, 0.0);
        close(pts[0].y, 5.0);

        let pts_negative = mline_offset_points(&verts, -5.0);
        close(pts_negative[0].y, -5.0);
    }

    fn render_one(entity: Entity) -> String {
        let db = CadDatabase {
            entities: vec![entity],
            tables: Tables::default(),
            read_diagnostics: Default::default(),
        };
        let options = ToSvgOptions {
            space: Space::All,
            ..ToSvgOptions::default()
        };
        to_svg(&db, options).svg
    }

    fn plain_common() -> EntityCommon {
        use uncad_model::model::{Confidence, Origin, Ref};
        EntityCommon {
            id: EntityId::new(1),
            origin: Origin::Vector,
            confidence: Confidence::High,
            source_handle: Ref::Absent,
            layer: Ref::Absent,
            color_index: 7,
            true_color: None,
            invisible: false,
        }
    }

    fn leader(has_arrowhead: Option<bool>) -> Entity {
        use uncad_model::model::{LeaderAnnotation, LeaderEntity, Ref};
        let p = |x| Point3D { x, y: 0.0, z: 0.0 };
        Entity::Leader(LeaderEntity {
            common: plain_common(),
            vertices: vec![p(0.0), p(10.0)],
            has_arrowhead,
            path_type: None,
            annotation: LeaderAnnotation::Nothing,
            annotation_id: Ref::Absent,
            style_name: Ref::Absent,
        })
    }

    #[test]
    fn a_leader_draws_an_arrowhead_only_where_its_file_states_one() {
        assert!(render_one(leader(Some(true))).contains("<polygon"));
        assert!(!render_one(leader(Some(false))).contains("<polygon"));
        assert!(
            !render_one(leader(None)).contains("<polygon"),
            "a flag the file did not state is not an arrowhead"
        );
    }

    fn light(light_type: Option<LightType>) -> Entity {
        use uncad_model::model::LightEntity;
        Entity::Light(LightEntity {
            common: plain_common(),
            position: Point3D {
                x: 0.0,
                y: 0.0,
                z: 10.0,
            },
            target: Point3D {
                x: 5.0,
                y: 5.0,
                z: 0.0,
            },
            light_type,
        })
    }

    #[test]
    fn only_a_light_the_file_says_aims_gets_a_line_to_its_target() {
        for aiming in [LightType::Distant, LightType::Spot] {
            assert!(
                render_one(light(Some(aiming))).contains("<line"),
                "{aiming:?}"
            );
        }
        assert!(!render_one(light(Some(LightType::Point))).contains("<line"));
        assert!(
            !render_one(light(None)).contains("<line"),
            "a light of no stated kind is not given a direction"
        );
    }

    fn ellipse(start: f64, end: f64) -> Entity {
        Entity::Ellipse(EllipseEntity {
            common: plain_common(),
            center: Point3D {
                x: 1.0,
                y: 1.0,
                z: 0.0,
            },
            // Major axis along +y, length 2; minor axis length 1, along -x.
            major_axis_endpoint: Point3D {
                x: 0.0,
                y: 2.0,
                z: 0.0,
            },
            axis_ratio: 0.5,
            start_angle: start,
            end_angle: end,
            extrusion: Point3D {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            },
        })
    }

    /// The numbers of the one `<path d="M x y A rx ry rot large sweep x y">`.
    fn arc_numbers(svg: &str) -> Vec<f64> {
        let d = svg
            .split("d=\"")
            .nth(1)
            .expect("a path")
            .split('"')
            .next()
            .unwrap();
        d.split_whitespace()
            .filter(|t| *t != "M" && *t != "A")
            .map(|t| t.parse().unwrap())
            .collect()
    }

    #[test]
    fn a_full_ellipse_is_drawn_whole() {
        let svg = render_one(ellipse(0.0, std::f64::consts::TAU));
        assert!(svg.contains("<ellipse") && !svg.contains("<path"));
    }

    #[test]
    fn a_partial_ellipse_is_drawn_from_its_start_to_its_end_parameter() {
        use std::f64::consts::FRAC_PI_2;
        // Parameter 0 is the major-axis end (1, 3); a quarter turn on is the
        // minor-axis end, the major axis turned counter-clockwise: (0, 1).
        let n = arc_numbers(&render_one(ellipse(0.0, FRAC_PI_2)));
        let [x1, y1, rx, ry, _rot, large, sweep, x2, y2] = n[..] else {
            panic!("{n:?}")
        };
        assert_eq!((x1, -y1), (1.0, 3.0));
        assert!(
            (x2 - 0.0).abs() < 1e-12 && (-y2 - 1.0).abs() < 1e-12,
            "({x2}, {y2})"
        );
        assert_eq!((rx, ry, large, sweep), (2.0, 1.0, 0.0, 0.0));
    }

    #[test]
    fn an_ellipse_arc_that_crosses_parameter_zero_takes_the_long_way() {
        // From 1.0 counter-clockwise round to 0.5: a sweep of TAU - 0.5.
        let n = arc_numbers(&render_one(ellipse(1.0, 0.5)));
        assert_eq!(n[5], 1.0, "large-arc flag: {n:?}");
    }

    #[test]
    fn an_mtext_block_hangs_from_its_attachment_point() {
        use MTextAttachment as A;
        // Height 2, three lines 2.4 apart: a block 6.8 tall. SVG y points down.
        let at = |a| mtext_placement(Some(a), 0.0, 2.0, 2.4, 3);
        assert_eq!(at(A::TopLeft), ("start", 2.0));
        let (anchor, baseline) = at(A::MiddleCenter);
        assert_eq!(anchor, "middle");
        assert!((baseline - (-3.4 + 2.0)).abs() < 1e-12);
        let (anchor, baseline) = at(A::BottomRight);
        assert_eq!(anchor, "end");
        assert!((baseline - (-6.8 + 2.0)).abs() < 1e-12);
        // Unstated: the insertion point stays the first baseline.
        assert_eq!(mtext_placement(None, 5.0, 2.0, 2.4, 3), ("start", 5.0));
    }

    #[test]
    fn an_entity_the_file_marks_invisible_is_not_drawn() {
        let line = |invisible| {
            let mut common = plain_common();
            common.invisible = invisible;
            Entity::Line(uncad_model::model::LineEntity {
                common,
                start_point: Point3D {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                end_point: Point3D {
                    x: 1.0,
                    y: 1.0,
                    z: 0.0,
                },
            })
        };
        assert!(render_one(line(false)).contains("<line"));
        assert!(!render_one(line(true)).contains("<line"));
    }

    #[test]
    fn a_mirrored_ellipse_arc_turns_the_other_way() {
        use std::f64::consts::FRAC_PI_2;
        let Entity::Ellipse(mut el) = ellipse(0.0, FRAC_PI_2) else {
            unreachable!()
        };
        el.extrusion = Point3D {
            x: 0.0,
            y: 0.0,
            z: -1.0,
        };
        // The minor axis is now the major axis turned clockwise: (1, 0) long,
        // so parameter pi/2 lands at (2, 1), and the arc runs clockwise.
        let n = arc_numbers(&render_one(Entity::Ellipse(el)));
        let [x1, y1, _rx, _ry, _rot, large, sweep, x2, y2] = n[..] else {
            panic!("{n:?}")
        };
        assert_eq!((x1, -y1), (1.0, 3.0));
        assert!(
            (x2 - 2.0).abs() < 1e-12 && (-y2 - 1.0).abs() < 1e-12,
            "({x2}, {y2})"
        );
        assert_eq!((large, sweep), (0.0, 1.0));
    }

    #[test]
    fn an_ellipse_whose_normal_is_z_up_to_rounding_is_still_an_exact_arc() {
        let Entity::Ellipse(mut el) = ellipse(0.0, 1.0) else {
            unreachable!()
        };
        el.extrusion = Point3D {
            x: 1e-17,
            y: -2e-17,
            z: 1.0,
        };
        assert!(render_one(Entity::Ellipse(el)).contains("<path d=\"M"));
    }
}
