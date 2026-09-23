//! A paper layout is drawn as its sheet: its own entities on the paper its
//! limits (or plot settings) describe, and the model shown through each
//! viewport -- at the viewport's scale, turned by its twist, clipped to its
//! frame, without the layers frozen in it, its strokes and pattern lines
//! the sheet's width -- except the overall viewport and a viewport that is
//! off, which show nothing, and one whose view cannot be drawn, which is
//! reported.
//!
//! Golden G14 is the sheet: an A3 border and title, the overall viewport,
//! a detail viewport at half scale turned 30 degrees with WALLS frozen in
//! it, and a viewport that is off, over a model of one line per layer
//! state. Every expected number is worked out from the viewport's view
//! (paper = C + s (R(twist) (model - T) - V)), not read off this crate's
//! output.

use iron_render_cad::{
    layout_to_png, layout_to_svg, LayoutError, PngError, PngSize, ToPngOptions, ToSvgOptions,
    ToSvgResult,
};
use uncad_model::model::{
    Confidence, Entity, EntityCommon, EntityId, HatchBoundaryPath, HatchEntity, HatchPatternLine,
    Origin, Point2D, Point3D, PolylineVertex, RayEntity, Ref,
};
use uncad_model::CadDatabase;

fn g14() -> CadDatabase {
    serde_json::from_str(include_str!("golden/g14.expected.json"))
        .expect("the golden model deserializes")
}

fn sheet(db: &CadDatabase, layout: &str) -> ToSvgResult {
    layout_to_svg(db, layout, ToSvgOptions::default()).expect("a sheet")
}

