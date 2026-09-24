//! A paper layout drawn as its sheet: the layout's own entities -- the
//! border, the title block, the viewport frames -- on the paper its limits
//! or plot settings describe, and the model shown through each viewport at
//! the viewport's scale and twist, clipped to its frame.
//!
//! A viewport is a window onto the model: its view is centred at a point of
//! the view's own display coordinates, measured from a target point in the
//! world and turned by a twist, and shows a given height of the model in
//! the frame's height of paper. So the model is drawn inside a group whose
//! matrix is that map, with every stroke, cross and pattern line kept at
//! the sheet's stroke width through it: to the walk, a viewport is one more
//! placement, like a block reference's, whose "world" is the paper.

use super::bounds::{intersects, Box2D};
use super::format::{clean, Frame};
use super::{
    choose_origin, crop_parts, crop_report, infinite, invert_point, scene,
    select_entities_for_space, select_owned_by, stroke_width_placeholder, svg_matrix, view_box_of,
    walk, Ctx, Framed, Part, Rect, Scene, Space, ToSvgOptions,
};
use crate::limits::MAX_WORLD_COORDINATE;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use uncad_model::model::{Entity, EntityId, Point2D, ViewportEntity, ViewportView};
use uncad_model::tables::{LayoutRecord, PlotPaperUnits, PlotRotation};
use uncad_model::{Affine2, CadDatabase};

/// Why [`crate::layout_to_svg`] or [`crate::layout_to_png`] has no sheet to
/// draw.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LayoutError {
    /// The model holds no layout by that name (a key of
    /// `tables.layouts`).
    NotFound,
    /// The layout is the model tab, which is not a sheet: render
    /// [`Space::Model`] instead.
    ModelLayout,
    /// The layout's block -- its paper space -- is not in the model: the
    /// reference did not resolve, or it names a block `tables.block_records`
    /// does not hold.
    BlockNotHeld,
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayoutError::NotFound => write!(f, "the drawing has no layout of that name"),
            LayoutError::ModelLayout => {
                write!(f, "the layout is the model tab, not a sheet")
            }
            LayoutError::BlockNotHeld => {
                write!(f, "the layout's paper space block is not in the model")
            }
        }
    }
}

impl std::error::Error for LayoutError {}

/// Where a layout's sheet -- the paper [`crate::layout_to_svg`] frames --
/// comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SheetSource {
    /// The layout's limits, which AutoCAD keeps equal to the paper's
    /// placement (margins and plot origin folded in).
    Limits,
    /// The paper its plot settings describe: the paper's size, turned a
    /// quarter for a quarter-turned plot, placed by the margins and plot
    /// origin, in the layout's paper units.
    PlotSettings,
}

/// One viewport of a layout's sheet, and what it shows of the model.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ViewportReport {
    /// The VIEWPORT's reference ID.
    pub id: EntityId,
    /// Its frame on the paper, in paper units, as the file states it.
    pub frame: Rect,
    /// Whether it is the layout's overall viewport: the sheet itself, as
    /// paper space shows it, not a window onto the model. A DXF numbers it
    /// 1; the binary format stores no number, and there it is the viewport
    /// whose view is its own frame (as tall as the frame, centred on it,
    /// untwisted).
    pub overall: bool,
    /// The map from the model's world to the paper the model is drawn
    /// through this viewport with -- `paper = C + s (R(twist) (p - T) -
    /// V)`, `C` the frame's centre, `T` the view's target, `V` its centre
    /// in display coordinates and `s` the frame's height over the view's.
    /// Its scale is the square root of its determinant's magnitude, its
    /// twist `atan2(b, a)`. `Some` exactly when the model is drawn through
    /// the viewport: it is on, is not the overall viewport and has a plan
    /// view this renderer draws (a viewport that is on and not overall but
    /// has no such view is in [`crate::ToSvgResult::undrawn_viewports`]).
    pub model_to_paper: Option<Affine2>,
    /// The part of the model the frame shows: the frame's lower-left,
    /// lower-right, upper-right and upper-left corners taken back through
    /// [`model_to_paper`](Self::model_to_paper) -- four corners, since a
    /// twisted view shows a turned rectangle of the model. `Some` exactly
    /// when that map is, and it can be inverted.
    pub model_window: Option<[Point2D; 4]>,
}

