//! A change set drawn on top of the original: the redline.
//!
//! The document holds two layers. The first is the original, drawn exactly
//! as [`crate::to_svg`] draws it on its own. The second holds the changes,
//! in one proposal colour: what an entity became (or the entity that was
//! added), what was removed as a dashed outline, and a revision cloud
//! around every change. Nothing in the second layer is guessed: a change
//! whose counterpart is uncertain gets a cloud and no geometry, and a change
//! the picture cannot show is reported by name rather than dropped.

use super::crop::{Crop, LeftOut};
use super::format::clean;
use super::scene::{doc_view_box, origin_attribute, page, root_stroke, Rect, Scene};
use super::ToSvgOptions;
use iron_diff_cad::{Change, ChangeSet};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use uncad_model::model::{Entity, EntityId, Point2D};
use uncad_model::CadDatabase;

/// The proposal colour [`OverlayOptions`] uses when none is given: red, as
/// in "redline".
pub const DEFAULT_PROPOSAL_COLOR: [u8; 3] = [0xe4, 0x00, 0x2b];

/// How [`overlay_to_svg`] draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayOptions {
    /// How both states are rendered, with the same meaning as for
    /// [`crate::to_svg`]. The original layer is that render of the first
    /// state; the stroke width, when none is given, is the first state's
    /// automatic one, and the second state is drawn at the same width. On
    /// a dark page ([`crate::Paper::Dark`]) both states' colors are
    /// resolved for it and the document starts with a black rectangle
    /// covering its view, outside the original layer; the proposal color
    /// is drawn as given.
    pub svg: ToSvgOptions,
    /// The sRGB colour every mark of the change layer is drawn in.
    pub proposal_color: [u8; 3],
    /// What the picture frames.
    pub frame: OverlayFrame,
}

/// What an overlay's picture frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum OverlayFrame {
    /// The whole drawing, as [`crate::to_svg`] frames it, grown to take in
    /// any change that reaches outside it.
    #[default]
    Drawing,
    /// The changes: every revision cloud, with as much of the drawing
    /// around them again as their box is wide (half its longer side on each
    /// side, but not past what the whole drawing's view shows), drawn at the
    /// stroke width the whole drawing's render would have for a view that
    /// size. A change in a large drawing is otherwise a few pixels of it.
    /// With nothing marked, the whole drawing.
    Changes,
}

impl Default for OverlayOptions {
    fn default() -> Self {
        OverlayOptions {
            svg: ToSvgOptions::default(),
            proposal_color: DEFAULT_PROPOSAL_COLOR,
            frame: OverlayFrame::Drawing,
        }
    }
}

/// The kind of change a mark shows: the change set's own kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum MarkKind {
    Added,
    Removed,
    Modified,
    Unknown,
}

/// One change the overlay drew.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Mark {
    /// The change's index in the change set.
    pub entry: usize,
    pub kind: MarkKind,
    /// The entity in the first state, when the change has one there.
    pub before: Option<EntityId>,
    /// The entity drawn from the second state, when the change has one
    /// there that could be drawn. `None` for an `UNKNOWN` change, whose
    /// counterpart is not decided and is never drawn.
    pub after: Option<EntityId>,
    /// The box, in drawing units, the revision cloud is drawn around: every
    /// extent the change involves (both states of a modified entity, all
    /// candidates of an unknown one), before the cloud's margin.
    pub cloud: Rect,
    /// Why the entity is not drawn from the second state although the
    /// change has it there: the cloud is drawn, the entity is not. `None`
    /// when it is drawn, and for the kinds that draw no second state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_drawn: Option<NotDrawnReason>,
    /// For a change to an entity inside a block definition: the top-level
    /// block reference this mark shows it through. Every reference that
    /// places the block, directly or through other blocks, gets a mark of
    /// its own with the same [`Mark::entry`]; the cloud is around the entity
    /// where that reference puts it. Empty for a top-level entity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub via: Vec<EntityId>,
    /// How large the change is in the picture: the longer side of
    /// [`Mark::cloud`] -- the box the cloud is drawn around, before its
    /// margin -- over the longer side of [`OverlayResult::view_box`]. A
    /// picture `n` pixels across shows the change about `share * n` pixels
    /// long -- a caller that knows its size can tell a change too small to
    /// find, and frame the changes instead ([`OverlayFrame::Changes`]). The
    /// cloud's margin is left out: it is a few strokes wide whatever the
    /// change, and would make every change look findable.
    pub share: f64,
}

