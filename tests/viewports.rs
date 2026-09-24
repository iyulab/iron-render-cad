//! What each viewport of a sheet shows of the model, and where the sheet
//! comes from. A caller that places something on a sheet -- a note beside
//! a detail, a marker on a model point seen through a viewport -- needs the
//! map the renderer drew the model with, and the part of the model each
//! frame shows, not a restatement of the rule that chose them.
//!
//! Golden G14's Layout1: the overall viewport (271, the sheet itself), a
//! detail viewport (272) at half scale turned 30 degrees, and one that is
//! off (273). Every expected number is worked out from the viewport's view
//! (paper = C + s (R(twist) (model - T) - V)), not read off this crate's
//! output.

use iron_render_cad::{
    layout_to_png, layout_to_svg, to_svg, Rect, Scene, SheetSource, ToPngOptions, ToSvgOptions,
    ViewportReport,
};
use uncad_model::model::{Entity, EntityId, Point2D};
use uncad_model::CadDatabase;

fn g14() -> CadDatabase {
    serde_json::from_str(include_str!("golden/g14.expected.json"))
        .expect("the golden model deserializes")
}

/// The paper point the detail viewport shows model point `(x, y)` at:
/// scale 150 / 300, twist 30 degrees, target (10, 5), view centre
/// (100, 50), frame centre (150, 150).
fn detail(x: f64, y: f64) -> (f64, f64) {
    let (s, t) = (0.5, 30f64.to_radians());
    let (dx, dy) = (x - 10.0, y - 5.0);
    let (rx, ry) = (t.cos() * dx - t.sin() * dy, t.sin() * dx + t.cos() * dy);
    (150.0 + s * (rx - 100.0), 150.0 + s * (ry - 50.0))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn by_id(reports: &[ViewportReport], id: u64) -> &ViewportReport {
    reports
        .iter()
        .find(|r| r.id == EntityId::new(id))
        .unwrap_or_else(|| panic!("viewport {id} in {reports:?}"))
}

#[test]
fn every_viewport_of_a_sheet_says_what_it_shows() {
    let result = layout_to_svg(&g14(), "Layout1", ToSvgOptions::default()).expect("a sheet");
    let ids: Vec<u64> = result.viewports.iter().map(|r| r.id.value()).collect();
    assert_eq!(ids, [271, 272, 273], "in the order paper space lists them");
    assert_eq!(result.sheet, Some(SheetSource::Limits));

    // The overall viewport is the sheet: no window onto the model.
    let overall = by_id(&result.viewports, 271);
    assert!(overall.overall);
    assert_eq!(overall.frame, Rect::new(0.0, 0.0, 420.0, 297.0));
    assert!(overall.model_to_paper.is_none() && overall.model_window.is_none());

    // The one that is off shows nothing either.
    let off = by_id(&result.viewports, 273);
    assert!(!off.overall);
    assert!(off.model_to_paper.is_none() && off.model_window.is_none());
    assert_eq!(off.frame, Rect::new(300.0, 20.0, 400.0, 100.0));

    // The detail viewport: its map is the view's, half scale, 30 degrees.
    let vp = by_id(&result.viewports, 272);
    assert!(!vp.overall);
    assert_eq!(vp.frame, Rect::new(50.0, 75.0, 250.0, 225.0));
    let m = vp.model_to_paper.expect("the model is drawn through it");
    assert!(close(m.determinant().abs().sqrt(), 0.5));
    assert!(close(m.b.atan2(m.a), 30f64.to_radians()));
    for (x, y) in [(0.0, 30.0), (200.0, 60.0), (-40.0, 7.5)] {
        let p = m.apply(Point2D { x, y });
        let (px, py) = detail(x, y);
        assert!(close(p.x, px) && close(p.y, py), "({x}, {y}): {p:?}");
    }
    // Its model window is the frame's corners taken back into the model,
    // in the frame's order: lower-left, lower-right, upper-right,
    // upper-left.
    let window = vp.model_window.expect("a model window");
    let frame = [(50.0, 75.0), (250.0, 75.0), (250.0, 225.0), (50.0, 225.0)];
    for (corner, (fx, fy)) in window.iter().zip(frame) {
        let (px, py) = detail(corner.x, corner.y);
        assert!(close(px, fx) && close(py, fy), "{corner:?} -> ({px}, {py})");
    }
    // The frame's centre shows the view's centre: T + R(-twist) V.
    let (sin, cos) = 30f64.to_radians().sin_cos();
    let centre = (
        (window[0].x + window[2].x) / 2.0,
        (window[0].y + window[2].y) / 2.0,
    );
    assert!(close(centre.0, 10.0 + cos * 100.0 + sin * 50.0));
    assert!(close(centre.1, 5.0 - sin * 100.0 + cos * 50.0));
}

#[test]
fn a_viewport_whose_view_is_not_drawn_says_so_twice() {
    let mut db = g14();
    let paper = db
        .tables
        .block_records
        .get_mut("*Paper_Space")
        .expect("paper space");
    for e in &mut paper.entities {
        if let Entity::Viewport(v) = e {
            if v.common.id == EntityId::new(272) {
                v.view = None;
            }
        }
    }
    let result = layout_to_svg(&db, "Layout1", ToSvgOptions::default()).expect("a sheet");
    assert_eq!(result.undrawn_viewports, [EntityId::new(272)]);
    let vp = by_id(&result.viewports, 272);
    assert!(vp.model_to_paper.is_none() && vp.model_window.is_none());
}

#[test]
fn a_binary_files_overall_viewport_is_found_by_its_view() {
    // The binary format stores no viewport number: the overall viewport is
    // the one whose view is its own frame.
    let mut db = g14();
    for e in &mut db
        .tables
        .block_records
        .get_mut("*Paper_Space")
        .expect("paper space")
        .entities
    {
        if let Entity::Viewport(v) = e {
            v.viewport_id = None;
        }
    }
    let result = layout_to_svg(&db, "Layout1", ToSvgOptions::default()).expect("a sheet");
    let overall: Vec<u64> = result
        .viewports
        .iter()
        .filter(|r| r.overall)
        .map(|r| r.id.value())
        .collect();
    assert_eq!(overall, [271]);
}

#[test]
fn an_inactive_layouts_overall_viewport_is_found_by_its_view() {
    // A DXF from R2000 on writes group 69 as 0 for every viewport of a
    // layout that is not the current one: 0 is no number, so the overall
    // viewport is again the one whose view is its own frame -- not a window
    // onto the model, which would draw the model over the whole sheet.
    let mut db = g14();
    for e in &mut db
        .tables
        .block_records
        .get_mut("*Paper_Space")
        .expect("paper space")
        .entities
    {
        if let Entity::Viewport(v) = e {
            v.viewport_id = Some(0);
        }
    }
    let result = layout_to_svg(&db, "Layout1", ToSvgOptions::default()).expect("a sheet");
    let overall: Vec<u64> = result
        .viewports
        .iter()
        .filter(|r| r.overall)
        .map(|r| r.id.value())
        .collect();
    assert_eq!(overall, [271]);
    let vp = by_id(&result.viewports, 271);
    assert!(vp.model_to_paper.is_none() && vp.model_window.is_none());
}

#[test]
fn a_sheet_read_from_a_layout_that_is_not_current_keeps_its_overall_viewport() {
    // G19: the same sheet as written for a layout that is not the current
    // one -- every viewport numbered 0. The overall viewport is found by its
    // view, and the model is drawn only through the two detail windows.
    let db: CadDatabase = serde_json::from_str(include_str!("golden/g19.expected.json"))
        .expect("the golden model deserializes");
    let result = layout_to_svg(&db, "Layout1", ToSvgOptions::default()).expect("a sheet");
    let overall: Vec<u64> = result
        .viewports
        .iter()
        .filter(|r| r.overall)
        .map(|r| r.id.value())
        .collect();
    assert_eq!(overall, [271]);
    let vp = by_id(&result.viewports, 271);
    assert!(vp.model_to_paper.is_none() && vp.model_window.is_none());
    // The same sheet as G14 draws it, where the viewports carry numbers.
    let numbered = layout_to_svg(&g14(), "Layout1", ToSvgOptions::default()).expect("a sheet");
    assert_eq!(result.svg, numbered.svg);
}

#[test]
fn the_sheet_says_where_its_paper_comes_from() {
    let mut db = g14();
    // No limits: the paper from the plot settings -- A3 stated portrait,
    // plotted a quarter turned, margins and plot origin cancelling.
    let layout = db.tables.layouts.get_mut("Layout1").expect("a layout");
    layout.limits_min = Point2D { x: 0.0, y: 0.0 };
    layout.limits_max = Point2D { x: 0.0, y: 0.0 };
    let from_plot = layout_to_svg(&db, "Layout1", ToSvgOptions::default()).expect("a sheet");
    assert_eq!(from_plot.sheet, Some(SheetSource::PlotSettings));
    assert_eq!(from_plot.view_box, Rect::new(0.0, 0.0, 420.0, 297.0));
    // Neither: framed like a render of its paper space.
    let layout = db.tables.layouts.get_mut("Layout1").expect("a layout");
    layout.plot_settings.paper_width = 0.0;
    let neither = layout_to_svg(&db, "Layout1", ToSvgOptions::default()).expect("a sheet");
    assert_eq!(neither.sheet, None);
    // A model render has no sheet and no viewports.
    let model = to_svg(&g14(), ToSvgOptions::default());
    assert_eq!(model.sheet, None);
    assert!(model.viewports.is_empty());
}

#[test]
fn a_sheets_png_and_scene_report_the_same_viewports() {
    let db = g14();
    let svg = layout_to_svg(&db, "Layout1", ToSvgOptions::default()).expect("a sheet");
    let png = layout_to_png(&db, "Layout1", ToPngOptions::default()).expect("a sheet");
    let scene = Scene::layout(&db, "Layout1", ToSvgOptions::default()).expect("a sheet");
    assert_eq!(png.viewports, svg.viewports);
    assert_eq!(scene.viewports, svg.viewports);
    assert_eq!((png.sheet, scene.sheet), (svg.sheet, svg.sheet));
}
