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
//! string formatting), [`hatch`] (HATCH fills), [`spline`] (SPLINE curves)
//! and [`bounds`] (viewBox and outlier trim).

mod bounds;
mod format;
mod hatch;
mod spline;
mod text_codes;

use crate::color::{resolve_color, DEFAULT_COLOR};
use bounds::{dominant_cluster_box, Box2D};
use format::{clean, escape_xml, neg, points_attr, rotate_transform_attr, xy};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use uncad_model::bulge::{self, Segment};
use uncad_model::model::{
    ArcEntity, AttribEntity, CircleEntity, EllipseEntity, Entity, EntityCommon, EntityId,
    LightType, LwPolylineEntity, MLineVertex, MTextAttachment, Point2D, Point3D, PolylineVertex,
    TextEntity, TextHorizontalAlignment, TextVerticalAlignment,
};
use uncad_model::tables::Tables;
use uncad_model::{Affine2, CadDatabase, Ocs};

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
}

impl Default for ToSvgOptions {
    fn default() -> Self {
        ToSvgOptions {
            padding: 5.0,
            stroke_width: None,
            space: Space::Model,
            outlier_trim: true,
        }
    }
}

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
}

// --- block transform ---------------------------------------------------

/// The SVG `matrix(a b c d e f)` of a placement, composed with the
/// renderer's CAD-y-up to SVG-y-down flip: conjugating the map by the flip
/// negates the two off-diagonal entries and the y translation.
fn svg_matrix(t: &Affine2) -> [f64; 6] {
    [t.a, neg(t.b), neg(t.c), t.d, t.e, neg(t.f)]
}

// --- render context ----------------------------------------------------

/// Generous enough for any real drawing this project has been checked against
/// while still cutting off combinatorial block-reference blowup quickly: 10
/// INSERTs per level exhausts it by nesting level 6, long before the 20-level
/// depth cap could engage.
const BLOCK_REF_BUDGET: u32 = 1_000_000;

/// How deep block references may nest before rendering gives up.
const MAX_BLOCK_REF_DEPTH: u32 = 20;

struct Ctx<'a> {
    xs: Vec<f64>,
    ys: Vec<f64>,
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
    tables: &'a Tables,
    depth: u32,
    scale: f64,
    inherited_color: String,
    /// Local (inside the block being rendered) -> world, composed across
    /// nested block references through the model's placement arithmetic.
    transform: Affine2,
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
}