/// The block record a model-tab layout shows.
const MODEL_SPACE: &str = "*MODEL_SPACE";

/// Renders the layout `name` as its sheet, leaving the stroke width
/// unresolved like [`super::render`]. See [`crate::layout_to_svg`].
pub(crate) fn render_layout(
    db: &CadDatabase,
    name: &str,
    options: ToSvgOptions,
) -> Result<Scene, LayoutError> {
    let layout = db.tables.layouts.get(name).ok_or(LayoutError::NotFound)?;
    let (block_name, block) = layout
        .block_name
        .resolved()
        .and_then(|b| db.tables.block_records.get_key_value(b))
        .ok_or(LayoutError::BlockNotHeld)?;
    if block_name.to_uppercase() == MODEL_SPACE {
        return Err(LayoutError::ModelLayout);
    }

    // The sheet's own entities, written about their own middle.
    let paper = select_owned_by(db, |b| b == block_name);
    let paper_origin = choose_origin(&paper);
    let paper_frame = Frame {
        ox: paper_origin.x,
        oy: paper_origin.y,
    };
    let mut ctx = Ctx::configured(&db.tables, &options, paper_origin);
    let mut walked = walk(&paper, &mut ctx);
    let paper_parts = walked.len();

    // The model, written about its own middle, once per viewport that
    // shows it.
    let model = select_entities_for_space(db, Space::Model);
    let model_origin = choose_origin(&model);
    let model_frame = Frame {
        ox: model_origin.x,
        oy: model_origin.y,
    };
    let mut undrawn = BTreeSet::new();
    let mut viewports = Vec::new();
    // Every viewport of the block, its own layer hidden or not: a frame on
    // a layer that is off or not plotted -- the usual way to hide the
    // border -- still shows its view; only its border is left out.
    for vp in block.entities.iter().filter_map(|e| match e {
        Entity::Viewport(v) => Some(v),
        _ => None,
    }) {
        viewports.push(report(vp));
        let view = match shows(vp) {
            Shows::Nothing => continue,
            Shows::Undrawable => {
                undrawn.insert(vp.common.id);
                continue;
            }
            Shows::View(view) => view,
        };
        let to_paper = model_to_paper(vp, &view);
        let scale = vp.height / view.height;
        let matrix = svg_matrix(&to_paper, paper_frame, model_frame);

        ctx.frame = model_frame;
        ctx.transform = to_paper;
        ctx.svg_matrix = matrix;
        ctx.scale = scale;
        ctx.viewport_frozen = vp
            .frozen_layers
            .iter()
            .filter_map(|l| l.resolved().cloned())
            .collect();
        ctx.id_path.push(vp.common.id);
        let texts_before = ctx.texts.len();
        let parts = walk(&model, &mut ctx);
        ctx.id_path.pop();
        ctx.frame = paper_frame;
        ctx.transform = Affine2::IDENTITY;
        ctx.svg_matrix = infinite::IDENTITY;
        ctx.scale = 1.0;
        ctx.viewport_frozen.clear();

        // What the frame shows: the parts whose extent -- measured on the
        // paper, through the viewport -- meets the frame, and every
        // construction line, which reaches wherever the frame is.
        let frame_box = Box2D {
            min_x: vp.center.x - vp.width / 2.0,
            max_x: vp.center.x + vp.width / 2.0,
            min_y: vp.center.y - vp.height / 2.0,
            max_y: vp.center.y + vp.height / 2.0,
        };
        let (shown_ids, shown): (BTreeSet<EntityId>, Vec<String>) = parts
            .into_iter()
            .filter(|(part, svg)| {
                !svg.is_empty()
                    && (part.unbounded
                        || part
                            .extent
                            .is_some_and(|b| intersects(&Box2D::from(b), &frame_box)))
            })
            .map(|(part, svg)| (part.id, svg))
            .unzip();
        // The texts of the entities this frame does not show are not on
        // the sheet: their paths are the viewport's, then the entity's.
        let walked_texts = ctx.texts.split_off(texts_before);
        ctx.texts.extend(
            walked_texts
                .into_iter()
                .filter(|t| t.path.get(1).is_some_and(|id| shown_ids.contains(id))),
        );
        if shown.is_empty() {
            continue;
        }
        let clip = ctx.next_def_id("vp");
        ctx.defs.push(format!(
            "<clipPath id=\"{clip}\"><rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"/></clipPath>",
            paper_frame.x(frame_box.min_x),
            paper_frame.y(frame_box.max_y),
            clean(vp.width),
            clean(vp.height)
        ));
        let [a, b, c, d, e, f] = matrix;
        let mut group = String::new();
        let _ = write!(
            group,
            "<g clip-path=\"url(#{clip})\"><g transform=\"matrix({a} {b} {c} {d} {e} {f})\" stroke-width=\"{}\">\n  {}\n</g></g>",
            stroke_width_placeholder(scale),
            shown.join("\n  ")
        );
        walked.push((
            Part {
                id: vp.common.id,
                type_name: "VIEWPORT".to_string(),
                extent: Some(Rect::from(frame_box)),
                drawn: true,
                unbounded: false,
                hidden: None,
                left_out: None,
                through_viewport: true,
            },
            group,
        ));
    }

    // The sheet frames itself; only a layout that states none is framed by
    // the crop, over the sheet's own entities.
    let stated = sheet(layout);
    let (framed, padding) = match stated {
        Some((paper, _)) => (
            Framed {
                content: Some(paper),
                stated_taken: false,
            },
            0.0,
        ),
        None => (
            crop_parts(&mut walked[..paper_parts], options.crop),
            options.padding,
        ),
    };
    let view_box = view_box_of(&framed.content_or_origin(), padding, paper_origin);
    let crop = crop_report(
        &mut walked,
        framed,
        &scene::world_rect(view_box.rect, paper_origin),
    );
    let mut scene = ctx.finish(walked, view_box, paper_origin, crop);
    scene.undrawn_viewports = undrawn.into_iter().collect();
    scene.viewports = viewports;
    scene.sheet = stated.map(|(_, source)| source);
    Ok(scene)
}