/// The numbers of the first `attr="..."` after `from` in `svg`.
fn numbers(svg: &str, from: &str, attr: &str) -> Vec<f64> {
    let rest = &svg[svg.find(from).unwrap_or_else(|| panic!("{from} in {svg}"))..];
    let at = rest.find(&format!("{attr}=\"")).unwrap() + attr.len() + 2;
    rest[at..at + rest[at..].find('"').unwrap()]
        .trim_start_matches("matrix(")
        .trim_end_matches(')')
        .split([' ', ','])
        .map(|v| v.parse().unwrap())
        .collect()
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

/// Where `matrix` (SVG order) sends the SVG point `(u, v)`.
fn apply(m: &[f64], (u, v): (f64, f64)) -> (f64, f64) {
    (m[0] * u + m[2] * v + m[4], m[1] * u + m[3] * v + m[5])
}

#[test]
fn g14s_sheet_is_the_paper_with_the_model_through_its_detail_viewport() {
    let result = sheet(&g14(), "Layout1");
    let svg = &result.svg;
    // The sheet is the layout's limits, 420 x 297, with no padding.
    assert_eq!(numbers(svg, "<svg", "viewBox"), [0.0, -297.0, 420.0, 297.0]);
    // One window onto the model: the detail viewport's frame, 200 x 150
    // about (150, 150). The overall viewport is the sheet itself and the
    // third is off.
    assert_eq!(svg.matches("<clipPath").count(), 1, "{svg}");
    assert_eq!(numbers(svg, "<clipPath", "x"), [50.0]);
    assert_eq!(numbers(svg, "<clipPath", "y"), [-225.0]);
    assert_eq!(numbers(svg, "<clipPath", "width"), [200.0]);
    assert_eq!(numbers(svg, "<clipPath", "height"), [150.0]);
    assert!(result.undrawn_viewports.is_empty());
    // Through it, the model at half scale and turned 30 degrees: the
    // group's matrix takes a model point, written y-down, to where the view
    // puts it on the paper, written y-down.
    let m = numbers(svg, "clip-path=", "transform");
    for (x, y) in [(0.0, 30.0), (200.0, 60.0)] {
        let (px, py) = detail(x, y);
        let (sx, sy) = apply(&m, (x, -y));
        assert!(
            (sx - px).abs() < 1e-9 && (sy + py).abs() < 1e-9,
            "({x}, {y}): ({sx}, {sy}) vs ({px}, {py})"
        );
    }
    // Strokes in the group are the sheet's: the automatic width divided by
    // the viewport's scale.
    let sheet_stroke = numbers(svg, "<svg", "stroke-width")[0];
    let group_stroke = numbers(svg, "clip-path=", "stroke-width")[0];
    assert!((group_stroke - sheet_stroke / 0.5).abs() < 1e-12);
    // The lines the viewport shows: not WALLS (frozen in it), nor the
    // layers that are off, frozen or not plotted -- LOCKED, PLOT and
    // DEFAULTWT, at y = 30, 50 and 60.
    let group = &svg[svg.find("clip-path=").unwrap()..];
    let group = &group[..group.find("</g></g>").unwrap()];
    let ys: Vec<f64> = group
        .split("<line x1=\"0\" y1=\"")
        .skip(1)
        .map(|l| l[..l.find('"').unwrap()].parse().unwrap())
        .collect();
    assert_eq!(ys, [-30.0, -50.0, -60.0], "{group}");
    assert_eq!(result.hidden, 4);
    // And the paper's own: the border, the title and three frames.
    assert!(svg.contains("<polygon points=\"0,0 420,0 420,-297 0,-297\""));
    assert!(svg.contains(">SHEET 1</text>"));
    assert_eq!(svg.matches("stroke-dasharray=\"2,2\"").count(), 3, "{svg}");
}

#[test]
fn an_empty_layout_is_its_bare_sheet() {
    let result = sheet(&g14(), "Layout2");
    assert_eq!(
        numbers(&result.svg, "<svg", "viewBox"),
        [0.0, -8.5, 11.0, 8.5]
    );
    assert!(!result.svg.contains("<clipPath"), "{}", result.svg);
}

#[test]
fn a_layout_that_is_not_a_sheet_says_why() {
    let db = g14();
    let error = |name: &str| layout_to_svg(&db, name, ToSvgOptions::default()).err();
    assert_eq!(error("Model"), Some(LayoutError::ModelLayout));
    assert_eq!(error("Layout9"), Some(LayoutError::NotFound));
    let mut unheld = g14();
    unheld.tables.layouts.get_mut("Layout2").unwrap().block_name =
        Ref::Unresolved("7F".to_string());
    assert_eq!(
        layout_to_svg(&unheld, "Layout2", ToSvgOptions::default()).err(),
        Some(LayoutError::BlockNotHeld)
    );
    assert!(matches!(
        layout_to_png(&db, "Model", ToPngOptions::default()).err(),
        Some(PngError::Layout(LayoutError::ModelLayout))
    ));
}

/// G14 with the detail viewport (reference ID 272) changed by `change`.
fn with_detail(change: impl Fn(&mut uncad_model::model::ViewportEntity)) -> CadDatabase {
    let mut db = g14();
    for e in db.all_entities_mut() {
        if let Entity::Viewport(v) = e {
            if v.common.id == EntityId::new(272) {
                change(v);
            }
        }
    }
    db
}

#[test]
fn a_view_that_cannot_be_drawn_is_reported_and_its_frame_stays() {
    for db in [
        with_detail(|v| {
            v.view.as_mut().unwrap().direction = Point3D {
                x: 1.0,
                y: 1.0,
                z: 1.0,
            }
        }),
        with_detail(|v| v.view = None),
    ] {
        let result = sheet(&db, "Layout1");
        assert_eq!(result.undrawn_viewports, [EntityId::new(272)]);
        assert!(!result.svg.contains("<clipPath"), "{}", result.svg);
        assert_eq!(result.svg.matches("stroke-dasharray=\"2,2\"").count(), 3);
    }
}

fn common(id: u64, layer: &str) -> EntityCommon {
    EntityCommon {
        id: EntityId::new(id),
        origin: Origin::Vector,
        confidence: Confidence::High,
        source_handle: Ref::Resolved(format!("{id:X}")),
        layer: Ref::Resolved(layer.to_string()),
        color_index: 7,
        true_color: None,
        invisible: false,
    }
}

/// Adds `e` to G14's model space.
fn in_model(db: &mut CadDatabase, e: Entity) {
    db.entities.push(e.clone());
    db.tables
        .block_records
        .get_mut("*Model_Space")
        .unwrap()
        .entities
        .push(e);
}

#[test]
fn each_viewport_fills_a_hatch_with_its_own_pattern_at_the_sheets_stroke() {
    // A pattern hatch in the model, and a second window onto it at scale 1
    // (the viewport that was off, switched on): the pattern is drawn inside
    // each viewport's matrix, so each needs its own definition, its line
    // the sheet's stroke over that viewport's scale.
    let mut db = with_detail(|_| {});
    for e in db.all_entities_mut() {
        if let Entity::Viewport(v) = e {
            if v.common.id == EntityId::new(273) {
                v.on = Some(true);
                v.frozen_layers.clear();
            }
        }
    }
    let square = |x: f64, y: f64| Point2D { x, y };
    in_model(
        &mut db,
        Entity::Hatch(HatchEntity {
            common: common(0x500, "LOCKED"),
            boundary_paths: vec![HatchBoundaryPath::Polyline(
                [
                    square(-10.0, -10.0),
                    square(10.0, -10.0),
                    square(10.0, 10.0),
                    square(-10.0, 10.0),
                ]
                .map(PolylineVertex::straight)
                .to_vec(),
            )],
            solid_fill: false,
            gradient: None,
            pattern_lines: vec![HatchPatternLine {
                angle: 0.0,
                base_point: square(0.0, 0.0),
                offset: square(0.0, 2.0),
                dash_pattern: Vec::new(),
            }],
            elevation: 0.0,
            extrusion: uncad_model::model::Point3D {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            },
            style: None,
        }),
    );
    let result = sheet(&db, "Layout1");
    let svg = &result.svg;
    assert_eq!(svg.matches("<clipPath").count(), 2, "{svg}");
    let patterns: Vec<&str> = svg.split("<pattern id=\"").skip(1).collect();
    assert_eq!(patterns.len(), 2, "{svg}");
    let sheet_stroke = numbers(svg, "<svg", "stroke-width")[0];
    let mut widths: Vec<f64> = patterns
        .iter()
        .map(|p| numbers(p, "<line", "stroke-width")[0] / sheet_stroke)
        .collect();
    widths.sort_by(f64::total_cmp);
    // Scale 1/2 draws its pattern line two model units wide, scale 1 one.
    assert!((widths[0] - 1.0).abs() < 1e-12 && (widths[1] - 2.0).abs() < 1e-12);
    // Each viewport's hatch fills with a pattern of its own.
    let fills: Vec<&str> = svg
        .split("fill=\"url(#")
        .skip(1)
        .map(|f| &f[..f.find(')').unwrap()])
        .collect();
    assert_eq!(fills.len(), 2);
    assert_ne!(fills[0], fills[1]);
}

#[test]
fn a_far_away_model_is_shown_through_the_viewport_in_sheet_sized_numbers() {
    // A model at 1e7 is written about its own middle; the viewport's
    // matrix takes those small numbers to the sheet, so nothing in the
    // document is far from zero.
    let far = 1.0e7;
    let mut db = with_detail(|v| {
        let view = v.view.as_mut().unwrap();
        view.target = Point3D {
            x: far + 10.0,
            y: far + 5.0,
            z: 0.0,
        };
    });
    for e in db.all_entities_mut() {
        if let Entity::Line(l) = e {
            l.start_point.x += far;
            l.start_point.y += far;
            l.end_point.x += far;
            l.end_point.y += far;
        }
    }
    let result = sheet(&db, "Layout1");
    let m = numbers(&result.svg, "clip-path=", "transform");
    assert!(m.iter().all(|v| v.abs() < 1000.0), "{m:?}");
    // The model's first drawn line (LOCKED, (0, 30) moved by 1e7) is
    // written about the model's origin, and lands where the unmoved model
    // did.
    let group = &result.svg[result.svg.find("clip-path=").unwrap()..];
    let line = numbers(group, "<line", "x1")[0];
    let y1 = numbers(group, "<line", "y1")[0];
    let (sx, sy) = apply(&m, (line, y1));
    let (px, py) = detail(0.0, 30.0);
    assert!(
        (sx - px).abs() < 1e-6 && (sy + py).abs() < 1e-6,
        "({sx}, {sy})"
    );
}

#[test]
fn a_construction_line_in_the_model_is_cut_at_the_sheets_edge() {
    // A horizontal XLINE through the model's (0, 30): through the detail
    // viewport it runs across the whole sheet at 30 degrees, and is cut
    // where it leaves the sheet's window -- in the sheet's frame, not the
    // model's.
    let mut db = with_detail(|_| {});
    in_model(
        &mut db,
        Entity::XLine(RayEntity {
            common: common(0x501, "LOCKED"),
            point: Point3D {
                x: 0.0,
                y: 30.0,
                z: 0.0,
            },
            vector: Point3D {
                x: 1.0,
                y: 0.0,
                z: 0.0,
            },
        }),
    );
    let result = sheet(&db, "Layout1");
    let svg = &result.svg;
    let group = &svg[svg.find("clip-path=").unwrap()..];
    let xline = &group[group.find("stroke-dasharray=\"4,2\"").unwrap() - 120..];
    let xline = &xline[xline.find("<line").unwrap()..];
    let m = numbers(svg, "clip-path=", "transform");
    let ends = [
        apply(
            &m,
            (
                numbers(xline, "<line", "x1")[0],
                numbers(xline, "<line", "y1")[0],
            ),
        ),
        apply(
            &m,
            (
                numbers(xline, "<line", "x2")[0],
                numbers(xline, "<line", "y2")[0],
            ),
        ),
    ];
    // The window is the viewBox grown by a percent of its diagonal.
    let margin = 420f64.hypot(297.0) * 0.01;
    for (x, y) in ends {
        let on_edge = (x + margin).abs() < 1e-6
            || (x - 420.0 - margin).abs() < 1e-6
            || (y + 297.0 + margin).abs() < 1e-6
            || (y - margin).abs() < 1e-6;
        assert!(on_edge, "({x}, {y}) is not on the sheet's edge");
    }
    // And it is the model's line: both ends on the image of y = 30.
    let (x0, y0) = detail(0.0, 30.0);
    let (x1, y1) = detail(1.0, 30.0);
    for (x, y) in ends {
        let cross = (x1 - x0) * (-y - y0) - (y1 - y0) * (x - x0);
        assert!(cross.abs() < 1e-6, "({x}, {y}) is off the line");
    }
}

#[test]
fn a_sheet_rasterizes_at_the_size_asked_for() {
    let png = layout_to_png(
        &g14(),
        "Layout1",
        ToPngOptions {
            size: PngSize::FitLongEdge(840),
            ..ToPngOptions::default()
        },
    )
    .expect("the sheet rasterizes");
    let width = u32::from_be_bytes(png.png[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png.png[20..24].try_into().unwrap());
    assert_eq!((width, height), (840, 594));
    assert_eq!(png.hidden, 4);
    assert!(png.undrawn_viewports.is_empty());
}

#[test]
fn the_same_sheet_is_the_same_bytes() {
    let db = g14();
    let first = sheet(&db, "Layout1").svg;
    for _ in 0..8 {
        assert_eq!(sheet(&db, "Layout1").svg, first);
    }
}