impl<'a> Ctx<'a> {
    fn new(tables: &'a Tables) -> Self {
        Ctx {
            xs: Vec::new(),
            ys: Vec::new(),
            ent_min_x: f64::INFINITY,
            ent_max_x: f64::NEG_INFINITY,
            ent_min_y: f64::INFINITY,
            ent_max_y: f64::NEG_INFINITY,
            unsupported: BTreeSet::new(),
            empty_blocks: BTreeSet::new(),
            tables,
            depth: 0,
            scale: 1.0,
            inherited_color: DEFAULT_COLOR.to_string(),
            transform: Affine2::IDENTITY,
            defs: Vec::new(),
            next_def_id: 0,
            block_ref_budget: BLOCK_REF_BUDGET,
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
        self.xs.push(x);
        self.ys.push(y);
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
fn polyline_element(pts: &[Point2D], closed: bool, color: &str) -> String {
    let tag = if closed { "polygon" } else { "polyline" };
    format!(
        "<{tag} points=\"{}\" fill=\"none\" stroke=\"{color}\"/>",
        points_attr(pts)
    )
}

/// Points sampled around a full turn of a circle drawn in a plane that is
/// not the world's, seen from above.
const OWN_PLANE_SAMPLES: usize = 64;

/// The coordinate system a CIRCLE or ARC is written in. An extrusion that
/// names no plane (zero, or not finite) is drawn in the world's.
fn own_plane(extrusion: Point3D) -> Ocs {
    Ocs::of(extrusion).unwrap_or(Ocs::WORLD)
}

/// A point of `plane` in world coordinates, seen from above: the page is the
/// world XY plane, so the world z is dropped.
fn seen_from_above(plane: Ocs, p: Point3D) -> Point2D {
    let w = plane.to_world(p);
    Point2D { x: w.x, y: w.y }
}

/// Where the points of `plane` at height `elevation` land on the page: the
/// plane taken to the world and seen from above, which drops the world z.
/// Both steps are linear, so the whole map is one affine map -- exact for
/// anything drawn in the plane, and degenerate (a line) only for a plane
/// seen edge-on.
fn plane_seen_from_above(plane: Ocs, elevation: f64) -> Affine2 {
    let (x, y, z) = (plane.x_axis(), plane.y_axis(), plane.z_axis());
    Affine2 {
        a: x.x,
        b: x.y,
        c: y.x,
        d: y.y,
        e: elevation * z.x,
        f: elevation * z.y,
    }
}

/// What `draw` emits in the coordinates `view` takes to the current ones,
/// wrapped in a group that applies it. Bounds are recorded through `view`
/// as well, the way a block reference records its contents'.
fn in_view(
    view: Affine2,
    ctx: &mut Ctx,
    draw: impl FnOnce(&mut Ctx) -> Option<String>,
) -> Option<String> {
    let parent = ctx.transform;
    ctx.transform = view.then(&parent);
    let body = draw(ctx);
    ctx.transform = parent;
    let [a, b, c, d, e, f] = svg_matrix(&view);
    body.map(|body| {
        format!(
            "<g transform=\"matrix({a} {b} {c} {d} {e} {f})\">
  {body}
</g>"
        )
    })
}

/// A CIRCLE whose extrusion is not the world Z axis. Facing down (a mirror
/// copy) it is still a circle, at its center taken to the world; on a tilted
/// plane it is seen from above, so it is drawn through points of its outline.
fn circle_in_own_plane(c: &CircleEntity, color: &str, ctx: &mut Ctx) -> String {
    let plane = own_plane(c.extrusion);
    if plane.is_flat() {
        let center = seen_from_above(plane, c.center);
        ctx.consider(center.x - c.radius, center.y - c.radius);
        ctx.consider(center.x + c.radius, center.y + c.radius);
        return format!(
            "<circle cx=\"{}\" cy=\"{}\" r=\"{}\" fill=\"none\" stroke=\"{color}\"/>",
            clean(center.x),
            neg(center.y),
            c.radius
        );
    }
    let points: Vec<Point2D> = (0..OWN_PLANE_SAMPLES)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / OWN_PLANE_SAMPLES as f64;
            seen_from_above(plane, on_circle(c.center, c.radius, a))
        })
        .collect();
    ctx.consider_all(&points);
    polyline_element(&points, true, color)
}

/// An ARC whose extrusion is not the world Z axis. Its angles run
/// counter-clockwise about the extrusion, so facing down (a mirror copy) it
/// runs clockwise in the world; on a tilted plane it is seen from above and
/// drawn through points of its outline.
fn arc_in_own_plane(a: &ArcEntity, color: &str, ctx: &mut Ctx) -> String {
    let plane = own_plane(a.extrusion);
    let mut sweep = a.end_angle - a.start_angle;
    if sweep < 0.0 {
        sweep += std::f64::consts::TAU;
    }
    let start = seen_from_above(plane, on_circle(a.center, a.radius, a.start_angle));
    let end = seen_from_above(plane, on_circle(a.center, a.radius, a.end_angle));
    if plane.is_flat() {
        let center = seen_from_above(plane, a.center);
        ctx.consider(center.x - a.radius, center.y - a.radius);
        ctx.consider(center.x + a.radius, center.y + a.radius);
        let r = a.radius;
        let large = u8::from(sweep > std::f64::consts::PI);
        // Clockwise in the world, which the page's flipped y axis turns
        // counter-clockwise: SVG's sweep flag 1.
        return format!(
            "<path d=\"M {} {} A {r} {r} 0 {large} 1 {} {}\" fill=\"none\" stroke=\"{color}\"/>",
            clean(start.x),
            neg(start.y),
            clean(end.x),
            neg(end.y)
        );
    }
    let steps = ((OWN_PLANE_SAMPLES as f64 * sweep / std::f64::consts::TAU).ceil() as usize).max(2);
    let points: Vec<Point2D> = (0..=steps)
        .map(|i| {
            let t = a.start_angle + sweep * i as f64 / steps as f64;
            seen_from_above(plane, on_circle(a.center, a.radius, t))
        })
        .collect();
    ctx.consider_all(&points);
    polyline_element(&points, false, color)
}

/// The point at `angle` on the circle of `radius` about `center`, in the
/// circle's own coordinate system.
fn on_circle(center: Point3D, radius: f64, angle: f64) -> Point3D {
    Point3D {
        x: center.x + radius * angle.cos(),
        y: center.y + radius * angle.sin(),
        z: center.z,
    }
}

/// A polyline in the world's plane: a `<polyline>` or `<polygon>` when every
/// segment is straight, a `<path>` with exact arcs otherwise.
fn polyline_drawn(vertices: &[PolylineVertex], closed: bool, color: &str, ctx: &mut Ctx) -> String {
    let points: Vec<Point2D> = vertices.iter().map(|v| v.point).collect();
    ctx.consider_all(&points);
    if vertices.iter().all(|v| v.bulge == 0.0) {
        return polyline_element(&points, closed, color);
    }
    bulged_polyline_element(vertices, closed, color, ctx)
}

/// A polyline on a tilted plane, seen from above: its arcs sampled in its
/// own coordinate system, then every point taken to the world.
fn polyline_on_tilted_plane(
    p: &LwPolylineEntity,
    plane: Ocs,
    color: &str,
    ctx: &mut Ctx,
) -> String {
    let mut own: Vec<Point2D> = Vec::new();
    for Segment { from, arc, .. } in bulge::segments(&p.vertices, p.closed) {
        own.push(from);
        if let Some(arc) = arc {
            let steps = 12;
            own.extend(
                (1..steps).map(|i| {
                    arc.at(arc.start_angle + arc.sweep * (f64::from(i) / f64::from(steps)))
                }),
            );
        }
    }
    if !p.closed || own.is_empty() {
        own.extend(p.vertices.last().map(|v| v.point));
    }
    let world: Vec<Point2D> = own
        .into_iter()
        .map(|q| {
            seen_from_above(
                plane,
                Point3D {
                    x: q.x,
                    y: q.y,
                    z: p.elevation,
                },
            )
        })
        .collect();
    ctx.consider_all(&world);
    polyline_element(&world, p.closed, color)
}

/// A polyline with arc segments, as a `<path>`: a straight segment is a line,
/// a bulged one an exact SVG arc. The arcs' extreme points join the bounds,
/// since an arc reaches past its two ends.
fn bulged_polyline_element(
    vertices: &[PolylineVertex],
    closed: bool,
    color: &str,
    ctx: &mut Ctx,
) -> String {
    let first = vertices[0].point;
    let mut d = format!("M {} {}", clean(first.x), neg(first.y));
    for Segment { to, arc, .. } in bulge::segments(vertices, closed) {
        match arc {
            Some(arc) => {
                for e in arc.extremes() {
                    ctx.consider(e.x, e.y);
                }
                let r = clean(arc.radius);
                let large = u8::from(arc.sweep.abs() > std::f64::consts::PI);
                // The y axis is flipped on the way out, which turns a
                // counter-clockwise arc into a clockwise one on the page.
                let sweep = u8::from(arc.sweep < 0.0);
                let _ = write!(
                    d,
                    " A {r} {r} 0 {large} {sweep} {} {}",
                    clean(to.x),
                    neg(to.y)
                );
            }
            None => {
                let _ = write!(d, " L {} {}", clean(to.x), neg(to.y));
            }
        }
    }
    if closed {
        d.push_str(" Z");
    }
    format!("<path d=\"{d}\" fill=\"none\" stroke=\"{color}\"/>")
}

/// A dashed outline, used for the shapes this renderer draws as an indication
/// rather than as real geometry (VIEWPORT frames, WIPEOUT boundaries).
fn dashed_outline(pts: &[Point2D], color: &str, dash: &str) -> String {
    format!(
        "<polygon points=\"{}\" fill=\"none\" stroke-dasharray=\"{dash}\" stroke=\"{color}\"/>",
        points_attr(pts)
    )
}

/// A single-line `<text>` at an already-y-flipped position -- TEXT, ATTRIB and
/// TOLERANCE all render to this.
fn text_element(at: Point2D, height: f64, rotation: f64, color: &str, text: &str) -> String {
    let (x, y) = (at.x, neg(at.y));
    format!(
        "<text x=\"{x}\" y=\"{y}\" font-size=\"{height}\" fill=\"{color}\" stroke=\"none\"{}>{}</text>",
        rotate_transform_attr(rotation, x, y),
        escape_xml(text)
    )
}

/// Where a single line of text goes: what a TEXT and an ATTRIB both state.
struct TextPlacement {
    start_point: Point2D,
    text_height: f64,
    rotation: f64,
    horizontal_alignment: TextHorizontalAlignment,
    vertical_alignment: TextVerticalAlignment,
    alignment_point: Option<Point2D>,
    width_factor: f64,
}

impl From<&TextEntity> for TextPlacement {
    fn from(t: &TextEntity) -> Self {
        TextPlacement {
            start_point: t.start_point,
            text_height: t.text_height,
            rotation: t.rotation,
            horizontal_alignment: t.horizontal_alignment,
            vertical_alignment: t.vertical_alignment,
            alignment_point: t.alignment_point,
            width_factor: t.width_factor,
        }
    }
}

impl From<&AttribEntity> for TextPlacement {
    fn from(a: &AttribEntity) -> Self {
        TextPlacement {
            start_point: a.start_point,
            text_height: a.text_height,
            rotation: a.rotation,
            horizontal_alignment: a.horizontal_alignment,
            vertical_alignment: a.vertical_alignment,
            alignment_point: a.alignment_point,
            width_factor: a.width_factor,
        }
    }
}

/// A TEXT or an ATTRIB as a `<text>`, placed the way its alignment says.
///
/// Left and baseline -- or an alignment the file gives no point for -- is
/// drawn from the start point. Otherwise the alignment point is the one the
/// text answers to: an SVG `text-anchor` puts the text's middle or end
/// there, and `dominant-baseline` its middle, top or bottom. `Aligned` and
/// `Fit` run from the start point to the alignment point along the line
/// between them, stretched to that length (`textLength`). The width factor
/// narrows or widens the characters about the point the text is placed at.
/// The font drawn is not the file's, so the start point the file states --
/// computed from the file's font -- would not center a centered text; the
/// alignment point does.
fn aligned_text_element(t: &TextPlacement, color: &str, text: &str) -> String {
    use TextHorizontalAlignment as H;
    use TextVerticalAlignment as V;
    let height = t.text_height;
    let plain = (t.horizontal_alignment, t.vertical_alignment) == (H::Left, V::Baseline);
    let (at, mut extra, rotation) = match (t.horizontal_alignment, t.alignment_point) {
        (_, None) => (t.start_point, String::new(), t.rotation),
        _ if plain => (t.start_point, String::new(), t.rotation),
        (H::Aligned | H::Fit, Some(end)) => {
            let (dx, dy) = (end.x - t.start_point.x, end.y - t.start_point.y);
            let length = dx.hypot(dy);
            if length == 0.0 || !length.is_finite() {
                (t.start_point, String::new(), t.rotation)
            } else {
                (
                    t.start_point,
                    format!(
                        " textLength=\"{}\" lengthAdjust=\"spacingAndGlyphs\"",
                        clean(length)
                    ),
                    dy.atan2(dx),
                )
            }
        }
        (h, Some(point)) => {
            let anchor = match h {
                H::Center | H::Middle => " text-anchor=\"middle\"",
                H::Right => " text-anchor=\"end\"",
                _ => "",
            };
            (point, anchor.to_string(), t.rotation)
        }
    };
    if !plain && t.alignment_point.is_some() {
        let baseline = match (t.horizontal_alignment, t.vertical_alignment) {
            (H::Middle, _) | (_, V::Middle) => " dominant-baseline=\"central\"",
            (_, V::Top) => " dominant-baseline=\"text-before-edge\"",
            (_, V::Bottom) => " dominant-baseline=\"text-after-edge\"",
            (_, V::Baseline) => "",
        };
        extra.push_str(baseline);
    }
    let (x, y) = (at.x, neg(at.y));
    let mut transform = Vec::new();
    if rotation != 0.0 {
        transform.push(format!("rotate({} {x} {y})", neg(rotation.to_degrees())));
    }
    let w = t.width_factor;
    if w != 1.0 && w.is_finite() && w > 0.0 {
        transform.push(format!(
            "translate({x} {y}) scale({} 1) translate({} {})",
            clean(w),
            neg(x),
            neg(y)
        ));
    }
    let transform = if transform.is_empty() {
        String::new()
    } else {
        format!(" transform=\"{}\"", transform.join(" "))
    };
    format!(
        "<text x=\"{x}\" y=\"{y}\" font-size=\"{height}\" fill=\"{color}\" stroke=\"none\"{extra}{transform}>{}</text>",
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
fn arrowhead_element(tip: &Point2D, from: &Point2D, size: f64, color: &str) -> String {
    let (dx, dy) = (tip.x - from.x, tip.y - from.y);
    let len = dx.hypot(dy);
    let len = if len == 0.0 { 1.0 } else { len };
    let (ux, uy) = (dx / len, dy / len);
    let (px, py) = (-uy, ux);
    let (back_x, back_y) = (tip.x - ux * size, tip.y - uy * size);
    let (p2x, p2y) = (back_x + px * size * 0.35, back_y + py * size * 0.35);
    let (p3x, p3y) = (back_x - px * size * 0.35, back_y - py * size * 0.35);
    format!(
        "<polygon points=\"{},{} {p2x},{} {p3x},{}\" fill=\"{color}\" stroke=\"none\"/>",
        tip.x,
        neg(tip.y),
        neg(p2y),
        neg(p3y)
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
            format!(
                "<line x1=\"{x1}\" y1=\"{}\" x2=\"{x2}\" y2=\"{}\" stroke=\"{color}\"/>",
                neg(y1),
                neg(y2)
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
        // which of the two it was.
        common.layer.name(),
        ctx.tables,
        &ctx.inherited_color,
    )
}

// --- entity rendering --------------------------------------------------

/// Looks `block_name` up in `ctx.tables.block_records` and recursively renders
/// its entities under a nested transform, wrapped in a
/// `<g transform="matrix(...)">`.
///
/// ATTDEF children are skipped: an attribute *template* is not drawn, and the
/// real values are separate top-level ATTRIB entities already rendered.
fn render_block_ref(
    block_name: &str,
    insertion_point: Point2D,
    x_scale: f64,
    y_scale: f64,
    rotation: f64,
    color: &str,
    ctx: &mut Ctx,
) -> String {
    let Some(block) = ctx.tables.block_records.get(block_name) else {
        return String::new();
    };
    if block.entities.is_empty() {
        ctx.empty_blocks.insert(block_name.to_string());
        return String::new();
    }
    if ctx.depth > MAX_BLOCK_REF_DEPTH || ctx.block_ref_budget == 0 {
        return String::new();
    }
    ctx.block_ref_budget -= 1;

    let child_transform = Affine2::placement(insertion_point, x_scale, y_scale, rotation);
    // Compose: local (within the block) -> world, via this block's own
    // placement followed by the parent's already-established one. The
    // parent's own state is restored afterwards.
    let parent_transform = ctx.transform;
    let parent_depth = ctx.depth;
    let parent_scale = ctx.scale;
    let parent_inherited = std::mem::replace(&mut ctx.inherited_color, color.to_string());

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

    let mut body_parts = Vec::new();
    for child in &block.entities {
        if matches!(child, Entity::Attdef(_)) {
            continue;
        }
        if let Some(svg) = render_entity(child, ctx) {
            body_parts.push(svg);
        }
    }

    ctx.transform = parent_transform;
    ctx.depth = parent_depth;
    ctx.scale = parent_scale;
    ctx.inherited_color = parent_inherited;

    if body_parts.is_empty() {
        ctx.empty_blocks.insert(block_name.to_string());
        return String::new();
    }

    // The parent transform is baked into ctx.transform for *bounds* purposes
    // (world-space consider()), but the emitted matrix is only this block's own
    // local transform -- nesting is expressed by nested <g> elements.
    let [a, b, c, d, e, f] = svg_matrix(&child_transform);
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
    let color = resolve_entity_color(e.common(), ctx);
    match e {
        Entity::Line(l) => {
            ctx.consider(l.start_point.x, l.start_point.y);
            ctx.consider(l.end_point.x, l.end_point.y);
            Some(format!(
                "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"{color}\"/>",
                l.start_point.x,
                neg(l.start_point.y),
                l.end_point.x,
                neg(l.end_point.y)
            ))
        }
        Entity::Circle(c) if !own_plane(c.extrusion).is_world() => {
            Some(circle_in_own_plane(c, &color, ctx))
        }
        Entity::Arc(a) if !own_plane(a.extrusion).is_world() => {
            Some(arc_in_own_plane(a, &color, ctx))
        }
        Entity::Circle(c) => {
            ctx.consider(c.center.x - c.radius, c.center.y - c.radius);
            ctx.consider(c.center.x + c.radius, c.center.y + c.radius);
            Some(format!(
                "<circle cx=\"{}\" cy=\"{}\" r=\"{}\" fill=\"none\" stroke=\"{color}\"/>",
                c.center.x,
                neg(c.center.y),
                c.radius
            ))
        }
        Entity::Arc(a) => {
            ctx.consider(a.center.x - a.radius, a.center.y - a.radius);
            ctx.consider(a.center.x + a.radius, a.center.y + a.radius);
            let (x, y, r) = (a.center.x, a.center.y, a.radius);
            let (x1, y1) = (x + r * a.start_angle.cos(), y + r * a.start_angle.sin());
            let (x2, y2) = (x + r * a.end_angle.cos(), y + r * a.end_angle.sin());
            let mut sweep = a.end_angle - a.start_angle;
            if sweep < 0.0 {
                sweep += 2.0 * std::f64::consts::PI;
            }
            let large = if sweep > std::f64::consts::PI { 1 } else { 0 };
            Some(format!(
                "<path d=\"M {x1} {} A {r} {r} 0 {large} 0 {x2} {}\" fill=\"none\" stroke=\"{color}\"/>",
                neg(y1), neg(y2)
            ))
        }
        Entity::Ellipse(el) => {
            let rx = el.major_axis_endpoint.x.hypot(el.major_axis_endpoint.y);
            ctx.consider(el.center.x - rx, el.center.y - rx);
            ctx.consider(el.center.x + rx, el.center.y + rx);
            let ry = rx * el.axis_ratio;
            let rot = el
                .major_axis_endpoint
                .y
                .atan2(el.major_axis_endpoint.x)
                .to_degrees();
            let (cx, cy) = (el.center.x, neg(el.center.y));
            let sweep = ellipse_sweep(el.start_angle, el.end_angle);
            if sweep >= std::f64::consts::TAU {
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
                return Some(polyline_element(&points, false, &color));
            }
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
                p1.x,
                neg(p1.y),
                neg(rot),
                p2.x,
                neg(p2.y)
            ))
        }
        Entity::LwPolyline(p) | Entity::Polyline2D(p) => {
            let plane = own_plane(p.extrusion);
            if plane.is_world() {
                return Some(polyline_drawn(&p.vertices, p.closed, &color, ctx));
            }
            if plane.is_flat() {
                // A mirror copy: its vertices taken to the world, and every
                // arc turning the other way there.
                let world: Vec<PolylineVertex> = p
                    .vertices
                    .iter()
                    .map(|v| PolylineVertex {
                        point: seen_from_above(
                            plane,
                            Point3D {
                                x: v.point.x,
                                y: v.point.y,
                                z: p.elevation,
                            },
                        ),
                        bulge: -v.bulge,
                    })
                    .collect();
                return Some(polyline_drawn(&world, p.closed, &color, ctx));
            }
            Some(polyline_on_tilted_plane(p, plane, &color, ctx))
        }
        Entity::Polyline3D(p) => {
            if p.vertices.is_empty() {
                return None;
            }
            ctx.consider_all_3d(&p.vertices);
            Some(polyline_element(&xy(&p.vertices), p.closed, &color))
        }
        Entity::Text(t) => {
            ctx.consider(t.start_point.x, t.start_point.y);
            if let Some(a) = t.alignment_point {
                ctx.consider(a.x, a.y);
            }
            Some(aligned_text_element(
                &TextPlacement::from(t),
                &color,
                &text_codes::decode(&t.text, false),
            ))
        }
        Entity::Attrib(a) => {
            ctx.consider(a.start_point.x, a.start_point.y);
            if let Some(p) = a.alignment_point {
                ctx.consider(p.x, p.y);
            }
            if a.text.is_empty() {
                return Some(String::new());
            }
            Some(aligned_text_element(
                &TextPlacement::from(a),
                &color,
                &text_codes::decode(&a.text, false),
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
            ))
        }
        Entity::MText(m) => {
            ctx.consider(m.insertion_point.x, m.insertion_point.y);
            let stripped = text_codes::decode(&m.text, true);
            let lines: Vec<&str> = stripped.lines().filter(|l| !l.is_empty()).collect();
            if lines.is_empty() {
                return Some(String::new());
            }
            // A stored 0 means "unset" at render time (the parsed value is
            // legitimately 0 in real files), not at parse time.
            let text_height = if m.text_height == 0.0 {
                1.0
            } else {
                m.text_height
            };
            let line_spacing_factor = if m.line_spacing_factor == 0.0 {
                1.0
            } else {
                m.line_spacing_factor
            };
            let line_height = text_height * line_spacing_factor * 1.2;
            let (x, y) = (m.insertion_point.x, neg(m.insertion_point.y));
            let (anchor, first_baseline) =
                mtext_placement(m.attachment, y, text_height, line_height, lines.len());
            let mut tspans = String::new();
            for (i, line) in lines.iter().enumerate() {
                let dy = if i == 0 { 0.0 } else { line_height };
                let _ = write!(
                    tspans,
                    "<tspan x=\"{x}\" dy=\"{dy}\">{}</tspan>",
                    escape_xml(line)
                );
            }
            Some(format!(
                "<text x=\"{x}\" y=\"{first_baseline}\" font-size=\"{text_height}\" text-anchor=\"{anchor}\" fill=\"{color}\" stroke=\"none\" transform=\"rotate({} {x} {y})\">{tspans}</text>",
                neg(m.rotation.to_degrees())
            ))
        }
        Entity::Point(p) => {
            ctx.consider(p.position.x, p.position.y);
            Some(format!(
                "<circle cx=\"{}\" cy=\"{}\" r=\"0.5\" fill=\"{color}\" stroke=\"none\"/>",
                p.position.x,
                neg(p.position.y)
            ))
        }
        Entity::Solid(s) | Entity::Trace(s) => {
            // Classic AutoCAD SOLID/TRACE vertex order is 1-2-4-3, not 1-2-3-4.
            let mut pts = [s.corner1, s.corner2, s.corner4, s.corner3];
            let plane = own_plane(s.extrusion);
            if !plane.is_world() {
                // Written in its own plane: each corner taken to the world.
                // A quadrilateral of straight edges stays one seen from
                // above, so this is exact on a tilted plane too.
                for p in &mut pts {
                    *p = seen_from_above(
                        plane,
                        Point3D {
                            x: p.x,
                            y: p.y,
                            z: s.elevation,
                        },
                    );
                }
            }
            ctx.consider_all(&pts);
            Some(format!(
                "<polygon points=\"{}\" fill=\"{color}\" fill-opacity=\"0.6\" stroke=\"none\"/>",
                points_attr(&pts)
            ))
        }
        Entity::Face3D(f) => {
            // Unlike SOLID, 3DFACE's 4 corners are already sequential.
            let pts = xy(&[f.corner1, f.corner2, f.corner3, f.corner4]);
            ctx.consider_all(&pts);
            if f.invisible_edges.iter().all(|hidden| !hidden) {
                return Some(format!(
                    "<polygon points=\"{}\" fill=\"none\" stroke=\"{color}\"/>",
                    points_attr(&pts)
                ));
            }
            // Only the edges the file does not hide: a mesh of faces shows
            // its outline, not the edges its faces share.
            let mut d = String::new();
            for (i, hidden) in f.invisible_edges.iter().enumerate() {
                if *hidden {
                    continue;
                }
                let (a, b) = (pts[i], pts[(i + 1) % 4]);
                if !d.is_empty() {
                    d.push(' ');
                }
                let _ = write!(
                    d,
                    "M {} {} L {} {}",
                    clean(a.x),
                    neg(a.y),
                    clean(b.x),
                    neg(b.y)
                );
            }
            if d.is_empty() {
                return Some(String::new());
            }
            Some(format!(
                "<path d=\"{d}\" fill=\"none\" stroke=\"{color}\"/>"
            ))
        }
        Entity::Ray(r) | Entity::XLine(r) => {
            let is_xline = matches!(e, Entity::XLine(_));
            ctx.consider(r.point.x, r.point.y);
            let len = 1e6;
            let (dx, dy) = (r.vector.x * len, r.vector.y * len);
            let (x1, y1) = if is_xline {
                (r.point.x - dx, r.point.y - dy)
            } else {
                (r.point.x, r.point.y)
            };
            let (x2, y2) = (r.point.x + dx, r.point.y + dy);
            Some(format!(
                "<line x1=\"{x1}\" y1=\"{}\" x2=\"{x2}\" y2=\"{}\" stroke-dasharray=\"4,2\" stroke=\"{color}\"/>",
                neg(y1), neg(y2)
            ))
        }
        Entity::Insert(i) => Some(render_block_ref(
            i.block_name.name(),
            Point2D {
                x: i.insertion_point.x,
                y: i.insertion_point.y,
            },
            i.scale.x,
            i.scale.y,
            i.rotation,
            &color,
            ctx,
        )),
        Entity::AcadTable(a) => Some(render_block_ref(
            a.block_name.name(),
            Point2D {
                x: a.insertion_point.x,
                y: a.insertion_point.y,
            },
            a.scale.x,
            a.scale.y,
            a.rotation,
            &color,
            ctx,
        )),
        Entity::Dimension(d) => {
            // The cached geometry block is already in final world coordinates,
            // so it is drawn with an identity transform.
            let svg = render_block_ref(
                d.block_name.name(),
                Point2D { x: 0.0, y: 0.0 },
                1.0,
                1.0,
                0.0,
                &color,
                ctx,
            );
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
            Some(dashed_outline(&corners, &color, "2,2"))
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
            Some(dashed_outline(&w.boundary, &color, "2,2"))
        }
        Entity::Spline(s) => {
            let pts = spline::spline_points(s);
            if pts.len() < 2 {
                return None;
            }
            ctx.consider_all(&pts);
            Some(polyline_element(&pts, false, &color))
        }
        Entity::Solid3D(s) => render_wireframe_entity(&s.wireframe_edges, "3DSOLID", &color, ctx),
        Entity::Region(r) => render_wireframe_entity(&r.wireframe_edges, "REGION", &color, ctx),
        Entity::PolylinePFace(p) => {
            render_wireframe_entity(&p.wireframe_edges, "POLYLINE_PFACE", &color, ctx)
        }
        Entity::Hatch(h) => {
            let plane = own_plane(h.extrusion);
            if plane.is_world() {
                hatch::render_hatch(h, &color, ctx)
            } else {
                // Everything a HATCH states -- boundary, pattern lines,
                // gradient -- is in its own plane, and that plane seen from
                // above is an affine map of the page: drawn in the plane and
                // placed by it, the whole fill stays exact.
                in_view(plane_seen_from_above(plane, h.elevation), ctx, |ctx| {
                    hatch::render_hatch(h, &color, ctx)
                })
            }
        }
        Entity::Leader(l) => {
            if l.vertices.is_empty() {
                return None;
            }
            ctx.consider_all_3d(&l.vertices);
            let pts = xy(&l.vertices);
            let line = polyline_element(&pts, false, &color);
            // A leader whose file does not state the flag draws none: an
            // arrowhead is a claim about the drawing, and nothing made it.
            let arrow = if l.has_arrowhead == Some(true) && pts.len() >= 2 {
                arrowhead_element(&pts[0], &pts[1], ARROWHEAD_SIZE, &color)
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
                parts.push(polyline_element(&pts, false, &color));
                let n = pts.len();
                parts.push(arrowhead_element(
                    &pts[n - 1],
                    &pts[n - 2],
                    ARROWHEAD_SIZE,
                    &color,
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
                    polyline_element(&mline_offset_points(&l.vertices, offset), l.closed, &color)
                })
                .collect();
            Some(lines.join("\n  "))
        }
        Entity::Light(l) => {
            ctx.consider(l.position.x, l.position.y);
            let marker = format!(
                "<circle cx=\"{}\" cy=\"{}\" r=\"0.5\" fill=\"none\" stroke=\"{color}\"/>",
                l.position.x,
                neg(l.position.y)
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
                l.position.x, neg(l.position.y), l.target.x, neg(l.target.y)
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

    let mut ctx = Ctx::new(&db.tables);
    for e in select_entities_for_space(db, options.space) {
        ctx.reset_entity_bounds();
        if let Some(svg) = render_entity(e, &mut ctx) {
            if !svg.is_empty() {
                body.push(svg);
            }
        }
        if let Some(b) = ctx.entity_box() {
            entity_boxes.push(b);
        }
    }

    let raw_bounds = || Box2D {
        min_x: ctx.xs.iter().cloned().fold(f64::INFINITY, f64::min),
        max_x: ctx.xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        min_y: ctx.ys.iter().cloned().fold(f64::INFINITY, f64::min),
        max_y: ctx.ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
    };
    let bounds = if ctx.xs.is_empty() {
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

    let x = bounds.min_x - options.padding;
    let y = -bounds.max_y - options.padding;
    let width = (bounds.max_x - bounds.min_x) + options.padding * 2.0;
    let height = (bounds.max_y - bounds.min_y) + options.padding * 2.0;
    let effective_stroke_width = options
        .stroke_width
        .unwrap_or_else(|| (width.hypot(height) / 6000.0).max(0.01));

    let resolved_body = resolve_stroke_widths(&body.join("\n  "), effective_stroke_width);
    // HATCH pattern defs carry stroke-width placeholders too. Kept separate
    // from the body only so an empty defs list emits no <defs> block at all.
    let defs_block = if ctx.defs.is_empty() {
        String::new()
    } else {
        let resolved_defs = resolve_stroke_widths(&ctx.defs.join("\n  "), effective_stroke_width);
        format!("<defs>\n  {resolved_defs}\n</defs>\n  ")
    };

    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{x} {y} {} {}\" stroke=\"black\" stroke-width=\"{effective_stroke_width}\">\n  {defs_block}{resolved_body}\n</svg>",
        if width != 0.0 { width } else { 1.0 },
        if height != 0.0 { height } else { 1.0 }
    );

    ToSvgResult {
        svg,
        unsupported_types: ctx.unsupported.into_iter().collect(),
        empty_blocks: ctx.empty_blocks.into_iter().collect(),
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
        let svg = render_block_ref(
            "R",
            Point2D { x: 0.0, y: 0.0 },
            1.0,
            1.0,
            0.0,
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
        let [a, b, c, d, e, f] = svg_matrix(&t);
        let (x, y) = (a * 1.0 + c * 0.0 + e, b * 1.0 + d * 0.0 + f);
        close(x, 10.0);
        close(y, -22.0);
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

    fn polyline(vertices: &[(f64, f64, f64)], closed: bool) -> Entity {
        Entity::LwPolyline(LwPolylineEntity {
            common: plain_common(),
            vertices: vertices
                .iter()
                .map(|&(x, y, bulge)| PolylineVertex {
                    point: Point2D { x, y },
                    bulge,
                })
                .collect(),
            closed,
            elevation: 0.0,
            extrusion: Point3D {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            },
        })
    }

    /// The path's commands, each with its numbers.
    fn path_commands(svg: &str) -> Vec<(String, Vec<f64>)> {
        let d = svg
            .split("d=\"")
            .nth(1)
            .expect("a path")
            .split('"')
            .next()
            .unwrap();
        let mut out: Vec<(String, Vec<f64>)> = Vec::new();
        for t in d.split_whitespace() {
            match t.parse::<f64>() {
                Ok(n) => out.last_mut().expect("a command first").1.push(n),
                Err(_) => out.push((t.to_string(), Vec::new())),
            }
        }
        out
    }

    fn view_box(svg: &str) -> [f64; 4] {
        let v: Vec<f64> = svg
            .split("viewBox=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .split_whitespace()
            .map(|t| t.parse().unwrap())
            .collect();
        [v[0], v[1], v[2], v[3]]
    }

    #[test]
    fn a_straight_polyline_is_still_a_polyline() {
        let svg = render_one(polyline(&[(0.0, 0.0, 0.0), (2.0, 0.0, 0.0)], false));
        assert!(svg.contains("<polyline") && !svg.contains("<path"), "{svg}");
    }

    #[test]
    fn a_bulged_segment_is_an_exact_arc_turning_the_way_the_bulge_says() {
        // Bulge 1 from (0, 0) to (2, 0): a half circle of radius 1,
        // counter-clockwise in the drawing -- which the page's flipped y axis
        // turns clockwise, so SVG's sweep flag is 0. Bulge -1 is the mirror.
        for (bulge, sweep) in [(1.0, 0.0), (-1.0, 1.0)] {
            let svg = render_one(polyline(&[(0.0, 0.0, bulge), (2.0, 0.0, 0.0)], false));
            let cmds = path_commands(&svg);
            assert_eq!(cmds[0], ("M".to_string(), vec![0.0, 0.0]), "{svg}");
            assert_eq!(
                cmds[1],
                ("A".to_string(), vec![1.0, 1.0, 0.0, 0.0, sweep, 2.0, 0.0]),
                "{svg}"
            );
        }
    }

    #[test]
    fn the_view_box_reaches_the_arc_not_just_its_ends() {
        // The half circle below the chord reaches y = -1 in the drawing, y = 1
        // on the page; a view box of the two ends alone would be flat.
        let svg = render_one(polyline(&[(0.0, 0.0, 1.0), (2.0, 0.0, 0.0)], false));
        let [_, y, _, h] = view_box(&svg);
        assert!(y <= 0.0 && y + h >= 1.0, "{svg}");
    }

    #[test]
    fn a_closed_polyline_draws_its_last_bulge_back_to_the_first_vertex() {
        let svg = render_one(polyline(
            &[(0.0, 0.0, 0.0), (2.0, 0.0, 0.0), (2.0, 2.0, -1.0)],
            true,
        ));
        let cmds = path_commands(&svg);
        let names: Vec<&str> = cmds.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(names, ["M", "L", "L", "A", "Z"], "{svg}");
        // The closing arc ends where the polyline began.
        assert_eq!(cmds[3].1[5..], [0.0, 0.0], "{svg}");
    }

    fn mirrored() -> Point3D {
        Point3D {
            x: 0.0,
            y: 0.0,
            z: -1.0,
        }
    }

    #[test]
    fn a_mirrored_solid_has_its_corners_taken_to_the_world() {
        use uncad_model::model::SolidEntity;
        let c = |x, y| Point2D { x, y };
        let svg = render_one(Entity::Solid(SolidEntity {
            common: plain_common(),
            corner1: c(-1.0, 0.0),
            corner2: c(-3.0, 0.0),
            corner3: c(-1.0, 2.0),
            corner4: c(-3.0, 2.0),
            elevation: 5.0,
            extrusion: mirrored(),
        }));
        // 1-2-4-3, each x reversed; the page flips y.
        assert!(svg.contains("points=\"1,0 3,0 3,-2 1,-2\""), "{svg}");
    }

    fn square_hatch(elevation: f64, extrusion: Point3D) -> Entity {
        use uncad_model::model::{HatchBoundaryPath, HatchEntity};
        Entity::Hatch(HatchEntity {
            common: plain_common(),
            boundary_paths: vec![HatchBoundaryPath::Polyline(
                [(1.0, 0.0), (3.0, 0.0), (3.0, 2.0), (1.0, 2.0)]
                    .iter()
                    .map(|&(x, y)| PolylineVertex::straight(Point2D { x, y }))
                    .collect(),
            )],
            solid_fill: true,
            gradient: None,
            pattern_lines: Vec::new(),
            elevation,
            extrusion,
        })
    }

    #[test]
    fn a_mirrored_hatch_is_drawn_in_its_plane_and_placed_by_it() {
        let svg = render_one(square_hatch(5.0, mirrored()));
        // The boundary as the file wrote it, inside the mirror (world x
        // reversed; the page flips y, which leaves a mirror in x as it is).
        assert!(
            svg.contains("<g transform=\"matrix(-1 0 0 1 0 0)\">"),
            "{svg}"
        );
        assert!(svg.contains("M 1 0 L 3 0 L 3 -2 L 1 -2 Z"), "{svg}");
        // The extent is the world's, x from -3 to -1: the view is centred
        // on -2, not on the 2 the unmirrored boundary would give.
        let [x, _, w, _] = view_box(&svg);
        close(x + w / 2.0, -2.0);
    }

    #[test]
    fn a_hatch_on_a_tilted_plane_is_seen_from_above() {
        // Tilted 45 degrees about the world x axis: the arbitrary axis
        // algorithm keeps x, and y is foreshortened by cos 45.
        let h = std::f64::consts::FRAC_1_SQRT_2;
        let svg = render_one(square_hatch(
            0.0,
            Point3D {
                x: 0.0,
                y: -h,
                z: h,
            },
        ));
        assert!(svg.contains("<g transform=\"matrix(1 "), "{svg}");
        let [_, _, w, height] = view_box(&svg);
        // Width 2 and height 2 cos 45, each with the same padding.
        close(w - height, 2.0 - 2.0 * h);
    }

    #[test]
    fn a_hatch_in_the_world_plane_is_not_wrapped() {
        let svg = render_one(square_hatch(
            0.0,
            Point3D {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            },
        ));
        assert!(!svg.contains("<g transform"), "{svg}");
    }

    fn face(invisible_edges: [bool; 4]) -> Entity {
        use uncad_model::model::Face3DEntity;
        let c = |x, y| Point3D { x, y, z: 0.0 };
        Entity::Face3D(Face3DEntity {
            common: plain_common(),
            corner1: c(0.0, 0.0),
            corner2: c(4.0, 0.0),
            corner3: c(4.0, 3.0),
            corner4: c(0.0, 3.0),
            invisible_edges,
        })
    }

    #[test]
    fn a_face_draws_only_the_edges_its_file_does_not_hide() {
        // All visible: the closed outline, as before.
        assert!(render_one(face([false; 4])).contains("<polygon"));
        // Second edge (corner 2 to corner 3) hidden: three separate edges.
        let svg = render_one(face([false, true, false, false]));
        let cmds = path_commands(&svg);
        let names: Vec<&str> = cmds.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(names, ["M", "L", "M", "L", "M", "L"], "{svg}");
        // Each edge is one M-L pair; the hidden one, x = 4 from y = 0 to 3
        // (page y 0 to -3), is not among them.
        let edges: Vec<(&[f64], &[f64])> = cmds
            .chunks(2)
            .map(|pair| (pair[0].1.as_slice(), pair[1].1.as_slice()))
            .collect();
        assert_eq!(edges.len(), 3, "{svg}");
        assert!(
            !edges.contains(&([4.0, 0.0].as_slice(), [4.0, -3.0].as_slice())),
            "{svg}"
        );
        // Every edge hidden: nothing drawn.
        assert!(!render_one(face([true; 4])).contains("<path"));
    }

    #[test]
    fn a_mirrored_polyline_has_its_vertices_and_arcs_taken_to_the_world() {
        // Written at (-2, 0) -> (0, 0) with bulge 1 (counter-clockwise in its
        // own system, so below that chord) in a mirror copy's system: in the
        // world it runs (2, 0) -> (0, 0) and the half circle still lies below
        // -- the arc turns the other way, so the page's sweep flag is 1.
        let mut e = polyline(&[(-2.0, 0.0, 1.0), (0.0, 0.0, 0.0)], false);
        if let Entity::LwPolyline(p) = &mut e {
            p.extrusion = mirrored();
        }
        let svg = render_one(e);
        let cmds = path_commands(&svg);
        assert_eq!(cmds[0], ("M".to_string(), vec![2.0, 0.0]), "{svg}");
        assert_eq!(
            cmds[1],
            ("A".to_string(), vec![1.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
            "{svg}"
        );
        let [_, y, _, h] = view_box(&svg);
        assert!(y <= 0.0 && y + h >= 1.0, "the arc reaches y = -1: {svg}");
    }

    #[test]
    fn a_mirrored_circle_is_drawn_at_its_center_taken_to_the_world() {
        let svg = render_one(Entity::Circle(CircleEntity {
            common: plain_common(),
            center: Point3D {
                x: -170.0,
                y: -50.0,
                z: 0.0,
            },
            radius: 3.0,
            extrusion: mirrored(),
        }));
        assert!(
            svg.contains("<circle cx=\"170\" cy=\"50\" r=\"3\""),
            "{svg}"
        );
    }

    #[test]
    fn a_mirrored_arc_runs_clockwise_in_the_world() {
        // Written about (-110, -50) from 30 to 150 degrees, counter-clockwise
        // about (0, 0, -1): in the world it is about (110, -50), from
        // (106.54, -48) over the top to (113.46, -48) -- clockwise.
        let svg = render_one(Entity::Arc(ArcEntity {
            common: plain_common(),
            center: Point3D {
                x: -110.0,
                y: -50.0,
                z: 0.0,
            },
            radius: 4.0,
            start_angle: 30f64.to_radians(),
            end_angle: 150f64.to_radians(),
            extrusion: mirrored(),
        }));
        let cmds = path_commands(&svg);
        let (m, a) = (&cmds[0].1, &cmds[1].1);
        let near = |p: f64, q: f64| (p - q).abs() < 1e-9;
        assert!(
            near(m[0], 110.0 - 12f64.sqrt()) && near(m[1], 48.0),
            "{svg}"
        );
        // r r rotation large sweep x y: the page's sweep flag 1.
        assert_eq!(a[..5], [4.0, 4.0, 0.0, 0.0, 1.0], "{svg}");
        assert!(
            near(a[5], 110.0 + 12f64.sqrt()) && near(a[6], 48.0),
            "{svg}"
        );
    }

    #[test]
    fn a_circle_on_a_tilted_plane_is_drawn_as_seen_from_above() {
        // Extrusion along the world X axis: the circle stands on edge, and
        // from above it is a line along the world y axis through (0, 2).
        let svg = render_one(Entity::Circle(CircleEntity {
            common: plain_common(),
            center: Point3D {
                x: 2.0,
                y: 0.0,
                z: 0.0,
            },
            radius: 1.0,
            extrusion: Point3D {
                x: 1.0,
                y: 0.0,
                z: 0.0,
            },
        }));
        assert!(svg.contains("<polygon"), "{svg}");
        let points = svg
            .split("points=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        for pair in points.split_whitespace() {
            let (x, y) = pair.split_once(',').unwrap();
            let (x, y): (f64, f64) = (x.parse().unwrap(), y.parse().unwrap());
            assert!(x.abs() < 1e-9, "{pair}");
            assert!((-y - 2.0).abs() <= 1.0 + 1e-9, "{pair}");
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

    fn text_entity(
        h: TextHorizontalAlignment,
        v: TextVerticalAlignment,
        alignment_point: Option<Point2D>,
        width_factor: f64,
    ) -> Entity {
        Entity::Text(TextEntity {
            common: plain_common(),
            start_point: Point2D { x: 1.0, y: 2.0 },
            text_height: 2.5,
            text: "A-1".to_string(),
            rotation: 0.0,
            horizontal_alignment: h,
            vertical_alignment: v,
            alignment_point,
            width_factor,
        })
    }

    #[test]
    fn a_left_baseline_text_is_drawn_from_its_start_point_as_before() {
        use TextHorizontalAlignment as H;
        use TextVerticalAlignment as V;
        let svg = render_one(text_entity(H::Left, V::Baseline, None, 1.0));
        assert!(
            svg.contains(
                "<text x=\"1\" y=\"-2\" font-size=\"2.5\" fill=\"#000000\" stroke=\"none\">A-1</text>"
            ),
            "{svg}"
        );
    }

    #[test]
    fn a_centered_text_is_anchored_on_its_alignment_point() {
        use TextHorizontalAlignment as H;
        use TextVerticalAlignment as V;
        let at = Some(Point2D { x: 10.0, y: 4.0 });
        let svg = render_one(text_entity(H::Center, V::Middle, at, 1.0));
        assert!(
            svg.contains("x=\"10\" y=\"-4\"")
                && svg.contains("text-anchor=\"middle\" dominant-baseline=\"central\""),
            "{svg}"
        );
        let svg = render_one(text_entity(H::Right, V::Top, at, 1.0));
        assert!(
            svg.contains("text-anchor=\"end\" dominant-baseline=\"text-before-edge\""),
            "{svg}"
        );
        // An alignment the file gives no point for is drawn from the start.
        let svg = render_one(text_entity(H::Center, V::Baseline, None, 1.0));
        assert!(
            svg.contains("x=\"1\" y=\"-2\"") && !svg.contains("text-anchor"),
            "{svg}"
        );
    }

    #[test]
    fn an_aligned_text_runs_from_its_start_to_its_alignment_point() {
        use TextHorizontalAlignment as H;
        use TextVerticalAlignment as V;
        // From (1, 2) to (1, 7): 5 long, straight up -- a quarter turn.
        let at = Some(Point2D { x: 1.0, y: 7.0 });
        let svg = render_one(text_entity(H::Aligned, V::Baseline, at, 1.0));
        assert!(
            svg.contains(
                "textLength=\"5\" lengthAdjust=\"spacingAndGlyphs\" transform=\"rotate(-90 1 -2)\""
            ),
            "{svg}"
        );
    }

    #[test]
    fn an_attribute_value_is_placed_by_its_alignment_too() {
        let svg = render_one(Entity::Attrib(AttribEntity {
            common: plain_common(),
            start_point: Point2D { x: 1.0, y: 2.0 },
            text_height: 2.5,
            tag: "DWGNO".to_string(),
            text: "BP-1042".to_string(),
            rotation: 0.0,
            horizontal_alignment: TextHorizontalAlignment::Right,
            vertical_alignment: TextVerticalAlignment::Middle,
            alignment_point: Some(Point2D { x: 40.0, y: 3.0 }),
            width_factor: 0.9,
        }));
        assert!(
            svg.contains("x=\"40\" y=\"-3\"")
                && svg.contains("text-anchor=\"end\" dominant-baseline=\"central\"")
                && svg.contains("scale(0.9 1)"),
            "{svg}"
        );
    }

    #[test]
    fn a_width_factor_narrows_the_text_about_where_it_is_placed() {
        use TextHorizontalAlignment as H;
        use TextVerticalAlignment as V;
        let svg = render_one(text_entity(H::Left, V::Baseline, None, 0.8));
        assert!(
            svg.contains("transform=\"translate(1 -2) scale(0.8 1) translate(-1 2)\""),
            "{svg}"
        );
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
