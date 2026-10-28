//! Numbered balloons drawn on top of the original: a circle holding a label,
//! joined by a leader to the thing it points at.
//!
//! The document holds two layers. The first is the original, drawn exactly
//! as [`crate::to_svg`] draws it on its own. The second holds the balloons,
//! in one colour. The caller supplies every label -- a balloon is a number
//! the caller assigned, written as given (`5`, `5.1`) -- and the renderer
//! only decides where the circle sits. Nothing is guessed: a balloon the
//! picture cannot show is reported by name, and one that has no free spot is
//! drawn anyway, overlapping, and says so, since a number that vanishes from
//! the picture breaks its pairing with the table it belongs to.

use super::format::{clean, escape_xml};
use super::overlay::{
    choose_proposal, colors_used, drawn_extent, pad, placements, top_level, union, ColorConflict,
    MarkKind, ProposalColor,
};
use super::scene::{doc_view_box, origin_attribute, page, root_stroke, Rect, Scene};
use super::ToSvgOptions;
use serde::Serialize;
use std::fmt::Write as _;
use uncad_model::model::{EntityId, Point2D};
use uncad_model::CadDatabase;

/// What a balloon points at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BalloonTarget {
    /// This entity: the leader ends at the centre of the box the entity
    /// covers in the picture. An entity inside a block definition gets one
    /// balloon at every top-level reference that places the block.
    Entity(EntityId),
    /// This point, in drawing units -- where no entity is left to point at
    /// (a characteristic that was dropped), or a place inside an entity.
    Point(Point2D),
}

/// How a balloon is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum BalloonState {
    /// A solid circle and leader.
    #[default]
    Active,
    /// A dashed circle and leader: the number is kept, the thing it
    /// numbered is gone.
    Dropped,
}

/// One balloon to place.
#[derive(Debug, Clone, PartialEq)]
pub struct Balloon {
    /// The text in the circle, written as given. The renderer does not read
    /// it: `5.1` is not a number to it.
    pub label: String,
    pub target: BalloonTarget,
    pub state: BalloonState,
}

impl Balloon {
    /// An active balloon on `target`.
    pub fn new(label: impl Into<String>, target: BalloonTarget) -> Balloon {
        Balloon {
            label: label.into(),
            target,
            state: BalloonState::Active,
        }
    }
}

/// The circle's size.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[non_exhaustive]
pub enum BalloonSize {
    /// A diameter of one sixtieth of the original's view box diagonal: the
    /// circle keeps its size in the picture whatever the drawing's units.
    #[default]
    Auto,
    /// This diameter, in drawing units.
    Units(f64),
}

/// How [`balloons_to_svg`] draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BalloonOptions {
    /// How the original is rendered, with the same meaning as for
    /// [`crate::to_svg`]; the balloon layer is drawn at the same stroke
    /// width.
    pub svg: ToSvgOptions,
    /// The colour of the balloon layer, chosen against the original's
    /// colours as for a redline ([`crate::overlay_to_svg`]).
    pub color: ProposalColor,
    pub size: BalloonSize,
}

impl Default for BalloonOptions {
    fn default() -> Self {
        BalloonOptions {
            svg: ToSvgOptions::default(),
            color: ProposalColor::Auto,
            size: BalloonSize::Auto,
        }
    }
}

/// A balloon that was drawn.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct PlacedBalloon {
    /// The balloon's index in the input.
    pub index: usize,
    /// The label, as given.
    pub label: String,
    /// For a balloon on an entity inside a block definition: the top-level
    /// reference this balloon is at. One input balloon gives one placed
    /// balloon per reference; empty otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub via: Vec<EntityId>,
    /// The circle's centre and radius, in drawing units.
    pub center: Point2D,
    pub radius: f64,
    /// Where the leader ends: the point the balloon points at.
    pub leader_end: Point2D,
    /// No free spot was found: the circle overlaps a text of the original or
    /// another balloon. It is drawn there all the same.
    pub crowded: bool,
}

/// Why a balloon could not be drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum NotPlacedReason {
    /// The render drew nothing for the entity: a type this renderer does not
    /// draw, one the drawing hides, or one a bound left out.
    EntityNotDrawn,
    /// The entity is not a top-level entity of the rendered space, and no
    /// top-level block reference of that space places it.
    NotTopLevel,
    /// The point is not a number, or lies where nothing can be drawn.
    NoPoint,
}