/// Why a marked change's entity is not drawn from the second state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum NotDrawnReason {
    /// The entity is drawn from a block the file made for its record -- a
    /// DIMENSION's or an ACAD_TABLE's -- and the change edits the record
    /// itself (any field outside `common`): a measurement, a text, a point,
    /// a style, a cell. The block still shows the record as it was, so
    /// drawing it would present the old values as the change.
    BlockOutOfDate,
}

/// The entity types drawn from a block the file made for the record, which
/// an edit of the record does not redraw.
const DRAWN_FROM_OWN_BLOCK: [&str; 2] = ["DIMENSION", "ACAD_TABLE"];

/// Whether `change` edits a record whose drawing is a block made for it, so
/// that block no longer shows the second state. The entity's own `common`
/// fields (layer, colour, linetype) are the block's too: its children take
/// them by layer 0 and by block.
fn block_out_of_date(change: &Change) -> bool {
    match change {
        Change::Modified(m) => {
            DRAWN_FROM_OWN_BLOCK.contains(&m.entity_type.as_str())
                && m.fields.iter().any(|f| !f.path.starts_with("common."))
        }
        _ => false,
    }
}

/// Why a change is not in the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum NotMarkedReason {
    /// The entity is not a top-level entity of the rendered space, and no
    /// top-level block reference of that space places it: it sits in a
    /// block nothing in the space references (or only a DIMENSION or a
    /// table does), or in a space the render did not draw. An entity in a
    /// block that a top-level INSERT places is marked through it
    /// ([`Mark::via`]).
    NotTopLevel,
    /// The render drew nothing for the entity in either state, so there is
    /// nothing to put a cloud around: a type this renderer does not draw,
    /// one the drawing hides, or one a bound left out.
    NothingDrawn,
}

/// One change the overlay could not draw.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct NotMarked {
    /// The change's index in the change set.
    pub entry: usize,
    pub kind: MarkKind,
    /// The entity the change is about ([`Change::id`]).
    pub id: EntityId,
    pub reason: NotMarkedReason,
}

/// A color of the original drawing close enough to the proposal color that
/// a change drawn in one may not stand out against a line drawn in the
/// other.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ColorConflict {
    /// The original's color, `#rrggbb`.
    pub color: String,
    /// How many times the original layer paints with it (stroke, fill and
    /// gradient-stop attributes), a measure of how much of the drawing it is.
    pub uses: usize,
    /// Its CIELAB distance (CIE76 ΔE) from the proposal color.
    pub delta_e: f64,
}

/// What [`overlay_to_svg`] drew.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct OverlayResult {
    /// The SVG document. Not serialized: a report of the overlay is the
    /// other fields.
    #[serde(skip)]
    pub svg: String,
    /// Every change drawn, in change-set order.
    pub marked: Vec<Mark>,
    /// Every change not drawn, in change-set order, with the reason.
    pub not_marked: Vec<NotMarked>,
    /// The world rectangle the document shows: the original's view box,
    /// grown to take in every cloud.
    pub view_box: Rect,
    /// The world point the document's coordinates are written relative to
    /// -- the original render's, see [`crate::ToSvgResult::origin`].
    pub origin: Point2D,
    /// The stroke width the document is drawn at, in drawing units: what a
    /// caller adding its own marks (a revision symbol by a cloud) sizes
    /// them by.
    pub stroke_width: f64,
    /// The colors the original layer paints with that are hard to tell from
    /// the proposal color (within [`CONFLICT_DELTA_E`]), nearest first. Not
    /// empty means some changes may not stand out from the original; a
    /// caller can pick another [`OverlayOptions::proposal_color`].
    pub proposal_color_conflicts: Vec<ColorConflict>,
    /// The top-level entities of the first state the original layer does
    /// not draw -- the outliers its crop set aside (see
    /// [`ToSvgOptions::crop`]), in drawing order. Not empty means the
    /// picture is not the whole drawing; a change to one of them is still
    /// marked.
    pub left_out: Vec<LeftOut>,
}