/// What [`ViewportReport`] says of `vp`.
fn report(vp: &ViewportEntity) -> ViewportReport {
    let (cx, cy, hw, hh) = (vp.center.x, vp.center.y, vp.width / 2.0, vp.height / 2.0);
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
    let model_to_paper = match shows(vp) {
        Shows::View(view) => Some(model_to_paper(vp, &view)),
        Shows::Nothing | Shows::Undrawable => None,
    };
    let model_window = model_to_paper.and_then(|m| {
        let [a, b, c, d] = corners.map(|p| invert_point(&m, p));
        Some([a?, b?, c?, d?])
    });
    ViewportReport {
        id: vp.common.id,
        frame: Rect::new(cx - hw, cy - hh, cx + hw, cy + hh),
        overall: is_overall(vp),
        model_to_paper,
        model_window,
    }
}

/// What a viewport shows of the model.
enum Shows {
    /// Nothing, by the drawing's own say: the viewport is off, or it is
    /// the layout's overall viewport -- the sheet itself, not a window
    /// onto the model.
    Nothing,
    /// A view this renderer cannot draw: see
    /// [`crate::ToSvgResult::undrawn_viewports`].
    Undrawable,
    /// This view, in plan.
    View(ViewportView),
}

fn shows(vp: &ViewportEntity) -> Shows {
    if vp.on == Some(false) || is_overall(vp) {
        return Shows::Nothing;
    }
    let Some(view) = vp.view else {
        return Shows::Undrawable;
    };
    let d = view.direction;
    let plan = d.z > 0.0 && d.x.abs().max(d.y.abs()) <= 1e-9 * d.z;
    let sized = [vp.width, vp.height, view.height]
        .iter()
        .all(|v| v.is_finite() && *v > 0.0);
    let real = [
        vp.center.x,
        vp.center.y,
        view.center.x,
        view.center.y,
        view.target.x,
        view.target.y,
        view.twist,
    ]
    .iter()
    .all(|v| v.is_finite());
    if plan && sized && real {
        Shows::View(view)
    } else {
        Shows::Undrawable
    }
}

