//! A change set drawn on top of the original: the redline.
//!
//! The document holds two layers. The first is the original, drawn exactly
//! as [`crate::to_svg`] draws it on its own. The second holds the changes,
//! in one proposal colour: what an entity became (or the entity that was
//! added), what was removed as a dashed outline, and a revision cloud
//! around every change. Nothing in the second layer is guessed: a change
//! whose counterpart is uncertain gets a cloud and no geometry, and a change
//! the picture cannot show is reported by name rather than dropped.

use super::format::clean;
use super::scene::{doc_view_box, Rect, Scene};
use super::ToSvgOptions;
use iron_diff_cad::{Change, ChangeSet};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use uncad_model::model::{EntityId, Point2D};
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
    /// automatic one, and the second state is drawn at the same width.
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
}

/// Why a change is not in the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum NotMarkedReason {
    /// The entity is not a top-level entity of the rendered space: it sits
    /// inside a block definition, where every reference to the block places
    /// it somewhere else, or in a space the render did not draw.
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
/// | `MODIFIED` | the entity as it is in `after`, and a cloud around both states |
/// | `REMOVED` | the entity, from `before`, dashed, and a cloud |
/// | `UNKNOWN` | a dashed cloud around the entity and all its candidates -- no geometry, since which candidate it became is not known |
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
        match kind {
            MarkKind::Added | MarkKind::Modified => {
                if let Some(&(i, _)) = after_extents.first() {
                    drawn_after.insert(i);
                    after = Some(a.parts[i].id);
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
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{x} {y} {width} {height}\" stroke=\"black\" stroke-width=\"{stroke_width}\">\n  {style}\n  {defs}<g id=\"original\">\n  {original}\n</g>\n  <g id=\"changes\">{layer}\n</g>\n</svg>"
    );

    OverlayResult {
        svg,
        marked,
        not_marked,
        view_box: window,
        origin: b.origin,
        stroke_width,
    }
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
}