/// A balloon that was not drawn.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct NotPlaced {
    /// The balloon's index in the input.
    pub index: usize,
    pub label: String,
    pub reason: NotPlacedReason,
}

/// What [`balloons_to_svg`] drew.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct BalloonResult {
    /// The SVG document. Not serialized: a report of the picture is the
    /// other fields.
    #[serde(skip)]
    pub svg: String,
    /// Every balloon drawn, in input order (one per reference for a balloon
    /// on an entity in a block definition).
    pub placed: Vec<PlacedBalloon>,
    /// Every balloon not drawn, in input order, with the reason.
    pub not_placed: Vec<NotPlaced>,
    /// The world rectangle the document shows: the original's view box,
    /// grown to take in every balloon.
    pub view_box: Rect,
    /// The world point the document's coordinates are written relative to.
    pub origin: Point2D,
    /// The stroke width the document is drawn at, in drawing units.
    pub stroke_width: f64,
    /// The colour of the balloon layer, `#rrggbb`.
    pub color: String,
    /// The original's colours that are hard to tell from [`Self::color`].
    pub color_conflicts: Vec<ColorConflict>,
}

/// The unit vectors a balloon may sit along from its target, clockwise from
/// straight up (the drawing's y is up).
const DIRECTIONS: [(f64, f64); 8] = [
    (0.0, 1.0),
    (
        std::f64::consts::FRAC_1_SQRT_2,
        std::f64::consts::FRAC_1_SQRT_2,
    ),
    (1.0, 0.0),
    (
        std::f64::consts::FRAC_1_SQRT_2,
        -std::f64::consts::FRAC_1_SQRT_2,
    ),
    (0.0, -1.0),
    (
        -std::f64::consts::FRAC_1_SQRT_2,
        -std::f64::consts::FRAC_1_SQRT_2,
    ),
    (-1.0, 0.0),
    (
        -std::f64::consts::FRAC_1_SQRT_2,
        std::f64::consts::FRAC_1_SQRT_2,
    ),
];

/// How far from its target a circle's centre sits, in radii, tried in order.
const DISTANCES: [f64; 3] = [2.5, 4.0, 6.0];

/// The label's font size in radii, before it is shrunk to fit a long one.
const LABEL_EM: f64 = 1.1;

/// How much of a circle's width a label may fill.
const LABEL_FILL: f64 = 1.7;

/// A character's width in ems, the estimate the renderer uses for texts.
const CHAR_EM: f64 = 0.6;

const DASH: f64 = 6.0;
const GAP: f64 = 4.0;