/// Whether `vp` is its layout's overall viewport: the one that is the sheet
/// as paper space shows it, not a window onto the model. The DXF numbers it
/// 1; the binary format stores no number, and a DXF from R2000 on writes 0
/// for every viewport of a layout that is not the current one -- no number
/// either. Without one it is the viewport whose view is itself -- as tall as
/// its frame, centred on the frame's centre, untwisted.
fn is_overall(vp: &ViewportEntity) -> bool {
    match vp.viewport_id {
        Some(id) if id > 0 => id == 1,
        _ => vp.view.is_some_and(|view| {
            let tol = 1e-6 * vp.height.abs().max(1.0);
            (view.height - vp.height).abs() < tol
                && (view.center.x - vp.center.x).abs() < tol
                && (view.center.y - vp.center.y).abs() < tol
                && view.twist.abs() < 1e-9
        }),
    }
}

/// The map from the world of the model to the paper, through `vp`'s view:
/// a model point `p` is at `C + s (R(twist) (p - T) - V)` on the sheet,
/// where `C` is the frame's centre, `T` the view's target, `V` its centre
/// in display coordinates and `s` the frame's height over the view's.
/// A positive twist turns the picture counter-clockwise -- the convention
/// ezdxf follows; unverified against a sheet AutoCAD plotted.
fn model_to_paper(vp: &ViewportEntity, view: &ViewportView) -> Affine2 {
    let s = vp.height / view.height;
    let (sin, cos) = view.twist.sin_cos();
    let (t, v) = (view.target, view.center);
    Affine2 {
        a: s * cos,
        b: s * sin,
        c: -s * sin,
        d: s * cos,
        e: vp.center.x - s * (cos * t.x - sin * t.y) - s * v.x,
        f: vp.center.y - s * (sin * t.x + cos * t.y) - s * v.y,
    }
}