/// Draws `changes` -- the change set from `before` to `after`, as
/// [`iron_diff_cad::diff`] returns it or a projection of it -- on top of
/// `before`.
///
/// The first layer is `before` rendered as [`crate::to_svg`] renders it
/// with `options.svg`, byte for byte when no change reaches outside that
/// render's view box (when one does, the view box grows and the layer is
/// the same render written for the larger window). The second layer, in
/// `options.proposal_color`:
///
/// | Change | Drawn |
/// |---|---|
/// | `ADDED` | the entity, from `after`, and a revision cloud |
/// | `MODIFIED` | the entity as it is in `after`, and a cloud around both states -- except a DIMENSION or an ACAD_TABLE whose record the change edits: its block still draws the old record, so only the cloud is drawn ([`NotDrawnReason::BlockOutOfDate`]) |
/// | `REMOVED` | the entity, from `before`, dashed, and a cloud |
/// | `UNKNOWN` | a dashed cloud around the entity and all its candidates -- no geometry, since which candidate it became is not known |
///
/// A change to an entity inside a block definition is shown where every
/// top-level INSERT places it, directly or through other blocks: one mark
/// per reference ([`Mark::via`]), with the entity alone drawn and clouded
/// there. A block only a DIMENSION or a table references, or none, leaves
/// the change in [`OverlayResult::not_marked`] as
/// [`NotMarkedReason::NotTopLevel`].
///
/// The change layer draws lines and text only: fills (solids, hatches) are
/// drawn as outlines. Every change is either in [`OverlayResult::marked`]
/// or in [`OverlayResult::not_marked`] with the reason, never silently
/// left out. The overlay draws every change it is given; to leave out
/// changes within tolerance, pass a projection ([`ChangeSet::without`]).
pub fn overlay_to_svg(
    before: &CadDatabase,
    after: &CadDatabase,
    changes: &ChangeSet,
    options: OverlayOptions,
) -> OverlayResult {
    let b = Scene::new(before, options.svg);
    let a = Scene::new(after, options.svg);
    let b_parts = top_level(&b);
    let a_parts = top_level(&a);

    let mut marked = Vec::new();
    let mut not_marked = Vec::new();
    // Parts to draw in the change layer, and the clouds (world box, dashed).
    let mut drawn_after = BTreeSet::new();
    let mut drawn_before = BTreeSet::new();
    let mut clouds: Vec<(Rect, bool)> = Vec::new();
    // Changes inside block definitions, drawn where each reference places
    // them: the renders of the reduced states, and which parts to draw.
    let mut placed_layers: Vec<PlacedLayer> = Vec::new();

    for (entry, change) in changes.changes.iter().enumerate() {
        let (kind, before_id, after_ids): (MarkKind, Option<EntityId>, Vec<EntityId>) = match change
        {
            Change::Added(e) => (MarkKind::Added, None, vec![e.id]),
            Change::Removed(e) => (MarkKind::Removed, Some(e.id), Vec::new()),
            Change::Modified(m) => (
                MarkKind::Modified,
                Some(m.id),
                vec![m.counterpart.unwrap_or(m.id)],
            ),
            Change::Unknown(u) => (MarkKind::Unknown, Some(u.id), u.candidates.clone()),
        };
        let before_part = before_id.and_then(|id| b_parts.get(&id).copied());
        let after_parts: Vec<usize> = after_ids
            .iter()
            .filter_map(|id| a_parts.get(id).copied())
            .collect();
        if before_part.is_none() && after_parts.is_empty() {
            let after_id = after_ids.first().copied();
            let placed = placements(
                kind,
                before_id.map(|id| (before, id)),
                after_id.map(|id| (after, id)),
                options.svg,
            );
            if placed.marks.is_empty() && placed.reached {
                not_marked.push(NotMarked {
                    entry,
                    kind,
                    id: change.id(),
                    reason: NotMarkedReason::NothingDrawn,
                });
                continue;
            }
            if !placed.marks.is_empty() {
                for (via, cloud, drawn) in placed.marks {
                    clouds.push((cloud, false));
                    marked.push(Mark {
                        entry,
                        kind,
                        before: before_id.filter(|_| kind != MarkKind::Added),
                        after: after_id.filter(|_| drawn),
                        cloud,
                        not_drawn: None,
                        via: vec![via],
                        share: 0.0,
                    });
                }
                placed_layers.extend(placed.layers);
                continue;
            }
            not_marked.push(NotMarked {
                entry,
                kind,
                id: change.id(),
                reason: NotMarkedReason::NotTopLevel,
            });
            continue;
        }

        let before_extent = before_part.and_then(|i| drawn_extent(&b, i));
        let after_extents: Vec<(usize, Rect)> = after_parts
            .iter()
            .filter_map(|&i| drawn_extent(&a, i).map(|r| (i, r)))
            .collect();
        let Some(cloud) = before_extent
            .into_iter()
            .chain(after_extents.iter().map(|&(_, r)| r))
            .reduce(union)
        else {
            not_marked.push(NotMarked {
                entry,
                kind,
                id: change.id(),
                reason: NotMarkedReason::NothingDrawn,
            });
            continue;
        };

        let mut after = None;
        let mut not_drawn = None;
        match kind {
            MarkKind::Added | MarkKind::Modified => {
                if let Some(&(i, _)) = after_extents.first() {
                    if block_out_of_date(change) {
                        not_drawn = Some(NotDrawnReason::BlockOutOfDate);
                    } else {
                        drawn_after.insert(i);
                        after = Some(a.parts[i].id);
                    }
                }
            }
            MarkKind::Removed => {
                if let (Some(i), Some(_)) = (before_part, before_extent) {
                    drawn_before.insert(i);
                }
            }
            MarkKind::Unknown => {}
        }
        clouds.push((cloud, kind == MarkKind::Unknown));
        marked.push(Mark {
            entry,
            kind,
            before: before_id.filter(|_| before_part.is_some()),
            after,
            cloud,
            not_drawn,
            via: Vec::new(),
            share: 0.0,
        });
    }

    // The window, and the stroke width that goes with it.
    let changed = clouds.iter().map(|(r, _)| *r).reduce(union);
    let (window, stroke_width) = match (options.frame, changed) {
        (OverlayFrame::Changes, Some(changed)) => {
            let window = around(changed, &b.view_box);
            // The whole render's stroke for its view box, scaled to this
            // one's: the lines keep the weight they have in the whole picture
            // seen at the same size.
            let ratio = b.auto_stroke_width / diagonal(&b.view_box);
            let auto = if ratio.is_finite() && ratio > 0.0 {
                diagonal(&window) * ratio
            } else {
                b.auto_stroke_width
            };
            let stroke_width = options.svg.stroke_width.unwrap_or(auto);
            let margin = cloud_margin(stroke_width);
            let window = clouds
                .iter()
                .map(|(r, _)| pad(*r, margin))
                .fold(window, union);
            (window, stroke_width)
        }
        _ => {
            // The original's view box, grown to take in every cloud with its
            // margin. Written exactly as the original render framed it when
            // nothing grew it.
            let stroke_width = options.svg.stroke_width.unwrap_or(b.auto_stroke_width);
            let margin = cloud_margin(stroke_width);
            let window = clouds
                .iter()
                .map(|(r, _)| pad(*r, margin))
                .fold(b.view_box, union);
            (window, stroke_width)
        }
    };
    let longer = |r: &Rect| r.width().max(r.height());
    for mark in &mut marked {
        let share = longer(&mark.cloud) / longer(&window);
        mark.share = if share.is_finite() { share } else { 0.0 };
    }
    let view_box = if window == b.view_box {
        b.doc_view_box
    } else {
        doc_view_box(&window, b.origin)
    };

    let original = b.layer(view_box, stroke_width, |i| b.in_document(i));
    let mut layer = String::new();
    if !drawn_after.is_empty() {
        let after_view_box = doc_view_box(&window, a.origin);
        let body = a.layer(after_view_box, stroke_width, |i| drawn_after.contains(&i));
        // The second render is written about its own origin; move it onto
        // the first's.
        let dx = clean(a.origin.x - b.origin.x);
        let dy = clean(b.origin.y - a.origin.y);
        let _ = write!(
            layer,
            "\n  <g transform=\"translate({dx} {dy})\">\n  {body}\n  </g>"
        );
    }
    if !drawn_before.is_empty() {
        let body = b.layer(view_box, stroke_width, |i| drawn_before.contains(&i));
        let _ = write!(layer, "\n  <g class=\"removed\">\n  {body}\n  </g>");
    }
    for placed in &placed_layers {
        let scene = &placed.scene;
        let body = scene.layer(doc_view_box(&window, scene.origin), stroke_width, |i| {
            placed.parts.contains(&i)
        });
        let dx = clean(scene.origin.x - b.origin.x);
        let dy = clean(b.origin.y - scene.origin.y);
        let class = if placed.removed {
            " class=\"removed\""
        } else {
            ""
        };
        let _ = write!(
            layer,
            "\n  <g{class} transform=\"translate({dx} {dy})\">\n  {body}\n  </g>"
        );
    }
    for (rect, dashed) in &clouds {
        let class = if *dashed { "cloud unknown" } else { "cloud" };
        let d = cloud_path(
            pad(*rect, cloud_margin(stroke_width)),
            b.origin,
            stroke_width,
        );
        let _ = write!(layer, "\n  <path class=\"{class}\" d=\"{d}\"/>");
    }

    let [x, y, width, height] = view_box;
    let [r, g, bl] = options.proposal_color;
    let color = format!("#{r:02x}{g:02x}{bl:02x}");
    let dash = format!(
        "{} {}",
        clean(stroke_width * DASH),
        clean(stroke_width * GAP)
    );
    let style = format!(
        "<style>#changes *{{stroke:{color};fill:none}}#changes text,#changes tspan{{fill:{color};stroke:none}}#changes .removed *,#changes .unknown{{stroke-dasharray:{dash}}}</style>"
    );
    let defs = b.defs_block(stroke_width);
    let left_out = b
        .crop
        .left_out
        .iter()
        .filter(|l| l.reason.is_set_aside())
        .cloned()
        .collect();
    let proposal_color_conflicts = color_conflicts(
        options.proposal_color,
        colors_used(&defs).into_iter().chain(colors_used(&original)),
    );
    let root_stroke = root_stroke(options.svg.paper);
    let page = page(options.svg.paper, view_box);
    let origin = origin_attribute(b.origin);
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{x} {y} {width} {height}\" data-origin=\"{origin}\" stroke=\"{root_stroke}\" stroke-width=\"{stroke_width}\">\n  {style}\n  {page}{defs}<g id=\"original\">\n  {original}\n</g>\n  <g id=\"changes\">{layer}\n</g>\n</svg>"
    );

    OverlayResult {
        svg,
        marked,
        not_marked,
        view_box: window,
        origin: b.origin,
        stroke_width,
        proposal_color_conflicts,
        left_out,
    }
}