/// Draws `balloons` on top of `db`.
///
/// The first layer is `db` rendered as [`crate::to_svg`] renders it with
/// `options.svg`, byte for byte when no balloon reaches outside that
/// render's view box (when one does, the view box grows). The second layer
/// holds one balloon each: a circle with the caller's label, and a leader to
/// the target.
///
/// Balloons are placed in input order, which makes the picture
/// deterministic: for each one, the first of eight directions (up, then
/// clockwise) at three distances from its target whose circle clears every
/// text of the original and every balloon already placed. When none does,
/// the first is used and the balloon is reported [`PlacedBalloon::crowded`]
/// -- a balloon is never left out of the picture for lack of room.
///
/// Every balloon is either in [`BalloonResult::placed`] or in
/// [`BalloonResult::not_placed`] with the reason.
pub fn balloons_to_svg(
    db: &CadDatabase,
    balloons: &[Balloon],
    options: BalloonOptions,
) -> BalloonResult {
    let scene = Scene::new(db, options.svg);
    let parts = top_level(&scene);
    let stroke_width = options.svg.stroke_width.unwrap_or(scene.auto_stroke_width);
    let radius = match options.size {
        BalloonSize::Units(d) if d.is_finite() && d > 0.0 => d / 2.0,
        _ => {
            let v = &scene.view_box;
            let diagonal = v.width().hypot(v.height());
            if diagonal.is_finite() && diagonal > 0.0 {
                diagonal / 120.0
            } else {
                1.0
            }
        }
    };

    // What a circle must stay clear of: the texts of the original (by the
    // box the render estimated for each).
    let mut obstacles: Vec<Rect> = scene
        .texts
        .iter()
        .filter_map(|t| t.estimate.map(Rect::from))
        .collect();

    let mut placed: Vec<PlacedBalloon> = Vec::new();
    let mut not_placed: Vec<NotPlaced> = Vec::new();
    // The circles already placed, as (centre, radius).
    let mut circles: Vec<(Point2D, f64)> = Vec::new();

    for (index, balloon) in balloons.iter().enumerate() {
        let targets: Result<Vec<(Vec<EntityId>, Point2D)>, NotPlacedReason> = match balloon.target {
            BalloonTarget::Point(p) => {
                if p.x.is_finite() && p.y.is_finite() {
                    Ok(vec![(Vec::new(), p)])
                } else {
                    Err(NotPlacedReason::NoPoint)
                }
            }
            BalloonTarget::Entity(id) => entity_targets(db, &scene, &parts, id, options.svg),
        };
        let targets = match targets {
            Ok(t) => t,
            Err(reason) => {
                not_placed.push(NotPlaced {
                    index,
                    label: balloon.label.clone(),
                    reason,
                });
                continue;
            }
        };
        for (via, end) in targets {
            let (center, crowded) = free_spot(end, radius, &obstacles, &circles);
            circles.push((center, radius));
            obstacles.push(Rect::new(
                center.x - radius,
                center.y - radius,
                center.x + radius,
                center.y + radius,
            ));
            placed.push(PlacedBalloon {
                index,
                label: balloon.label.clone(),
                via,
                center,
                radius,
                leader_end: end,
                crowded,
            });
        }
    }

    // The window: the original's view box, grown to take in every circle and
    // every leader end -- a point given outside the drawing is where its
    // leader must reach, not only where its circle sits.
    let window = placed
        .iter()
        .flat_map(|p| {
            [
                pad(
                    Rect::new(p.center.x, p.center.y, p.center.x, p.center.y),
                    p.radius + stroke_width,
                ),
                pad(
                    Rect::new(
                        p.leader_end.x,
                        p.leader_end.y,
                        p.leader_end.x,
                        p.leader_end.y,
                    ),
                    stroke_width,
                ),
            ]
        })
        .fold(scene.view_box, union);
    let view_box = if window == scene.view_box {
        scene.doc_view_box
    } else {
        doc_view_box(&window, scene.origin)
    };

    let original = scene.layer(view_box, stroke_width, |i| scene.in_document(i));
    let defs = scene.defs_block(stroke_width);
    let mut used = colors_used(&defs);
    for (c, uses) in colors_used(&original) {
        *used.entry(c).or_insert(0) += uses;
    }
    let (chosen, color_conflicts) = choose_proposal(options.color, &used);
    let [r, g, b] = chosen;
    let color = format!("#{r:02x}{g:02x}{b:02x}");

    let mut layer = String::new();
    for (balloon, p) in placed.iter().map(|p| (&balloons[p.index], p)) {
        let class = match balloon.state {
            BalloonState::Active => "balloon",
            BalloonState::Dropped => "balloon dropped",
        };
        let (cx, cy) = (p.center.x - scene.origin.x, scene.origin.y - p.center.y);
        let (ex, ey) = (
            p.leader_end.x - scene.origin.x,
            scene.origin.y - p.leader_end.y,
        );
        // The leader starts on the circle's edge, on the line to the target.
        let (dx, dy) = (ex - cx, ey - cy);
        let length = dx.hypot(dy);
        let (sx, sy) = if length > p.radius {
            (cx + dx / length * p.radius, cy + dy / length * p.radius)
        } else {
            (cx, cy)
        };
        let chars = balloon.label.chars().count().max(1) as f64;
        let em = (p.radius * LABEL_EM).min(p.radius * LABEL_FILL / (CHAR_EM * chars));
        let label = escape_xml(&balloon.label);
        let _ = write!(
            layer,
            "\n  <g class=\"{class}\"><line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"/><circle cx=\"{}\" cy=\"{}\" r=\"{}\"/><text x=\"{}\" y=\"{}\" font-size=\"{}\" text-anchor=\"middle\">{label}</text></g>",
            clean(sx),
            clean(sy),
            clean(ex),
            clean(ey),
            clean(cx),
            clean(cy),
            clean(p.radius),
            clean(cx),
            // Text sits on its baseline: a capital is about 0.7 em tall, so
            // the baseline goes 0.35 em below the centre.
            clean(cy + em * 0.35),
            clean(em),
        );
    }

    let dash = format!(
        "{} {}",
        clean(stroke_width * DASH),
        clean(stroke_width * GAP)
    );
    let paper = options.svg.paper.hex();
    let style = format!(
        "<style>#balloons *{{stroke:{color};fill:none}}#balloons circle{{fill:{paper}}}#balloons text{{fill:{color};stroke:none}}#balloons .dropped line,#balloons .dropped circle{{stroke-dasharray:{dash}}}</style>"
    );
    let [x, y, width, height] = view_box;
    let root_stroke = root_stroke(options.svg.paper);
    let page = page(options.svg.paper, view_box);
    let origin = origin_attribute(scene.origin);
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{x} {y} {width} {height}\" data-origin=\"{origin}\" stroke=\"{root_stroke}\" stroke-width=\"{stroke_width}\">\n  {style}\n  {page}{defs}<g id=\"original\">\n  {original}\n</g>\n  <g id=\"balloons\">{layer}\n</g>\n</svg>"
    );

    BalloonResult {
        svg: super::with_screen_stroke(svg, options.svg.screen_stroke),
        placed,
        not_placed,
        view_box: window,
        origin: scene.origin,
        stroke_width,
        color,
        color_conflicts,
    }
}