/// The sheet of paper the layout is set up on, in its own paper units: the
/// layout's limits when they span a rectangle -- AutoCAD keeps them equal
/// to the paper's placement -- and otherwise the paper its plot settings
/// describe: the paper's size, turned a quarter for a quarter-turned plot,
/// with the printable area's lower-left corner moved by the plot origin at
/// the layout's origin (the rule ezdxf's `reset_paper_limits` applies). The
/// settings are in millimetres; a layout drawn in inches has them divided
/// by 25.4. `None` when the layout states neither -- or states one no
/// drawing can mean, a number past [`MAX_WORLD_COORDINATE`] (a corrupt
/// header), which would otherwise size the picture the way the extent of
/// the model space is kept from doing.
fn sheet(layout: &LayoutRecord) -> Option<(Box2D, SheetSource)> {
    let sane = |v: f64| v.is_finite() && v.abs() < MAX_WORLD_COORDINATE;
    let (lo, hi) = (layout.limits_min, layout.limits_max);
    if [lo.x, lo.y, hi.x, hi.y].into_iter().all(sane) && hi.x > lo.x && hi.y > lo.y {
        let limits = Box2D {
            min_x: lo.x,
            max_x: hi.x,
            min_y: lo.y,
            max_y: hi.y,
        };
        return Some((limits, SheetSource::Limits));
    }
    let p = &layout.plot_settings;
    let sized = |v: f64| sane(v) && v > 0.0;
    if !(sized(p.paper_width) && sized(p.paper_height)) {
        return None;
    }
    let (width, height) = match p.rotation {
        Some(PlotRotation::Counterclockwise90 | PlotRotation::Clockwise90) => {
            (p.paper_height, p.paper_width)
        }
        _ => (p.paper_width, p.paper_height),
    };
    let per_mm = if p.paper_units == Some(PlotPaperUnits::Inches) {
        1.0 / 25.4
    } else {
        1.0
    };
    let finite = |v: f64| if sane(v) { v } else { 0.0 };
    let shift = Point2D {
        x: finite(p.margin_left) + finite(p.plot_origin.x),
        y: finite(p.margin_bottom) + finite(p.plot_origin.y),
    };
    let paper = Box2D {
        min_x: -shift.x * per_mm,
        max_x: (width - shift.x) * per_mm,
        min_y: -shift.y * per_mm,
        max_y: (height - shift.y) * per_mm,
    };
    Some((paper, SheetSource::PlotSettings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uncad_model::model::{Confidence, EntityCommon, EntityId, Origin, Point3D, Ref};
    use uncad_model::tables::PlotSettings;

    fn layout(limits: (Point2D, Point2D), plot: PlotSettings) -> LayoutRecord {
        LayoutRecord {
            name: "Layout1".to_string(),
            tab_order: 1,
            block_name: Ref::Resolved("*Paper_Space".to_string()),
            limits_min: limits.0,
            limits_max: limits.1,
            plot_settings: plot,
            paper_space_linetype_scaling: true,
            limits_check: false,
            extents_min: None,
            extents_max: None,
            active_viewport: Ref::Absent,
        }
    }

    fn plot(size: (f64, f64), margins: (f64, f64), origin: (f64, f64)) -> PlotSettings {
        PlotSettings {
            paper_name: String::new(),
            paper_width: size.0,
            paper_height: size.1,
            margin_left: margins.0,
            margin_bottom: margins.1,
            margin_right: margins.0,
            margin_top: margins.1,
            plot_origin: Point2D {
                x: origin.0,
                y: origin.1,
            },
            paper_units: Some(PlotPaperUnits::Millimeters),
            rotation: Some(PlotRotation::Unrotated),
            scale_numerator: 1.0,
            scale_denominator: 1.0,
        }
    }

    fn p(x: f64, y: f64) -> Point2D {
        Point2D { x, y }
    }

    fn corners(b: Box2D) -> [f64; 4] {
        [b.min_x, b.min_y, b.max_x, b.max_y]
    }

    #[test]
    fn a_sheet_size_no_drawing_can_mean_is_not_a_sheet() {
        // Limits past the coordinate bound fall back to the paper ...
        let l = layout(
            (p(0.0, 0.0), p(1e200, 1e200)),
            plot((420.0, 297.0), (0.0, 0.0), (0.0, 0.0)),
        );
        assert_eq!(sheet(&l).unwrap().1, SheetSource::PlotSettings);
        // ... and a paper past it is no sheet at all.
        let l = layout(
            (p(0.0, 0.0), p(0.0, 0.0)),
            plot((1e200, 297.0), (0.0, 0.0), (0.0, 0.0)),
        );
        assert!(sheet(&l).is_none());
    }

    #[test]
    fn the_sheet_is_the_layouts_limits_when_they_span_a_rectangle() {
        let l = layout(
            (p(-5.0, -5.0), p(415.0, 292.0)),
            plot((0.0, 0.0), (0.0, 0.0), (0.0, 0.0)),
        );
        assert_eq!(corners(sheet(&l).unwrap().0), [-5.0, -5.0, 415.0, 292.0]);
        assert_eq!(sheet(&l).unwrap().1, SheetSource::Limits);
    }

    #[test]
    fn without_limits_the_sheet_is_the_paper_moved_by_margin_and_plot_origin() {
        let none = (p(0.0, 0.0), p(0.0, 0.0));
        // A4 landscape stated as such, margins 5/10, plot origin 0: the
        // printable corner is at the origin, so the paper starts 5 left and
        // 10 below it.
        let l = layout(none, plot((297.0, 210.0), (5.0, 10.0), (0.0, 0.0)));
        assert_eq!(corners(sheet(&l).unwrap().0), [-5.0, -10.0, 292.0, 200.0]);
        // The usual page setup, origin = minus the margins: the paper's own
        // corner is at the origin.
        let l = layout(none, plot((297.0, 210.0), (5.0, 10.0), (-5.0, -10.0)));
        assert_eq!(corners(sheet(&l).unwrap().0), [0.0, 0.0, 297.0, 210.0]);
        assert_eq!(sheet(&l).unwrap().1, SheetSource::PlotSettings);
        // Stated portrait and plotted a quarter turned: landscape.
        let mut turned = plot((210.0, 297.0), (0.0, 0.0), (0.0, 0.0));
        turned.rotation = Some(PlotRotation::Clockwise90);
        assert_eq!(
            corners(sheet(&layout(none, turned)).unwrap().0),
            [0.0, 0.0, 297.0, 210.0]
        );
        // In inches, the millimetres are divided by 25.4.
        let mut inches = plot((254.0, 127.0), (0.0, 0.0), (0.0, 0.0));
        inches.paper_units = Some(PlotPaperUnits::Inches);
        assert_eq!(
            corners(sheet(&layout(none, inches)).unwrap().0),
            [0.0, 0.0, 10.0, 5.0]
        );
        // No paper either: no sheet.
        assert!(sheet(&layout(none, plot((0.0, 0.0), (0.0, 0.0), (0.0, 0.0)))).is_none());
    }

    fn viewport(id: Option<i32>, view: Option<ViewportView>) -> ViewportEntity {
        ViewportEntity {
            common: EntityCommon {
                id: EntityId::new(1),
                origin: Origin::Vector,
                confidence: Confidence::High,
                source_handle: Ref::Absent,
                layer: Ref::Absent,
                color_index: 7,
                true_color: None,
                invisible: false,
                linetype: uncad_model::model::EntityLinetype::ByLayer,
                linetype_scale: 1.0,
                lineweight: Some(-1),
                transparency: Some(0),
            },
            center: Point3D {
                x: 150.0,
                y: 100.0,
                z: 0.0,
            },
            width: 200.0,
            height: 120.0,
            view,
            on: Some(true),
            viewport_id: id,
            frozen_layers: Vec::new(),
        }
    }

    fn view(center: Point2D, height: f64, twist: f64) -> ViewportView {
        ViewportView {
            center,
            height,
            target: Point3D {
                x: 10.0,
                y: 5.0,
                z: 0.0,
            },
            direction: Point3D {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            },
            twist,
            lens_length: 50.0,
        }
    }

    #[test]
    fn the_overall_viewport_is_numbered_1_or_shows_itself() {
        let own = view(p(150.0, 100.0), 120.0, 0.0);
        assert!(is_overall(&viewport(Some(1), Some(own))));
        assert!(!is_overall(&viewport(Some(2), Some(own))));
        // Numbered 0 (every viewport of a DXF layout that is not current):
        // no number, so again the viewport whose view is its own frame.
        assert!(is_overall(&viewport(Some(0), Some(own))));
        assert!(!is_overall(&viewport(
            Some(0),
            Some(view(p(150.0, 100.0), 60.0, 0.0))
        )));
        // No number (a DWG): the viewport whose view is its own frame.
        assert!(is_overall(&viewport(None, Some(own))));
        assert!(!is_overall(&viewport(
            None,
            Some(view(p(150.0, 100.0), 60.0, 0.0))
        )));
        assert!(!is_overall(&viewport(None, None)));
    }

    #[test]
    fn a_view_that_is_off_missing_or_not_in_plan_is_not_shown() {
        let shown = view(p(0.0, 0.0), 60.0, 0.0);
        assert!(matches!(
            shows(&viewport(Some(2), Some(shown))),
            Shows::View(_)
        ));
        let mut off = viewport(Some(2), Some(shown));
        off.on = Some(false);
        assert!(matches!(shows(&off), Shows::Nothing));
        assert!(matches!(shows(&viewport(Some(2), None)), Shows::Undrawable));
        let mut tilted = shown;
        tilted.direction = Point3D {
            x: 1.0,
            y: 1.0,
            z: 1.0,
        };
        assert!(matches!(
            shows(&viewport(Some(2), Some(tilted))),
            Shows::Undrawable
        ));
        let mut flat = shown;
        flat.height = 0.0;
        assert!(matches!(
            shows(&viewport(Some(2), Some(flat))),
            Shows::Undrawable
        ));
    }

    #[test]
    fn the_model_lands_on_the_paper_by_centre_target_scale_and_twist() {
        // Scale 120 / 60 = 2, a quarter-turn twist: the target (10, 5) plus
        // the view's centre (in display coordinates, turned) is the frame's
        // centre.
        let v = view(p(20.0, 30.0), 60.0, std::f64::consts::FRAC_PI_2);
        let m = model_to_paper(&viewport(Some(2), Some(v)), &v);
        // Display point (20, 30) is the world point T + R(-90)(20, 30) =
        // (10 + 30, 5 - 20).
        let c = m.apply(p(40.0, -15.0));
        assert!(
            (c.x - 150.0).abs() < 1e-9 && (c.y - 100.0).abs() < 1e-9,
            "{c:?}"
        );
        // One model unit along x is two paper units, turned a quarter.
        let x = m.apply(p(41.0, -15.0));
        assert!(
            (x.x - 150.0).abs() < 1e-9 && (x.y - 102.0).abs() < 1e-9,
            "{x:?}"
        );
    }
}
