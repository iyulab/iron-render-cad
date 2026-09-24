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
//! rendering context, per-entity rendering, then the top level -- `render`
//! (walk and measure, into a [`Scene`]) and [`to_svg`], which assembles the
//! scene's whole document ([`scene`] resolves the placeholders).
//! Submodules hold the parts that stand on their own -- [`format`] (number and
//! string formatting), [`scene`] (a render kept, and documents written from
//! it), [`hatch`] (HATCH fills), [`spline`] (SPLINE curves),
//! [`infinite`] (RAY and XLINE, cut to the picture once the viewBox is
//! known), [`ocs`] (the plane a planar entity is written in),
//! [`bulge`] (the arcs a polyline's bulges describe), [`text_codes`] (what
//! a text's control codes stand for), [`justify`] (where a single-line text
//! hangs and the box it fills), [`bounds`] (boxes and the cluster trim) and
//! [`crop`] (the rectangle the picture shows, and what it leaves out).

mod bounds;
mod crop;
mod format;
mod hatch;
mod infinite;
mod justify;
mod scene;
mod sheet;
mod spline;
mod text_codes;
mod visibility;

pub use crop::{Crop, CropReport, LeftOut, LeftOutReason};
pub use scene::{Part, Rect, Scene, TextBox};
pub(crate) use sheet::render_layout;
pub use sheet::{LayoutError, SheetSource, ViewportReport};
pub use visibility::Hidden;