/// How close, as a CIELAB distance (CIE76 ΔE), an original color may come
/// to the proposal color before the two are reported as hard to tell apart.
/// There is no standard threshold for "confused at a glance" (2.3 is the
/// just-noticeable difference); 30 takes in pure red (#ff0000 is 23.9 from
/// the default proposal color) and dark or light reds, and leaves orange
/// (46.5), rose (42.0) and magenta (109.0) out.
pub const CONFLICT_DELTA_E: f64 = 30.0;

/// Every hex color an SVG fragment paints with -- `stroke`, `fill` and
/// gradient `stop-color` attributes -- and how many times.
fn colors_used(svg: &str) -> BTreeMap<[u8; 3], usize> {
    let mut used = BTreeMap::new();
    for attribute in ["stroke=\"#", "fill=\"#", "stop-color=\"#"] {
        for (at, _) in svg.match_indices(attribute) {
            let hex = &svg[at + attribute.len()..];
            let Some(hex) = hex
                .get(..6)
                .filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
            else {
                continue;
            };
            let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0);
            *used
                .entry([channel(0), channel(2), channel(4)])
                .or_insert(0) += 1;
        }
    }
    used
}

/// The colors of `used` within [`CONFLICT_DELTA_E`] of `proposal`, nearest
/// first (then by color, so the order does not depend on the drawing).
fn color_conflicts(
    proposal: [u8; 3],
    used: impl IntoIterator<Item = ([u8; 3], usize)>,
) -> Vec<ColorConflict> {
    let mut merged: BTreeMap<[u8; 3], usize> = BTreeMap::new();
    for (color, uses) in used {
        *merged.entry(color).or_insert(0) += uses;
    }
    let target = lab(proposal);
    let mut conflicts: Vec<ColorConflict> = merged
        .into_iter()
        .filter_map(|(color, uses)| {
            let c = lab(color);
            let delta_e = ((c[0] - target[0]).powi(2)
                + (c[1] - target[1]).powi(2)
                + (c[2] - target[2]).powi(2))
            .sqrt();
            (delta_e < CONFLICT_DELTA_E).then(|| ColorConflict {
                color: format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2]),
                uses,
                delta_e,
            })
        })
        .collect();
    conflicts.sort_by(|a, b| a.delta_e.total_cmp(&b.delta_e).then(a.color.cmp(&b.color)));
    conflicts
}