/// Where the leaders of a balloon on `id` end: the centre of the box the
/// entity covers, once for a top-level entity, once per top-level reference
/// for one inside a block definition.
fn entity_targets(
    db: &CadDatabase,
    scene: &Scene,
    parts: &std::collections::BTreeMap<EntityId, usize>,
    id: EntityId,
    svg: ToSvgOptions,
) -> Result<Vec<(Vec<EntityId>, Point2D)>, NotPlacedReason> {
    let centre = |r: Rect| Point2D {
        x: (r.min_x + r.max_x) / 2.0,
        y: (r.min_y + r.max_y) / 2.0,
    };
    if let Some(&i) = parts.get(&id) {
        return match drawn_extent(scene, i) {
            Some(r) => Ok(vec![(Vec::new(), centre(r))]),
            None => Err(NotPlacedReason::EntityNotDrawn),
        };
    }
    let found = placements(MarkKind::Added, None, Some((db, id)), svg);
    if found.marks.is_empty() {
        return Err(if found.reached {
            NotPlacedReason::EntityNotDrawn
        } else {
            NotPlacedReason::NotTopLevel
        });
    }
    Ok(found
        .marks
        .into_iter()
        .map(|(via, cloud, _)| (vec![via], centre(cloud)))
        .collect())
}

/// The centre of a circle of `radius` for a leader ending at `end`: the
/// first candidate that clears every obstacle and every circle already
/// placed, and `false`; when none does, the first candidate and `true`.
fn free_spot(
    end: Point2D,
    radius: f64,
    obstacles: &[Rect],
    circles: &[(Point2D, f64)],
) -> (Point2D, bool) {
    let mut first = None;
    for distance in DISTANCES {
        for (ux, uy) in DIRECTIONS {
            let center = Point2D {
                x: end.x + ux * radius * distance,
                y: end.y + uy * radius * distance,
            };
            first.get_or_insert(center);
            let clear_of_texts = obstacles
                .iter()
                .all(|r| !circle_meets_rect(center, radius, r));
            let clear_of_circles = circles
                .iter()
                .all(|(c, other)| (center.x - c.x).hypot(center.y - c.y) >= radius + other);
            if clear_of_texts && clear_of_circles {
                return (center, false);
            }
        }
    }
    (first.expect("there are candidates"), true)
}

/// Whether the circle and the rectangle share a point.
fn circle_meets_rect(center: Point2D, radius: f64, r: &Rect) -> bool {
    let nearest_x = center.x.clamp(r.min_x, r.max_x);
    let nearest_y = center.y.clamp(r.min_y, r.max_y);
    (center.x - nearest_x).hypot(center.y - nearest_y) < radius
}