use crate::color::{effective_layer, resolve_color, DEFAULT_COLOR};
use crate::limits::{
    Cap, LimitReport, MAX_BLOCK_REFS, MAX_BLOCK_REF_DEPTH, MAX_ENTITY_POINTS, MAX_ENTITY_SVG_BYTES,
    MAX_SVG_BODY_BYTES, MAX_WORLD_COORDINATE,
};
use bounds::Box2D;
use format::{clean, escape_xml, neg, xy, Frame};
use justify::{Anchor, MTextBlock, TextLayout};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use uncad_model::bulge::{self, BulgeArc, Segment};
use uncad_model::model::{
    ArcEntity, CircleEntity, EllipseEntity, Entity, EntityCommon, EntityId, HatchBoundaryPath,
    HatchEdge, LightType, LwPolylineEntity, MLineVertex, MTextAttachment, Point2D, Point3D,
    PolylineVertex, Ref,
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
    /// How the rectangle the picture shows is chosen from the extents the
    /// entities measured -- see [`Crop`]. Default [`Crop::Cluster`]; what it
    /// leaves out is in [`ToSvgResult::crop`].
    pub crop: Crop,
    /// How tall a capital letter is in the face the text will be drawn with,
    /// as a fraction of its em (the font's OS/2 `sCapHeight` over its units
    /// per em). A CAD text height is the height of the capitals, so a text of
    /// height `h` is written at `font-size = h / cap_height`, and its
    /// capitals come out `h` tall in that face. Default
    /// [`DEFAULT_CAP_HEIGHT`]; a value that is not a positive number is
    /// taken as the default.
    pub cap_height: f64,
    /// Draw the entities the drawing hides at half opacity instead of
    /// leaving them out: an entity marked invisible (DXF 60), an attribute
    /// whose own invisible flag is set (DXF 70), anything on the
    /// `DEFPOINTS` layer, and anything on a layer that is off, frozen or
    /// stated not to plot. Default `false`. Either way they do not count
    /// towards the extent, and [`ToSvgResult::hidden`] counts them.
    pub include_hidden: bool,
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
            crop: Crop::Cluster,
            cap_height: DEFAULT_CAP_HEIGHT,
            include_hidden: false,
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
    /// How many times the render met an entity the drawing hides (see
    /// [`ToSvgOptions::include_hidden`]), block contents included: a hidden
    /// entity in a block referenced twice counts twice, and the contents of
    /// a hidden block reference are not visited at all. Left out of the
    /// picture, or drawn faded when asked, and never part of the extent. In
    /// a layout's sheet ([`layout_to_svg`]) the model is walked once per
    /// viewport that shows it, and an entity on a layer frozen in that
    /// viewport alone counts too.
    pub hidden: usize,
    /// For a layout's sheet ([`layout_to_svg`]): the viewports that are on
    /// and are windows onto the model, but whose view this renderer does
    /// not draw through them -- one the file does not state (a viewport
    /// older than R2000 keeps it where the model does not read it), one of
    /// no positive height or frame, or one not looking straight down the z
    /// axis (a 3D view). Each one's frame is drawn and nothing is shown in
    /// it. By reference ID, sorted. Always empty for [`to_svg`].
    pub undrawn_viewports: Vec<EntityId>,
    /// The world point the SVG's coordinates are written relative to: an
    /// SVG user unit at `(u, v)` is the world point `(origin.x + u,
    /// origin.y - v)`, the viewBox included. `(0, 0)` -- the SVG reads in
    /// world units, y flipped -- unless the drawing lies more than 32768
    /// units from the world origin: the rasterizer keeps coordinates in
    /// `f32`, which at 2.5e8 cannot tell two points 16 units apart, so a
    /// far-away drawing is written about a whole-unit point near its own
    /// middle instead.
    pub origin: Point2D,
    /// The world rectangle the document shows -- its `viewBox`, padding
    /// included, in drawing units with y up rather than as the document
    /// writes it (relative to [`origin`](Self::origin), y down). For a
    /// layout's sheet, the paper in the layout's paper units.
    pub view_box: Rect,
    /// How [`view_box`](Self::view_box) was chosen ([`ToSvgOptions::crop`])
    /// and which top-level entities the picture does not show.
    pub crop: CropReport,
    /// For a layout's sheet ([`layout_to_svg`]): every viewport of the
    /// layout, in the order its paper space lists them -- the overall one,
    /// ones that are off and ones on hidden layers included -- with what
    /// each shows of the model. Always empty for [`to_svg`].
    pub viewports: Vec<ViewportReport>,
    /// For a layout's sheet: where the sheet the viewBox frames comes
    /// from, or `None` when the layout states no sheet and is framed like
    /// a render of its paper space. Always `None` for [`to_svg`].
    pub sheet: Option<SheetSource>,
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

/// The frame the interior of a group placed by `placement` is written in,
/// under a parent written in `parent`. For a drawing near the origin (the
/// parent frame is the default) it is (0, 0): the interior is written in
/// its own coordinates and the placement goes into the group's matrix.
/// When a render origin is in use that is wrong -- a DIMENSION's block is
/// placed through an identity precisely because its children already hold
/// world coordinates, and they would be written at full world magnitude,
/// where the rasterizer's f32 quantizes them away. So the interior is then
/// written about the point this placement sends to the parent frame's
/// origin: every number stays near zero, and the group's own translation
/// is zero.
fn child_frame(placement: &Affine2, parent: Frame) -> Frame {
    if parent == Frame::default() {
        return Frame::default();
    }
    match invert_point(
        placement,
        Point2D {
            x: parent.ox,
            y: parent.oy,
        },
    ) {
        Some(o) => Frame { ox: o.x, oy: o.y },
        // A singular placement flattens the block whatever the frame.
        None => Frame::default(),
    }
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
    /// [`ToSvgOptions::include_hidden`].
    include_hidden: bool,
    /// The layers frozen in the viewport being drawn through (see
    /// [`sheet`]); empty everywhere else.
    viewport_frozen: BTreeSet<String>,
    /// Entities [`render_entity`] found hidden (see
    /// [`ToSvgResult::hidden`]).
    hidden: usize,
    /// The reference IDs of the block references the walk is inside,
    /// outermost first -- on a sheet, after the viewport's own: what a
    /// text's path ([`TextBox::path`]) starts with.
    id_path: Vec<EntityId>,
    /// Every text drawn so far, in drawing order (see [`Scene::text_boxes`]).
    texts: Vec<scene::DrawnText>,
}

impl<'a> Ctx<'a> {
    /// A context for a render written about `origin`, set up as `options`
    /// say.
    fn configured(tables: &'a Tables, options: &ToSvgOptions, origin: Point2D) -> Self {
        let mut ctx = Ctx::new(tables);
        ctx.frame = Frame {
            ox: origin.x,
            oy: origin.y,
        };
        if options.cap_height.is_finite() && options.cap_height > 0.0 {
            ctx.cap_height = options.cap_height;
        }
        ctx.include_hidden = options.include_hidden;
        ctx
    }

    /// The scene this context has walked: `walked` its parts in drawing
    /// order, each with the elements it drew, framed by `view_box`.
    fn finish(
        self,
        walked: Vec<(Part, String)>,
        view_box: ViewBox,
        origin: Point2D,
        crop: CropReport,
    ) -> Scene {
        let (parts, body) = walked.into_iter().unzip();
        Scene {
            parts,
            body,
            defs: self.defs,
            doc_view_box: view_box.rect,
            view_box: scene::world_rect(view_box.rect, origin),
            origin,
            auto_stroke_width: view_box.auto_stroke_width,
            unsupported_types: self.unsupported.into_iter().collect(),
            empty_blocks: self.empty_blocks.into_iter().collect(),
            unresolved_block_refs: self.unresolved_block_refs.into_iter().collect(),
            limits: self.limits,
            hidden: self.hidden,
            undrawn_viewports: Vec::new(),
            crop,
            viewports: Vec::new(),
            sheet: None,
            texts: self.texts,
        }
    }

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
            include_hidden: false,
            viewport_frozen: BTreeSet::new(),
            hidden: 0,
            id_path: Vec::new(),
            texts: Vec::new(),
        }
    }

    fn reset_entity_bounds(&mut self) {
        self.ent_min_x = f64::INFINITY;
        self.ent_max_x = f64::NEG_INFINITY;
        self.ent_min_y = f64::INFINITY;
        self.ent_max_y = f64::NEG_INFINITY;
    }

    /// The running bounds, to be put back with
    /// [`set_bounds`](Self::set_bounds).
    fn bounds(&self) -> [f64; 4] {
        [
            self.ent_min_x,
            self.ent_max_x,
            self.ent_min_y,
            self.ent_max_y,
        ]
    }

    fn set_bounds(&mut self, [min_x, max_x, min_y, max_y]: [f64; 4]) {
        self.ent_min_x = min_x;
        self.ent_max_x = max_x;
        self.ent_min_y = min_y;
        self.ent_max_y = max_y;
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

    /// The world box of *local* `points`, taken through the current
    /// transform like [`consider`](Self::consider) takes them; a point the
    /// transform sends past what a number holds is left out. `None` when
    /// none is left.
    fn world_box(&self, points: &[Point2D]) -> Option<Box2D> {
        let mut b: Option<Box2D> = None;
        for p in points {
            let Point2D { x, y } = self.transform.apply(*p);
            if !x.is_finite() || !y.is_finite() {
                continue;
            }
            let grown = match b {
                None => Box2D {
                    min_x: x,
                    max_x: x,
                    min_y: y,
                    max_y: y,
                },
                Some(b) => Box2D {
                    min_x: b.min_x.min(x),
                    max_x: b.max_x.max(x),
                    min_y: b.min_y.min(y),
                    max_y: b.max_y.max(y),
                },
            };
            b = Some(grown);
        }
        b
    }

    /// Records a text the entity `id` draws here -- showing `text`, its
    /// estimated world box `estimate` -- and returns the `id` attribute its
    /// `<text>` carries: see [`scene::text_id`].
    fn record_text(&mut self, id: EntityId, text: &str, estimate: Option<Box2D>) -> String {
        let mut path = self.id_path.clone();
        path.push(id);
        let attribute = scene::text_id(&path);
        self.texts.push(scene::DrawnText {
            path,
            text: text.to_string(),
            estimate,
        });
        attribute
    }

    /// How far a text's descenders reach below its baseline, in text
    /// heights: [`justify::DESCENDER_EM`] in the em the text is drawn at.
    fn descender(&self) -> f64 {
        justify::DESCENDER_EM / self.cap_height
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

/// Points sampled around a full turn of a circle drawn in a plane that is
/// not the world's, seen from above.
const OWN_PLANE_SAMPLES: usize = 64;

/// The coordinate system a planar entity is written in. An extrusion that
/// names no plane (zero, or not finite) is drawn in the world's.
fn own_plane(extrusion: Point3D) -> Ocs {
    Ocs::of(extrusion).unwrap_or(Ocs::WORLD)
}

/// Whether an entity stated in the plane of `extrusion` at height `z` has
/// real numbers for everything its plane is drawn from: the extrusion
/// always, and the height wherever the plane is not the world's own. A
/// mirrored or tilted plane is taken to the world through arithmetic that
/// takes the height in -- even where it moves nothing in plan, `0 * NaN` is
/// `NaN`. In the world's own plane the height is not used.
fn plane_numbers_are_real(extrusion: &Point3D, z: f64) -> bool {
    [extrusion.x, extrusion.y, extrusion.z]
        .iter()
        .all(|v| v.is_finite())
        && (z.is_finite() || Ocs::of(*extrusion).is_none_or(Ocs::is_world))
}

/// Whether a bulge's arc can be drawn as an arc at all. A bulge of 1e-160
/// over a hundred-unit segment is an arc of radius 1e238: handed to the
/// rasterizer as an arc, the arc-to-curve conversion works at a scale that
/// has nothing to do with the segment's (a fuzzed drawing with one such
/// vertex had not finished rasterizing after five minutes), and a point
/// sampled on it is reckoned from a center 1e238 away, which no `f64`
/// resolves to the segment. An arc that flat is drawn as its chord.
pub(super) fn arc_drawable(arc: &BulgeArc) -> bool {
    arc.radius.is_finite() && arc.radius < MAX_WORLD_COORDINATE
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
/// as well, and the group is written in the frame [`child_frame`] chooses,
/// the way a block reference records and writes its contents.
fn in_view(
    view: Affine2,
    ctx: &mut Ctx,
    draw: impl FnOnce(&mut Ctx) -> Option<String>,
) -> Option<String> {
    let parent = ctx.transform;
    let parent_frame = ctx.frame;
    let frame = child_frame(&view, parent_frame);
    let matrix = svg_matrix(&view, parent_frame, frame);
    let parent_svg_matrix = ctx.svg_matrix;
    ctx.transform = view.then(&parent);
    ctx.frame = frame;
    ctx.svg_matrix = infinite::compose(parent_svg_matrix, matrix);
    let body = draw(ctx);
    ctx.transform = parent;
    ctx.frame = parent_frame;
    ctx.svg_matrix = parent_svg_matrix;
    let [a, b, c, d, e, f] = matrix;
    body.map(|body| {
        format!(
            "<g transform=\"matrix({a} {b} {c} {d} {e} {f})\">
  {body}
</g>"
        )
    })
}

/// What `draw` emits in the coordinates of the plane `extrusion` names, at
/// height `elevation`: as it is in the world's own plane, and otherwise
/// wrapped in a group that takes that plane to the page seen from above.
/// Exact for anything drawn in the plane -- a text's glyphs included, which a
/// mirror copy's plane shows reversed.
fn in_own_plane(
    extrusion: Point3D,
    elevation: f64,
    ctx: &mut Ctx,
    draw: impl FnOnce(&mut Ctx) -> Option<String>,
) -> Option<String> {
    let plane = own_plane(extrusion);
    if plane.is_world() {
        draw(ctx)
    } else {
        in_view(plane_seen_from_above(plane, elevation), ctx, draw)
    }
}

/// Where the point `p` at height `z` of the plane of `extrusion` is in the
/// world, seen from above. In the world's own plane that is `p`, whatever
/// the height.
fn in_world(extrusion: Point3D, p: Point2D, z: f64) -> Point2D {
    let plane = own_plane(extrusion);
    if plane.is_world() {
        return p;
    }
    seen_from_above(plane, Point3D { x: p.x, y: p.y, z })
}

/// A CIRCLE whose extrusion is not the world Z axis. Facing down (a mirror
/// copy) it is still a circle, at its center taken to the world; on a tilted
/// plane it is seen from above, so it is drawn through points of its outline.
fn circle_in_own_plane(c: &CircleEntity, color: &str, ctx: &mut Ctx) -> String {
    let plane = own_plane(c.extrusion);
    let frame = ctx.frame;
    if plane.is_flat() {
        let center = seen_from_above(plane, c.center);
        ctx.consider_box(&Box2D {
            min_x: center.x - c.radius,
            max_x: center.x + c.radius,
            min_y: center.y - c.radius,
            max_y: center.y + c.radius,
        });
        return format!(
            "<circle cx=\"{}\" cy=\"{}\" r=\"{}\" fill=\"none\" stroke=\"{color}\"/>",
            frame.x(center.x),
            frame.y(center.y),
            clean(c.radius)
        );
    }
    let points: Vec<Point2D> = (0..OWN_PLANE_SAMPLES)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / OWN_PLANE_SAMPLES as f64;
            seen_from_above(plane, on_circle(c.center, c.radius, a))
        })
        .collect();
    ctx.consider_all(&points);
    polyline_element(&points, true, color, frame)
}

/// An ARC whose extrusion is not the world Z axis. Its angles run
/// counter-clockwise about the extrusion, so facing down (a mirror copy) it
/// runs clockwise in the world; on a tilted plane it is seen from above and
/// drawn through points of its outline.
///
/// Two angles a whole turn apart name the same direction, so the sweep is
/// taken within one turn: a stored angle of any size makes no longer an
/// outline, and no arc that goes round more than once.
fn arc_in_own_plane(a: &ArcEntity, color: &str, ctx: &mut Ctx) -> String {
    let plane = own_plane(a.extrusion);
    let frame = ctx.frame;
    let sweep = (a.end_angle - a.start_angle).rem_euclid(std::f64::consts::TAU);
    let start = seen_from_above(plane, on_circle(a.center, a.radius, a.start_angle));
    let end = seen_from_above(plane, on_circle(a.center, a.radius, a.end_angle));
    if plane.is_flat() {
        let center = seen_from_above(plane, a.center);
        // The arc's own box, not the whole circle's. Mirrored, the arc runs
        // clockwise from the mirror image of its start -- counter-clockwise
        // from the mirror image of its end, `pi - end`.
        ctx.consider_box(&arc_extent(
            center,
            a.radius,
            std::f64::consts::PI - (a.start_angle + sweep),
            sweep,
        ));
        let r = clean(a.radius);
        let large = u8::from(sweep > std::f64::consts::PI);
        // Clockwise in the world, which the page's flipped y axis turns
        // counter-clockwise: SVG's sweep flag 1.
        return format!(
            "<path d=\"M {} {} A {r} {r} 0 {large} 1 {} {}\" fill=\"none\" stroke=\"{color}\"/>",
            frame.x(start.x),
            frame.y(start.y),
            frame.x(end.x),
            frame.y(end.y)
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
    polyline_element(&points, false, color, frame)
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
        return polyline_element(&points, closed, color, ctx.frame);
    }
    bulged_polyline_element(vertices, closed, color, ctx)
}

/// A polyline on a tilted plane, seen from above: its arcs sampled in its
/// own coordinate system, then every point taken to the world. An arc too
/// flat to draw (see [`arc_drawable`]) is its chord.
fn polyline_on_tilted_plane(
    p: &LwPolylineEntity,
    plane: Ocs,
    color: &str,
    ctx: &mut Ctx,
) -> String {
    let mut own: Vec<Point2D> = Vec::new();
    for Segment { from, arc, .. } in bulge::segments(&p.vertices, p.closed) {
        own.push(from);
        if let Some(arc) = arc.filter(arc_drawable) {
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
    polyline_element(&world, p.closed, color, ctx.frame)
}

/// A polyline with arc segments, as a `<path>`: a straight segment is a line,
/// a bulged one an exact SVG arc. An arc reaches past its two ends, so each
/// one's own box -- its ends and the extreme points it passes -- joins the
/// bounds, through all four corners so that it still contains the arc under
/// a rotated block placement. An arc too flat to draw (see
/// [`arc_drawable`]) is its chord.
fn bulged_polyline_element(
    vertices: &[PolylineVertex],
    closed: bool,
    color: &str,
    ctx: &mut Ctx,
) -> String {
    let frame = ctx.frame;
    let first = vertices[0].point;
    let mut d = format!("M {} {}", frame.x(first.x), frame.y(first.y));
    for Segment { from, to, arc } in bulge::segments(vertices, closed) {
        match arc.filter(arc_drawable) {
            Some(arc) => {
                let mut b = Box2D {
                    min_x: from.x.min(to.x),
                    max_x: from.x.max(to.x),
                    min_y: from.y.min(to.y),
                    max_y: from.y.max(to.y),
                };
                for e in arc.extremes() {
                    b.min_x = b.min_x.min(e.x);
                    b.max_x = b.max_x.max(e.x);
                    b.min_y = b.min_y.min(e.y);
                    b.max_y = b.max_y.max(e.y);
                }
                ctx.consider_box(&b);
                let r = clean(arc.radius);
                let large = u8::from(arc.sweep.abs() > std::f64::consts::PI);
                // The y axis is flipped on the way out, which turns a
                // counter-clockwise arc into a clockwise one on the page.
                let sweep = u8::from(arc.sweep < 0.0);
                let _ = write!(
                    d,
                    " A {r} {r} 0 {large} {sweep} {} {}",
                    frame.x(to.x),
                    frame.y(to.y)
                );
            }
            None => {
                let _ = write!(d, " L {} {}", frame.x(to.x), frame.y(to.y));
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

/// Whether every one of these edges lies in one plane parallel to XY, so the
/// body has a true plan view and needs no projecting.
///
/// A REGION is built from a closed 2D profile, so this is the normal case
/// for one; a 3DSOLID or POLYLINE_PFACE reaches it whenever the body is
/// flat. The test is on the z span against the xy span, relatively, because
/// a body's vertices come back with rounding noise around their plane; the
/// floor of 1 keeps a flat but tiny profile from being judged by its own
/// size.
fn flat_in_xy(edges: &[[Point3D; 2]]) -> bool {
    let (mut lo_z, mut hi_z) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for p in edges.iter().flatten() {
        lo_z = lo_z.min(p.z);
        hi_z = hi_z.max(p.z);
        lo = lo.min(p.x).min(p.y);
        hi = hi.max(p.x).max(p.y);
    }
    if !lo_z.is_finite() {
        return false;
    }
    (hi_z - lo_z) <= 1e-9 * (hi - lo).max(1.0)
}

/// One `<line>` per edge -- shared by 3DSOLID, REGION and POLYLINE_PFACE,
/// which all reduce to a set of 3D edges.
///
/// A body flat in a plane parallel to XY is drawn in that plane, where the
/// file puts it. Only a body with depth goes through [`project_isometric`],
/// which scales x and shears y: a flat rectangle drawn that way came out as a
/// parallelogram of the wrong size, away from the rest of the drawing, and
/// its extent with it.
fn wireframe_element(edges: &[[Point3D; 2]], color: &str, ctx: &mut Ctx) -> String {
    let flat = flat_in_xy(edges);
    let place = |p: &Point3D| {
        if flat {
            (p.x, p.y)
        } else {
            project_isometric(p)
        }
    };
    edges
        .iter()
        .map(|[a, b]| {
            let (x1, y1) = place(a);
            let (x2, y2) = place(b);
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
    let parent_frame = ctx.frame;
    let child_frame = child_frame(&child_transform, parent_frame);
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
    ctx.id_path.push(owner.common().id);

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

    ctx.id_path.pop();
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
    // The drawing hides it: not drawn -- or drawn faded, when asked -- and
    // not part of the extent either way.
    let hidden = visibility::hidden_reason(
        e,
        ctx.tables,
        ctx.inherited_layer.as_deref(),
        &ctx.viewport_frozen,
    )
    .is_some();
    if hidden {
        ctx.hidden += 1;
        if !ctx.include_hidden {
            return None;
        }
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
    let bounds = ctx.bounds();
    let mut svg = draw_entity(e, ctx);
    if hidden {
        ctx.set_bounds(bounds);
        svg = svg.map(|svg| {
            if svg.is_empty() {
                svg
            } else {
                format!("<g opacity=\"0.5\">{svg}</g>")
            }
        });
    }
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
        Entity::Solid3D(s)
        | Entity::Region(s)
        | Entity::PolylinePFace(s)
        | Entity::PolylineMesh(s) => s.wireframe_edges.len(),
        Entity::Hatch(h) => {
            let boundary: usize = h
                .boundary_paths
                .iter()
                .map(|path| match path {
                    // A bulged segment is drawn through as many points as
                    // an arc edge.
                    HatchBoundaryPath::Polyline(v) => v
                        .iter()
                        .map(|v| {
                            if v.bulge == 0.0 {
                                1
                            } else {
                                hatch::ARC_SEGMENTS
                            }
                        })
                        .sum::<usize>(),
                    HatchBoundaryPath::Edges(edges) => edges
                        .iter()
                        .map(|edge| match edge {
                            HatchEdge::Line { .. } => 1,
                            HatchEdge::Arc { .. } => hatch::ARC_SEGMENTS,
                            HatchEdge::Ellipse { .. } => hatch::ELLIPSE_SEGMENTS,
                            HatchEdge::Spline { control_points, .. } => control_points.len(),
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
        Entity::Circle(c) => {
            p3(&c.center) && real(&[c.radius]) && plane_numbers_are_real(&c.extrusion, c.center.z)
        }
        Entity::Arc(a) => {
            p3(&a.center)
                && real(&[a.radius])
                && plane_numbers_are_real(&a.extrusion, a.center.z)
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
        Entity::LwPolyline(p) | Entity::Polyline2D(p) => {
            p.vertices.iter().all(|v| p2(&v.point) && real(&[v.bulge]))
                && plane_numbers_are_real(&p.extrusion, p.elevation)
        }
        Entity::Polyline3D(p) => p.vertices.iter().all(p3),
        Entity::Text(t) => {
            p2(&t.start_point)
                && t.alignment_point.as_ref().is_none_or(p2)
                && real(&[t.text_height, t.rotation, t.width_factor])
                && is_sane_angle(t.oblique_angle)
                && plane_numbers_are_real(&t.extrusion, t.elevation)
        }
        Entity::Attrib(a) => {
            p2(&a.start_point)
                && a.alignment_point.as_ref().is_none_or(p2)
                && real(&[a.text_height, a.rotation, a.width_factor])
                && is_sane_angle(a.oblique_angle)
                && plane_numbers_are_real(&a.extrusion, a.elevation)
        }
        Entity::Tolerance(t) => {
            p3(&t.insertion_point) && t.text_height.is_none_or(|h| h.is_finite())
        }
        Entity::MText(m) => {
            p3(&m.insertion_point)
                && real(&[
                    m.text_height,
                    m.rotation,
                    m.line_spacing_factor,
                    m.reference_width,
                ])
                && real(&[m.extents_width, m.extents_height].map(|v| v.unwrap_or(0.0)))
        }
        Entity::Point(p) => p3(&p.position),
        Entity::Solid(s) | Entity::Trace(s) => {
            [s.corner1, s.corner2, s.corner3, s.corner4].iter().all(p2)
                && plane_numbers_are_real(&s.extrusion, s.elevation)
        }
        Entity::Face3D(f) => [f.corner1, f.corner2, f.corner3, f.corner4].iter().all(p3),
        Entity::Ray(r) | Entity::XLine(r) => p3(&r.point) && p3(&r.vector),
        Entity::Insert(i) => {
            p3(&i.insertion_point)
                && real(&[i.scale.x, i.scale.y, i.rotation])
                && plane_numbers_are_real(&i.extrusion, i.insertion_point.z)
        }
        Entity::AcadTable(a) => p3(&a.insertion_point) && real(&[a.scale.x, a.scale.y, a.rotation]),
        Entity::Viewport(v) => p3(&v.center) && real(&[v.width, v.height]),
        Entity::Wipeout(w) => w.boundary.iter().all(p2),
        Entity::Spline(s) => s.fit_points.iter().all(p3) && s.control_points.iter().all(p3),
        Entity::Solid3D(s)
        | Entity::Region(s)
        | Entity::PolylinePFace(s)
        | Entity::PolylineMesh(s) => s.wireframe_edges.iter().all(|[a, b]| xyz(a) && xyz(b)),
        Entity::Hatch(h) => h.boundary_paths.iter().all(|path| match path {
            HatchBoundaryPath::Polyline(v) => v.iter().all(|v| p2(&v.point) && real(&[v.bulge])),
            HatchBoundaryPath::Edges(edges) => edges.iter().all(|edge| match edge {
                HatchEdge::Line { start, .. } => p2(start),
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
                HatchEdge::Spline { control_points, .. } => control_points.iter().all(p2),
            }),
        }),
        Entity::Leader(l) => l.vertices.iter().all(p3),
        Entity::MultiLeader(m) => m.lines.iter().flatten().all(p3),
        Entity::MLine(l) => {
            l.vertices
                .iter()
                .all(|v| p3(&v.point) && p3(&v.miter_direction))
                && l.scale.is_none_or(f64::is_finite)
        }
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
        Entity::Circle(c) if !own_plane(c.extrusion).is_world() => {
            Some(circle_in_own_plane(c, &color, ctx))
        }
        Entity::Arc(a) if !own_plane(a.extrusion).is_world() => {
            Some(arc_in_own_plane(a, &color, ctx))
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
            let on_plane = |angle: f64| Point2D {
                x: x + r * angle.cos(),
                y: y + r * angle.sin(),
            };
            let mut sweep = a.end_angle - a.start_angle;
            if sweep < 0.0 {
                sweep += 2.0 * std::f64::consts::PI;
            }
            let (p1, p2) = (on_plane(a.start_angle), on_plane(a.end_angle));
            // The arc's own extent, not the whole circle's: a large-radius
            // fillet must not stretch the picture to its centre.
            ctx.consider_box(&arc_extent(Point2D { x, y }, r, a.start_angle, sweep));
            let large = if sweep > std::f64::consts::PI { 1 } else { 0 };
            let r = clean(r);
            Some(format!(
                "<path d=\"M {} {} A {r} {r} 0 {large} 0 {} {}\" fill=\"none\" stroke=\"{color}\"/>",
                frame.x(p1.x),
                frame.y(p1.y),
                frame.x(p2.x),
                frame.y(p2.y)
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
                        ..*v
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
            Some(polyline_element(&xy(&p.vertices), p.closed, &color, frame))
        }
        Entity::Text(t) => {
            let layout = TextLayout::new(
                justify::anchor(
                    t.start_point,
                    t.alignment_point,
                    t.horizontal_justification,
                    t.vertical_justification,
                    ctx.descender(),
                ),
                effective_text_height(t.text_height),
                t.rotation,
                t.oblique_angle,
                t.width_factor,
            )
            .in_plane(own_plane(t.extrusion), t.elevation);
            let text = text_codes::decode(&t.text, false);
            let estimate = justify::consider_text_box(&layout, &text, ctx);
            let id = ctx.record_text(t.common.id, &text, estimate);
            Some(justify::text_element(
                &id,
                &layout,
                &color,
                &text,
                frame,
                ctx.cap_height,
            ))
        }
        Entity::Attrib(a) => {
            let layout = TextLayout::new(
                justify::anchor(
                    a.start_point,
                    a.alignment_point,
                    a.horizontal_justification,
                    a.vertical_justification,
                    ctx.descender(),
                ),
                effective_text_height(a.text_height),
                a.rotation,
                a.oblique_angle,
                a.width_factor,
            )
            .in_plane(own_plane(a.extrusion), a.elevation);
            if a.text.is_empty() {
                ctx.consider(layout.anchor.at.x, layout.anchor.at.y);
                return Some(String::new());
            }
            let text = text_codes::decode(&a.text, false);
            let estimate = justify::consider_text_box(&layout, &text, ctx);
            let id = ctx.record_text(a.common.id, &text, estimate);
            Some(justify::text_element(
                &id,
                &layout,
                &color,
                &text,
                frame,
                ctx.cap_height,
            ))
        }
        Entity::Tolerance(t) => {
            ctx.consider(t.insertion_point.x, t.insertion_point.y);
            if t.text_value.is_empty() {
                return Some(String::new());
            }
            // A frame whose file never stated a height still has to be drawn
            // at some size -- the renderer's choice, which is why the model
            // does not make it: the text height (DIMTXT) of the dimension
            // style the frame names, and 1 when that states none either.
            let positive = |h: f64| (h.is_finite() && h > 0.0).then_some(h);
            let height = t
                .text_height
                .and_then(positive)
                .or_else(|| {
                    t.style_name
                        .resolved()
                        .and_then(|name| ctx.tables.dim_styles.get(name))
                        .and_then(|style| style.text_height)
                        .and_then(positive)
                })
                .unwrap_or(1.0);
            let layout = TextLayout::new(
                Anchor {
                    at: Point2D {
                        x: t.insertion_point.x,
                        y: t.insertion_point.y,
                    },
                    anchor: "start",
                    drop: 0.0,
                },
                height,
                0.0,
                0.0,
                1.0,
            );
            // Its box is not counted towards the extent (the insertion
            // point is), but it is estimated like any text's.
            let estimate = justify::estimate_text_box(&layout, &t.text_value, ctx);
            let id = ctx.record_text(t.common.id, &t.text_value, estimate);
            Some(justify::text_element(
                &id,
                &layout,
                &color,
                &t.text_value,
                frame,
                ctx.cap_height,
            ))
        }
        Entity::MText(m) => {
            let at = Point2D {
                x: m.insertion_point.x,
                y: m.insertion_point.y,
            };
            let decoded = text_codes::decode(&m.text, true);
            // An empty line is a real line: `\P\P` is how a note spaces its
            // paragraphs, and it takes up its line height.
            let lines: Vec<&str> = decoded
                .split('\n')
                .map(|l| l.strip_suffix('\r').unwrap_or(l))
                .collect();
            if lines.iter().all(|l| l.is_empty()) {
                ctx.consider(at.x, at.y);
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
            let block = MTextBlock::new(
                (m.extents_width, m.extents_height),
                m.reference_width,
                lines.iter().map(|l| l.chars().count()).max().unwrap_or(0),
                text_height,
                text_height + line_height * lines.len().saturating_sub(1) as f64,
                ctx.cap_height,
            );
            let estimate =
                justify::consider_mtext_box(at, m.rotation, m.attachment, &block, text_height, ctx);
            let id = ctx.record_text(m.common.id, &lines.join("\n"), estimate);
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
                "<text id=\"{id}\" x=\"{x}\" y=\"{first_baseline}\" font-size=\"{font_size}\" text-anchor=\"{anchor}\" fill=\"{color}\" stroke=\"none\" transform=\"rotate({} {x} {y})\">{tspans}</text>",
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
                frame.points(&pts)
            ))
        }
        Entity::Face3D(f) => {
            // Unlike SOLID, 3DFACE's 4 corners are already sequential.
            let pts = xy(&[f.corner1, f.corner2, f.corner3, f.corner4]);
            ctx.consider_all(&pts);
            if f.invisible_edges.iter().all(|hidden| !hidden) {
                return Some(format!(
                    "<polygon points=\"{}\" fill=\"none\" stroke=\"{color}\"/>",
                    frame.points(&pts)
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
                    frame.x(a.x),
                    frame.y(a.y),
                    frame.x(b.x),
                    frame.y(b.y)
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
        Entity::Insert(i) => {
            // In a plane parallel to the world's -- the world's own, or a
            // mirror copy's -- the model places the block exactly. A tilted
            // plane has no exact 2D placement: the block is placed in its
            // plane and that plane seen from above, as a HATCH is.
            let placement = i.world_transform().unwrap_or_else(|| {
                i.transform().then(&plane_seen_from_above(
                    own_plane(i.extrusion),
                    i.insertion_point.z,
                ))
            });
            Some(render_block_ref(e, &i.block_name, placement, &color, ctx))
        }
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
        Entity::PolylineMesh(p) => {
            render_wireframe_entity(&p.wireframe_edges, "POLYLINE_MESH", &color, ctx)
        }
        // Everything a HATCH states -- boundary, pattern lines, gradient --
        // is in its own plane, so the whole fill stays exact.
        Entity::Hatch(h) => in_own_plane(h.extrusion, h.elevation, ctx, |ctx| {
            hatch::render_hatch(h, h.common.id, &color, ctx)
        }),
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
            // The style's offsets are in the style's units; the MLINE's own
            // scale (DXF 40) is what puts them in drawing units -- a wall
            // 200 thick is the +-0.5 STANDARD style at scale 200. A model
            // not given the scale draws the style's own offsets: the
            // renderer's choice, the model states none.
            let scale = l.scale.unwrap_or(1.0);
            let lines: Vec<String> = offsets
                .iter()
                .map(|&offset| {
                    let points = mline_offset_points(&l.vertices, offset * scale);
                    // The offset lines are what is drawn, so they are what
                    // the extent covers, not the centerline alone.
                    ctx.consider_all(&points);
                    polyline_element(&points, l.closed, &color, frame)
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
    select_owned_by(db, |name| {
        let upper = name.to_uppercase();
        match space {
            Space::Model => upper == "*MODEL_SPACE",
            Space::Paper => upper.starts_with("*PAPER_SPACE"),
            Space::All => unreachable!(),
        }
    })
}

/// The top-level entities the blocks `owns` accepts (by name) own, in the
/// order `db.entities` lists them: each block's own entities, and the
/// attribute values of its block references, which the model lists at the
/// top level beside them.
fn select_owned_by(db: &CadDatabase, owns: impl Fn(&str) -> bool) -> Vec<&Entity> {
    let mut ids: BTreeSet<EntityId> = BTreeSet::new();
    for (name, record) in &db.tables.block_records {
        if !owns(name) {
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
        Entity::Circle(c) => in_world(c.extrusion, p3(&c.center), c.center.z),
        Entity::Arc(a) => in_world(a.extrusion, p3(&a.center), a.center.z),
        Entity::Ellipse(el) => p3(&el.center),
        Entity::LwPolyline(p) | Entity::Polyline2D(p) => {
            in_world(p.extrusion, p.vertices.first()?.point, p.elevation)
        }
        Entity::Polyline3D(p) => p3(p.vertices.first()?),
        Entity::Text(t) => in_world(t.extrusion, t.start_point, t.elevation),
        Entity::Attrib(a) => in_world(a.extrusion, a.start_point, a.elevation),
        Entity::Attdef(a) => in_world(a.extrusion, a.start_point, a.elevation),
        Entity::Tolerance(t) => p3(&t.insertion_point),
        Entity::MText(m) => p3(&m.insertion_point),
        Entity::Point(p) => p3(&p.position),
        Entity::Solid(s) | Entity::Trace(s) => in_world(s.extrusion, s.corner1, s.elevation),
        Entity::Face3D(f) => p3(&f.corner1),
        Entity::Ray(r) | Entity::XLine(r) => p3(&r.point),
        Entity::Insert(i) => in_world(
            i.extrusion,
            Point2D {
                x: i.insertion_point.x,
                y: i.insertion_point.y,
            },
            i.insertion_point.z,
        ),
        Entity::AcadTable(a) => p3(&a.insertion_point),
        Entity::Dimension(d) => d.text_midpoint,
        Entity::Viewport(v) => p3(&v.center),
        Entity::Wipeout(w) => *w.boundary.first()?,
        Entity::Spline(s) => p3(s.fit_points.first().or(s.control_points.first())?),
        Entity::Solid3D(s)
        | Entity::Region(s)
        | Entity::PolylinePFace(s)
        | Entity::PolylineMesh(s) => p3(&s.wireframe_edges.first()?[0]),
        Entity::Hatch(h) => match h.boundary_paths.first()? {
            HatchBoundaryPath::Polyline(v) => v.first()?.point,
            HatchBoundaryPath::Edges(edges) => match edges.first()? {
                HatchEdge::Line { start, .. } => *start,
                HatchEdge::Arc { center, .. } | HatchEdge::Ellipse { center, .. } => *center,
                HatchEdge::Spline { control_points, .. } => *control_points.first()?,
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

/// Renders every entity of `options.space`, measures the extent and frames
/// it as `options.crop` says, leaving the stroke width unresolved: the
/// [`Scene`] [`to_svg`] and [`crate::to_png`] assemble.
pub(crate) fn render(db: &CadDatabase, options: ToSvgOptions) -> Scene {
    let selected = select_entities_for_space(db, options.space);
    let origin = choose_origin(&selected);
    let mut ctx = Ctx::configured(&db.tables, &options, origin);
    let mut walked = walk(&selected, &mut ctx);
    let framed = crop_parts(&mut walked, options.crop);
    let view_box = view_box_of(&framed.content_or_origin(), options.padding, origin);
    let crop = crop_report(
        &mut walked,
        framed,
        &scene::world_rect(view_box.rect, origin),
    );
    ctx.finish(walked, view_box, origin, crop)
}

/// Renders each of `selected` as one top-level [`Part`], in order, with the
/// elements it drew (empty when it drew nothing). An entity whose extent
/// reaches past what a viewBox can be built from is left out and reported;
/// one cut short by the per-entity budget is kept and reported as
/// truncated.
fn walk(selected: &[&Entity], ctx: &mut Ctx) -> Vec<(Part, String)> {
    let mut parts = Vec::with_capacity(selected.len());
    for e in selected {
        let hidden = visibility::hidden_reason(
            e,
            ctx.tables,
            ctx.inherited_layer.as_deref(),
            &ctx.viewport_frozen,
        );
        ctx.reset_entity_bounds();
        ctx.entity_start = ctx.emitted;
        ctx.part_truncated = false;
        let texts_before = ctx.texts.len();
        let svg = render_entity(e, ctx);
        let extent = ctx.entity_box();
        let mut part = Part {
            id: e.common().id,
            type_name: e.type_name().to_string(),
            extent: extent.map(Rect::from),
            drawn: false,
            unbounded: false,
            hidden,
            left_out: None,
            through_viewport: false,
        };
        if extent.is_some_and(|b| !within_world(&b)) {
            // Past what a viewBox -- and the stroke width, padding and dash
            // lengths derived from it -- can be built from.
            ctx.limits.out_of_range_entities += 1;
            ctx.limits
                .note(Cap::OutOfRange, e.common().id, e.type_name());
            // Nothing of it is drawn, so neither are the texts it drew.
            ctx.texts.truncate(texts_before);
            part.extent = None;
            parts.push((part, String::new()));
            continue;
        }
        let svg = match svg {
            Some(svg) => {
                if ctx.part_truncated {
                    // What the entity drew before its budget ran out is
                    // kept; the report says the part is incomplete.
                    ctx.limits.truncated_parts += 1;
                    ctx.limits
                        .note(Cap::EntityBytes, e.common().id, e.type_name());
                }
                svg
            }
            None => String::new(),
        };
        part.drawn = !svg.is_empty();
        part.unbounded = infinite::contains_placeholder(&svg);
        parts.push((part, svg));
    }
    parts
}

/// What [`crop_parts`] framed.
struct Framed {
    /// See [`CropReport::content`].
    content: Option<Box2D>,
    stated_taken: bool,
}

impl Framed {
    /// The framed rectangle, or the origin when nothing was measured: an
    /// empty drawing is a padded canvas there.
    fn content_or_origin(&self) -> Box2D {
        self.content.unwrap_or(Box2D {
            min_x: 0.0,
            max_x: 0.0,
            min_y: 0.0,
            max_y: 0.0,
        })
    }
}

/// Runs `crop` over the extents `walked` measured and marks the parts it
/// sets aside. A construction line is never set aside: its extent is only
/// its base point, which may well be an outlier the frame should not
/// stretch to, but the line crosses whatever the frame is.
fn crop_parts(walked: &mut [(Part, String)], crop: Crop) -> Framed {
    let measured: Vec<(usize, Box2D)> = walked
        .iter()
        .enumerate()
        .filter_map(|(i, (part, _))| part.extent.map(|e| (i, Box2D::from(e))))
        .collect();
    let boxes: Vec<Box2D> = measured.iter().map(|(_, b)| *b).collect();
    let choice = crop::choose(&boxes, crop);
    for (j, reason) in choice.set_aside {
        let part = &mut walked[measured[j].0].0;
        if !part.unbounded {
            part.left_out = Some(reason);
        }
    }
    Framed {
        content: choice.content,
        stated_taken: choice.stated_taken,
    }
}

/// The crop's report for a scene whose view box is `view` (world): the
/// parts set aside, and every other part whose extent does not reach the
/// view -- marked in `walked` too -- in drawing order. A construction line
/// reaches every view.
fn crop_report(walked: &mut [(Part, String)], framed: Framed, view: &Rect) -> CropReport {
    let mut left_out = Vec::new();
    for (part, _) in walked.iter_mut() {
        let Some(extent) = part.extent else { continue };
        if part.left_out.is_none() && !part.unbounded && !extent.intersects(view) {
            part.left_out = Some(LeftOutReason::OutsideView);
        }
        if let Some(reason) = part.left_out {
            left_out.push(LeftOut {
                id: part.id,
                type_name: part.type_name.clone(),
                extent,
                reason,
            });
        }
    }
    CropReport {
        content: framed.content.map(Rect::from),
        stated_taken: framed.stated_taken,
        left_out,
    }
}

/// A viewBox and the stroke width [`to_svg`] uses when none is given.
struct ViewBox {
    /// `x, y, width, height`, in the render's frame.
    rect: [f64; 4],
    /// ~1/6000th of the padded extent's diagonal, floored at 0.01.
    auto_stroke_width: f64,
}

/// The viewBox showing the world box `bounds` with `padding` around it,
/// written in the frame whose origin is `origin`, like every coordinate
/// inside it. A degenerate (zero-size) extent still gets a 1 x 1 canvas.
fn view_box_of(bounds: &Box2D, padding: f64, origin: Point2D) -> ViewBox {
    let x = bounds.min_x - padding - origin.x;
    let y = -bounds.max_y - padding + origin.y;
    let width = (bounds.max_x - bounds.min_x) + padding * 2.0;
    let height = (bounds.max_y - bounds.min_y) + padding * 2.0;
    let auto_stroke_width = (width.hypot(height) / 6000.0).max(0.01);
    let width = if width != 0.0 { width } else { 1.0 };
    let height = if height != 0.0 { height } else { 1.0 };
    ViewBox {
        rect: [x, y, width, height],
        auto_stroke_width,
    }
}

/// Renders a parsed [`CadDatabase`] to an SVG string.
///
/// The viewBox is the rectangle [`ToSvgOptions::crop`] chooses (by default
/// the dominant spatially-connected cluster of entities rather than the raw
/// min/max, so that a stray far-off entity does not shrink the drawing to a
/// dot), padded; the entities it does not show are in
/// [`ToSvgResult::crop`], and an outlier [`Crop::Guarded`] sets aside is not
/// drawn at all.
///
/// `stroke_width` defaults to ~1/6000th of the computed viewBox diagonal
/// rather than a fixed value, and nested block references keep a constant
/// visual weight whatever they are scaled by.
pub fn to_svg(db: &CadDatabase, options: ToSvgOptions) -> ToSvgResult {
    svg_result(render(db, options), options.stroke_width)
}

/// Renders the paper layout named `layout` (a key of `tables.layouts`) as
/// its sheet: the layout's own entities, and the model shown through each
/// of its viewports -- at the viewport's scale (its frame's height over its
/// view's), turned by its twist, clipped to its frame, without the layers
/// frozen in it. The viewBox is the sheet: the layout's limits when they
/// span a rectangle, else the paper its plot settings describe, with no
/// padding; a layout that states neither is framed like a render of its
/// paper space (`padding` and `crop` as `options` say). `space` is not
/// used. The SVG is written in the layout's paper units, relative to
/// [`ToSvgResult::origin`] like any render.
///
/// A viewport that is off, or is the layout's overall viewport (the sheet
/// itself), shows nothing; one whose view cannot be drawn is reported in
/// [`ToSvgResult::undrawn_viewports`]. Every viewport's frame is drawn as
/// paper space draws it, hidden when its layer is -- and its view is shown
/// either way.
///
/// An error when the model holds no such layout, when it is the model tab,
/// or when its paper space block is not in the model.
pub fn layout_to_svg(
    db: &CadDatabase,
    layout: &str,
    options: ToSvgOptions,
) -> Result<ToSvgResult, LayoutError> {
    Ok(svg_result(
        sheet::render_layout(db, layout, options)?,
        options.stroke_width,
    ))
}

/// `scene`'s whole document at `stroke_width`, or at its own automatic
/// width when none is given, with its reports.
fn svg_result(scene: Scene, stroke_width: Option<f64>) -> ToSvgResult {
    let stroke_width = stroke_width.unwrap_or(scene.auto_stroke_width);
    ToSvgResult {
        svg: scene.document(stroke_width),
        unsupported_types: scene.unsupported_types,
        empty_blocks: scene.empty_blocks,
        unresolved_block_refs: scene.unresolved_block_refs,
        limits: scene.limits,
        origin: scene.origin,
        hidden: scene.hidden,
        undrawn_viewports: scene.undrawn_viewports,
        view_box: scene.view_box,
        crop: scene.crop,
        viewports: scene.viewports,
        sheet: scene.sheet,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bounds::diag;
    use uncad_model::model::{HorizontalJustification, TextEntity, VerticalJustification};

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
                    extrusion: Point3D {
                        x: 0.0,
                        y: 0.0,
                        z: 1.0,
                    },
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
            extrusion: Point3D {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            },
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

    fn p3(x: f64, y: f64, z: f64) -> Point3D {
        Point3D { x, y, z }
    }

    #[test]
    fn flat_in_xy_tolerates_rounding_noise_but_not_real_depth() {
        // 1e-12 over a ten-unit profile is flat, 1e-3 is not.
        assert!(flat_in_xy(&[[p3(0.0, 0.0, 0.0), p3(10.0, 10.0, 1e-12)]]));
        assert!(!flat_in_xy(&[[p3(0.0, 0.0, 0.0), p3(10.0, 10.0, 1e-3)]]));
        // A tiny but flat profile is judged against the floor of 1, not
        // against its own size.
        assert!(flat_in_xy(&[[p3(0.0, 0.0, 0.0), p3(0.001, 0.001, 0.0)]]));
        // A flat profile off z = 0 is still flat.
        assert!(flat_in_xy(&[[p3(0.0, 0.0, 7.0), p3(5.0, 5.0, 7.0)]]));
        assert!(!flat_in_xy(&[]));
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
                    ..PolylineVertex::default()
                })
                .collect(),
            closed,
            const_width: 0.0,
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
            style: None,
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
    fn a_mirrored_block_reference_is_placed_where_the_world_sees_it() {
        use std::collections::BTreeMap;
        use uncad_model::model::{InsertEntity, LineEntity, Ref};
        use uncad_model::tables::BlockRecord;
        let line = Entity::Line(LineEntity {
            common: plain_common(),
            start_point: Point3D {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            end_point: Point3D {
                x: 8.0,
                y: 0.0,
                z: 0.0,
            },
        });
        let mut block_records = BTreeMap::new();
        block_records.insert(
            "M".to_string(),
            BlockRecord {
                name: "M".to_string(),
                entities: vec![line],
            },
        );
        let db = CadDatabase {
            entities: vec![Entity::Insert(InsertEntity {
                common: plain_common(),
                block_name: Ref::Resolved("M".to_string()),
                insertion_point: Point3D {
                    x: -175.0,
                    y: -66.0,
                    z: 0.0,
                },
                scale: Point3D {
                    x: 1.0,
                    y: 1.0,
                    z: 1.0,
                },
                rotation: 0.0,
                attribs: Vec::new(),
                extrusion: mirrored(),
            })],
            tables: Tables {
                block_records,
                ..Tables::default()
            },
            read_diagnostics: Default::default(),
        };
        let svg = to_svg(
            &db,
            ToSvgOptions {
                space: Space::All,
                ..ToSvgOptions::default()
            },
        )
        .svg;
        // Written at x -175 in a system whose x is the world's -x: the line
        // runs from world x 175 back to 167, so the view is centred on 171.
        let [x, _, w, _] = view_box(&svg);
        close(x + w / 2.0, 171.0);
        assert!(svg.contains("<g transform=\"matrix(-1 "), "{svg}");
    }

    #[test]
    fn a_mirrored_text_is_drawn_in_its_plane_and_placed_by_it() {
        let text = |x: f64, extrusion: Point3D| {
            render_one(Entity::Text(TextEntity {
                common: plain_common(),
                start_point: Point2D { x, y: -72.0 },
                text_height: 2.5,
                text: "MIRROR".to_string(),
                rotation: 0.0,
                horizontal_justification: HorizontalJustification::Left,
                vertical_justification: VerticalJustification::Baseline,
                alignment_point: None,
                width_factor: 1.0,
                oblique_angle: 0.0,
                style_name: Ref::Absent,
                elevation: 0.0,
                extrusion,
            }))
        };
        // Stated at x -170 in a system whose x is the world's -x, the text
        // starts at the world's 170 and reads leftwards: its extent is the
        // mirror image, about x 170, of the same text written at 170 in the
        // world's own plane.
        let [mx, _, mw, _] = view_box(&text(-170.0, mirrored()));
        let [ux, _, uw, _] = view_box(&text(
            170.0,
            Point3D {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            },
        ));
        close(mw, uw);
        close(mx + mw / 2.0, 340.0 - (ux + uw / 2.0));
        assert!(mx + mw / 2.0 < 170.0);
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

    fn polygon_points(svg: &str, tag: &str) -> Vec<(f64, f64)> {
        svg.split(&format!("<{tag} points=\""))
            .nth(1)
            .unwrap_or_else(|| panic!("a {tag}: {svg}"))
            .split('"')
            .next()
            .unwrap()
            .split_whitespace()
            .map(|pair| {
                let (x, y) = pair.split_once(',').unwrap();
                (x.parse().unwrap(), y.parse().unwrap())
            })
            .collect()
    }

    #[test]
    fn a_tilted_arc_whose_angles_are_turns_apart_goes_round_once_at_most() {
        // From 0 to a quarter turn past two whole turns: the same directions
        // as a quarter turn, and drawn through as many points -- not round
        // the circle twice more, nor through points a stored angle of any
        // size could multiply.
        use std::f64::consts::{FRAC_PI_2, TAU};
        let tilted_arc = |end: f64| {
            render_one(Entity::Arc(ArcEntity {
                common: plain_common(),
                center: Point3D {
                    x: 2.0,
                    y: 0.0,
                    z: 0.0,
                },
                radius: 1.0,
                start_angle: 0.0,
                end_angle: end,
                extrusion: Point3D {
                    x: 1.0,
                    y: 0.0,
                    z: 1.0,
                },
            }))
        };
        let quarter = polygon_points(&tilted_arc(FRAC_PI_2), "polyline");
        let wound = polygon_points(&tilted_arc(2.0 * TAU + FRAC_PI_2), "polyline");
        assert_eq!(quarter.len(), 17);
        assert_eq!(wound.len(), quarter.len());
        for (a, b) in quarter.iter().zip(&wound) {
            assert!((a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9);
        }
    }

    #[test]
    fn an_arc_too_flat_to_draw_is_its_chord_on_a_tilted_plane_too() {
        // A bulge of 1e-160 over ten units: sampled on its arc, every point
        // would be reckoned from a center some 1e160 away.
        let mut e = polyline(&[(0.0, 0.0, 1e-160), (10.0, 0.0, 0.0)], false);
        if let Entity::LwPolyline(p) = &mut e {
            p.extrusion = Point3D {
                x: 1.0,
                y: 0.0,
                z: 1.0,
            };
        }
        let svg = render_one(e);
        // The two vertices, seen from above: (0, 0) and (0, 10).
        assert_eq!(
            polygon_points(&svg, "polyline"),
            [(0.0, 0.0), (0.0, -10.0)],
            "{svg}"
        );
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

    #[test]
    fn an_arc_so_flat_its_radius_passes_the_world_is_not_drawable() {
        // A bulge of 1e-160 over ten units: a radius of about 6e160.
        let flat = BulgeArc::between(
            Point2D { x: 0.0, y: 0.0 },
            Point2D { x: 10.0, y: 5.0 },
            1e-160,
        )
        .unwrap();
        assert!(!arc_drawable(&flat), "{flat:?}");
        let half =
            BulgeArc::between(Point2D { x: 0.0, y: 0.0 }, Point2D { x: 2.0, y: 0.0 }, 1.0).unwrap();
        assert!(arc_drawable(&half));
    }

    #[test]
    fn a_plane_seen_from_above_at_a_height() {
        // Normal (1, 0, 0): a point (x, y) at height z is the world point
        // (z, x, y) -- seen from above, (z, x).
        let m = plane_seen_from_above(
            Ocs::of(Point3D {
                x: 1.0,
                y: 0.0,
                z: 0.0,
            })
            .unwrap(),
            7.0,
        );
        let q = m.apply(Point2D { x: 2.0, y: 3.0 });
        assert!(
            (q.x - 7.0).abs() < 1e-12 && (q.y - 2.0).abs() < 1e-12,
            "{q:?}"
        );
    }

    #[test]
    fn a_flat_plane_seen_from_above_is_the_models_placement() {
        use uncad_model::model::InsertEntity;
        // An INSERT at the plane's origin, unscaled and unturned, is placed
        // by exactly the plane's map wherever the model places it at all:
        // the renderer's view and the model's placement cannot disagree.
        for normal in [
            Point3D {
                x: 0.0,
                y: 0.0,
                z: -1.0,
            },
            Point3D {
                x: 1e-17,
                y: 0.0,
                z: 1.0,
            },
        ] {
            let insert = InsertEntity {
                common: plain_common(),
                block_name: Ref::Absent,
                insertion_point: Point3D {
                    x: 0.0,
                    y: 0.0,
                    z: 2.5,
                },
                scale: Point3D {
                    x: 1.0,
                    y: 1.0,
                    z: 1.0,
                },
                rotation: 0.0,
                attribs: Vec::new(),
                extrusion: normal,
            };
            let model = insert.world_transform().expect("a flat plane");
            let ours = plane_seen_from_above(Ocs::of(normal).unwrap(), 2.5);
            for (a, b) in [
                (model.a, ours.a),
                (model.b, ours.b),
                (model.c, ours.c),
                (model.d, ours.d),
                (model.e, ours.e),
                (model.f, ours.f),
            ] {
                assert!((a - b).abs() < 1e-12, "{normal:?}: {model:?} vs {ours:?}");
            }
        }
    }

    #[test]
    fn a_planes_height_has_to_be_a_number_unless_the_plane_is_the_worlds() {
        let p3 = |x, y, z| Point3D { x, y, z };
        assert!(plane_numbers_are_real(&p3(0.0, 0.0, 1.0), f64::NAN));
        // Mirrored, the height adds nothing in plan -- but it is multiplied
        // in, and a NaN reaches the point.
        assert!(!plane_numbers_are_real(&p3(0.0, 0.0, -1.0), f64::NAN));
        assert!(!plane_numbers_are_real(&p3(1.0, 0.0, 1.0), f64::NAN));
        assert!(!plane_numbers_are_real(&p3(0.0, f64::INFINITY, 1.0), 0.0));
    }
}