/// An sRGB color in CIELAB (D65 white point).
fn lab(rgb: [u8; 3]) -> [f64; 3] {
    let linear = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (linear(rgb[0]), linear(rgb[1]), linear(rgb[2]));
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f64| {
        if t > 216.0 / 24389.0 {
            t.cbrt()
        } else {
            (24389.0 / 27.0 * t + 16.0) / 116.0
        }
    };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// A dash and a gap of the dashed marks, in stroke widths.
const DASH: f64 = 6.0;
const GAP: f64 = 4.0;
/// The revision cloud's margin around the box it marks, and the length of
/// one of its arcs, in stroke widths.
const MARGIN: f64 = 10.0;
const ARC: f64 = 16.0;
/// The most arcs one side of a cloud has: a cloud around a large box gets
/// longer arcs rather than thousands of them.
const MAX_ARCS_PER_SIDE: usize = 100;

fn cloud_margin(stroke_width: f64) -> f64 {
    stroke_width * MARGIN
}

/// One reduced state's render, and the parts of it the change layer draws.
struct PlacedLayer {
    scene: Scene,
    parts: BTreeSet<usize>,
    /// Drawn dashed: the first state of a removed entity.
    removed: bool,
}

/// What [`placements`] found: a mark per top-level reference (the
/// reference, the cloud, whether the second state is drawn there) and the
/// layers that draw them.
struct Placed {
    marks: Vec<(EntityId, Rect, bool)>,
    layers: Vec<PlacedLayer>,
    /// Whether a top-level INSERT reaches the entity in either state --
    /// when none drew it, the change is drawn nowhere, not out of reach.
    reached: bool,
}

/// A change to an entity inside a block definition, shown where every
/// top-level INSERT places it.
///
/// Each state is reduced to what reaches the entity -- the top-level
/// INSERTs whose block holds it directly or through other blocks, and in
/// each such block only the entity and the INSERTs that lead to it -- and
/// rendered as [`crate::to_svg`] would, with no crop. A part of that render
/// is one top-level INSERT drawing the entity alone, at every place the
/// reference puts it.
fn placements(
    kind: MarkKind,
    before: Option<(&CadDatabase, EntityId)>,
    after: Option<(&CadDatabase, EntityId)>,
    options: ToSvgOptions,
) -> Placed {
    let options = ToSvgOptions {
        crop: Crop::Everything,
        ..options
    };
    let scene = |state: Option<(&CadDatabase, EntityId)>| {
        state
            .and_then(|(db, id)| reduced_to(db, id))
            .map(|db| Scene::new(&db, options))
    };
    let (b, a) = match kind {
        MarkKind::Added => (None, scene(after)),
        MarkKind::Removed => (scene(before), None),
        MarkKind::Modified => (scene(before), scene(after)),
        MarkKind::Unknown => (None, None),
    };
    let extents = |scene: &Option<Scene>| -> BTreeMap<EntityId, (usize, Rect)> {
        scene
            .iter()
            .flat_map(|s| {
                top_level(s)
                    .into_iter()
                    .filter_map(|(id, i)| drawn_extent(s, i).map(|r| (id, (i, r))))
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    let reached = b.is_some() || a.is_some();
    let (b_extents, a_extents) = (extents(&b), extents(&a));
    let references: BTreeSet<EntityId> =
        b_extents.keys().chain(a_extents.keys()).copied().collect();
    let mut marks = Vec::new();
    let (mut b_parts, mut a_parts) = (BTreeSet::new(), BTreeSet::new());
    for reference in references {
        let before = b_extents.get(&reference);
        let after = a_extents.get(&reference);
        let Some(cloud) = before
            .map(|x| x.1)
            .into_iter()
            .chain(after.map(|x| x.1))
            .reduce(union)
        else {
            continue;
        };
        if let Some(&(i, _)) = after {
            a_parts.insert(i);
        }
        if let (MarkKind::Removed, Some(&(i, _))) = (kind, before) {
            b_parts.insert(i);
        }
        marks.push((reference, cloud, after.is_some()));
    }
    let mut layers = Vec::new();
    if let Some(scene) = a.filter(|_| !a_parts.is_empty()) {
        layers.push(PlacedLayer {
            scene,
            parts: a_parts,
            removed: false,
        });
    }
    if let Some(scene) = b.filter(|_| !b_parts.is_empty()) {
        layers.push(PlacedLayer {
            scene,
            parts: b_parts,
            removed: true,
        });
    }
    Placed {
        marks,
        layers,
        reached,
    }
}

/// `db` reduced to what reaches entity `id` inside a block definition, or
/// `None` when `id` is top-level or no top-level INSERT reaches it.
fn reduced_to(db: &CadDatabase, id: EntityId) -> Option<CadDatabase> {
    if db.entities.iter().any(|e| e.common().id == id) {
        return None;
    }
    let blocks = &db.tables.block_records;
    let holds = |name: &str| {
        blocks
            .get(name)
            .is_some_and(|b| b.entities.iter().any(|e| e.common().id == id))
    };
    // The blocks that place the entity: holding it, or holding an INSERT of
    // a block that does -- grown until nothing more joins.
    let mut reaching: BTreeSet<&str> = blocks
        .keys()
        .map(String::as_str)
        .filter(|name| !is_space(name) && holds(name))
        .collect();
    if reaching.is_empty() {
        return None;
    }
    loop {
        let more: Vec<&str> = blocks
            .iter()
            .filter(|(name, _)| !is_space(name) && !reaching.contains(name.as_str()))
            .filter(|(_, b)| b.entities.iter().any(|e| inserts_one_of(e, &reaching)))
            .map(|(name, _)| name.as_str())
            .collect();
        if more.is_empty() {
            break;
        }
        reaching.extend(more);
    }
    let mut out = db.clone();
    out.entities.retain(|e| inserts_one_of(e, &reaching));
    if out.entities.is_empty() {
        return None;
    }
    for (name, block) in out.tables.block_records.iter_mut() {
        if !reaching.contains(name.as_str()) {
            continue;
        }
        block
            .entities
            .retain(|e| e.common().id == id || inserts_one_of(e, &reaching));
        // A nested INSERT's attribute values are drawn beside it; they are
        // not the entity.
        for e in block.entities.iter_mut() {
            if let Entity::Insert(i) = e {
                i.attribs.clear();
            }
        }
    }
    Some(out)
}

/// Whether `e` is an INSERT of one of `blocks`.
fn inserts_one_of(e: &Entity, blocks: &BTreeSet<&str>) -> bool {
    match e {
        Entity::Insert(i) => i
            .block_name
            .resolved()
            .is_some_and(|n| blocks.contains(n.as_str())),
        _ => false,
    }
}

/// A space's own block record (`*Model_Space`, `*Paper_Space…`): its
/// entities are top-level, not a definition placed by reference.
fn is_space(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("*model_space") || n.starts_with("*paper_space")
}

/// Every top-level part of `scene` by its entity's reference ID. A layout's
/// view through a viewport is not an entity of the drawing and is left out.
fn top_level(scene: &Scene) -> BTreeMap<EntityId, usize> {
    scene
        .parts
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.through_viewport)
        .map(|(i, p)| (p.id, i))
        .collect()
}

/// The world box part `i` of `scene` covers, when it drew something.
fn drawn_extent(scene: &Scene, i: usize) -> Option<Rect> {
    let part = &scene.parts[i];
    if part.drawn {
        part.extent
    } else {
        None
    }
}

fn union(a: Rect, b: Rect) -> Rect {
    Rect::new(
        a.min_x.min(b.min_x),
        a.min_y.min(b.min_y),
        a.max_x.max(b.max_x),
        a.max_y.max(b.max_y),
    )
}

fn diagonal(r: &Rect) -> f64 {
    r.width().hypot(r.height())
}

/// The window [`OverlayFrame::Changes`] shows around `changed`: half the longer
/// side again on each side, but no more of the drawing than its whole view
/// shows. A box with no extent -- one point changed -- gets a window a
/// fiftieth of the whole drawing's diagonal across.
fn around(changed: Rect, drawing: &Rect) -> Rect {
    let longer = changed.width().max(changed.height());
    let context = if longer > 0.0 {
        longer / 2.0
    } else {
        diagonal(drawing) / 100.0
    };
    let wanted = pad(changed, context);
    // The context is cut to the drawing; the change itself never is (the
    // clouds are added back by the caller).
    let cut = Rect::new(
        wanted.min_x.max(drawing.min_x.min(changed.min_x)),
        wanted.min_y.max(drawing.min_y.min(changed.min_y)),
        wanted.max_x.min(drawing.max_x.max(changed.max_x)),
        wanted.max_y.min(drawing.max_y.max(changed.max_y)),
    );
    union(cut, changed)
}

fn pad(r: Rect, by: f64) -> Rect {
    Rect::new(r.min_x - by, r.min_y - by, r.max_x + by, r.max_y + by)
}

/// A revision cloud around the world box `rect`, as a path in the document
/// frame of a render written about `origin`: the box's outline walked
/// clockwise, each side cut into equal chords, each chord bulging outward
/// as a circular arc.
fn cloud_path(rect: Rect, origin: Point2D, stroke_width: f64) -> String {
    // Document frame: x right, y down.
    let left = rect.min_x - origin.x;
    let right = rect.max_x - origin.x;
    let top = origin.y - rect.max_y;
    let bottom = origin.y - rect.min_y;
    let corners = [(left, top), (right, top), (right, bottom), (left, bottom)];
    let target = stroke_width * ARC;
    let mut d = format!("M{} {}", clean(left), clean(top));
    for side in 0..4 {
        let (x0, y0) = corners[side];
        let (x1, y1) = corners[(side + 1) % 4];
        let length = (x1 - x0).hypot(y1 - y0);
        let arcs = arcs_on(length, target);
        let radius = clean(length / arcs as f64 * 0.6);
        for k in 1..=arcs {
            let t = k as f64 / arcs as f64;
            let x = clean(x0 + (x1 - x0) * t);
            let y = clean(y0 + (y1 - y0) * t);
            // Sweep 1 is clockwise on the page: walking the box clockwise,
            // that bows every arc outward.
            let _ = write!(d, "A{radius} {radius} 0 0 1 {x} {y}");
        }
    }
    d.push('Z');
    d
}

/// How many arcs a side of `length` gets: sides at least one arc, at most
/// [`MAX_ARCS_PER_SIDE`], each close to `target` long.
fn arcs_on(length: f64, target: f64) -> usize {
    if !(length.is_finite() && target.is_finite() && target > 0.0) {
        return 1;
    }
    let n = (length / target).ceil();
    if n < 1.0 {
        1
    } else if n > MAX_ARCS_PER_SIDE as f64 {
        MAX_ARCS_PER_SIDE
    } else {
        n as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_side_gets_between_one_and_the_cap_arcs() {
        assert_eq!(arcs_on(0.0, 1.0), 1);
        assert_eq!(arcs_on(10.0, 1.0), 10);
        assert_eq!(arcs_on(10.5, 1.0), 11);
        assert_eq!(arcs_on(1e9, 1.0), MAX_ARCS_PER_SIDE);
        assert_eq!(arcs_on(f64::NAN, 1.0), 1);
        assert_eq!(arcs_on(10.0, 0.0), 1);
    }

    #[test]
    fn a_cloud_walks_the_box_clockwise_and_closes() {
        let d = cloud_path(
            Rect::new(0.0, 0.0, 4.0, 2.0),
            Point2D { x: 0.0, y: 0.0 },
            0.125,
        );
        // Top-left corner of the page: x 0, y -2 (y down about the origin).
        assert!(d.starts_with("M0 -2A"), "{d}");
        assert!(d.ends_with("0 -2Z"), "{d}");
        // 4 / 2 arcs on the long sides, 2 / 2 on the short ones.
        assert_eq!(d.matches('A').count(), 2 + 1 + 2 + 1, "{d}");
    }

    #[test]
    fn colors_are_counted_by_attribute_and_named_colors_are_not_hex() {
        let svg = concat!(
            r##"<line stroke="#FF0000"/><text fill="#ff0000" stroke="none">x</text>"##,
            r##"<stop stop-color="#00ff00"/><g stroke="black"><path fill="#12"/></g>"##,
        );
        let used = colors_used(svg);
        assert_eq!(used.get(&[0xff, 0, 0]), Some(&2));
        assert_eq!(used.get(&[0, 0xff, 0]), Some(&1));
        assert_eq!(used.len(), 2, "{used:?}");
    }

    #[test]
    fn conflicts_are_the_close_colors_nearest_first() {
        let used = [
            ([0xff, 0, 0], 4),
            ([0xcc, 0, 0], 1),
            ([0xff, 0x7f, 0], 9),
            ([0, 0, 0], 3),
        ];
        let conflicts = color_conflicts(DEFAULT_PROPOSAL_COLOR, used);
        let colors: Vec<&str> = conflicts.iter().map(|c| c.color.as_str()).collect();
        // #cc0000 is 14.7 away, #ff0000 23.9; orange (46.5) and black stay out.
        assert_eq!(colors, ["#cc0000", "#ff0000"]);
        assert_eq!(conflicts[1].uses, 4);
        assert_eq!(
            color_conflicts(DEFAULT_PROPOSAL_COLOR, [(DEFAULT_PROPOSAL_COLOR, 1)])[0].delta_e,
            0.0
        );
    }
}
