//! Where a picture's pixels lie in the world. A [`View`] is the pixel grid
//! laid over the drawing -- its size, its scale and the world point of its
//! top-left corner -- so a caller can say which world point a pixel shows
//! and the other way round; [`Scene::png`] draws exactly that grid, and
//! every [`ToPngResult`] says which grid it drew. No pixel is compared:
//! the sizes are read from the PNG header.

use std::collections::BTreeMap;

use iron_render_cad::{
    to_png, to_svg, Background, Fonts, PngError, PngSize, Rect, Scene, Space, ToPngOptions,
    ToSvgOptions, View, DEFAULT_MAX_EDGE,
};
use uncad_model::model::{
    Confidence, Entity, EntityCommon, EntityId, LineEntity, Origin, Point2D, Point3D, Ref,
};
use uncad_model::tables::Tables;
use uncad_model::{CadDatabase, ReadDiagnostics};

fn line(id: u64, from: (f64, f64), to: (f64, f64)) -> Entity {
    Entity::Line(LineEntity {
        common: EntityCommon {
            id: EntityId::new(id),
            origin: Origin::Vector,
            confidence: Confidence::High,
            source_handle: Ref::Resolved(format!("{id:X}")),
            layer: Ref::Resolved("0".to_string()),
            color_index: 7,
            true_color: None,
            invisible: false,
        },
        start_point: Point3D {
            x: from.0,
            y: from.1,
            z: 0.0,
        },
        end_point: Point3D {
            x: to.0,
            y: to.1,
            z: 0.0,
        },
    })
}

/// A 40 x 10 frame whose lower-left corner is at (x, y).
fn frame(x: f64, y: f64) -> CadDatabase {
    CadDatabase {
        entities: vec![
            line(0x1, (x, y), (x + 40.0, y)),
            line(0x2, (x + 40.0, y), (x + 40.0, y + 10.0)),
            line(0x3, (x + 40.0, y + 10.0), (x, y + 10.0)),
            line(0x4, (x, y + 10.0), (x, y)),
        ],
        tables: Tables {
            block_records: BTreeMap::new(),
            ..Tables::default()
        },
        read_diagnostics: ReadDiagnostics::default(),
    }
}

fn all() -> ToSvgOptions {
    ToSvgOptions {
        space: Space::All,
        padding: 1.0,
        ..ToSvgOptions::default()
    }
}

/// Width and height from a PNG's IHDR chunk.
fn png_size(png: &[u8]) -> (u32, u32) {
    (
        u32::from_be_bytes(png[16..20].try_into().unwrap()),
        u32::from_be_bytes(png[20..24].try_into().unwrap()),
    )
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn a_view_maps_world_points_to_pixels_and_back() {
    let view = View::of(Rect::new(-20.0, 5.0, 60.0, 45.0), 1.5).expect("a view");
    assert_eq!((view.width, view.height), (120, 60));
    assert_eq!((view.left, view.top, view.px_per_unit), (-20.0, 45.0, 1.5));
    // The corners of the window are the corners of the picture.
    assert_eq!(view.world_to_px(Point2D { x: -20.0, y: 45.0 }), [0.0, 0.0]);
    assert_eq!(view.world_to_px(Point2D { x: 60.0, y: 5.0 }), [120.0, 60.0]);
    assert_eq!(view.window(), Rect::new(-20.0, 5.0, 60.0, 45.0));
    // And the map round-trips anywhere.
    let p = view.px_to_world([100.5, 20.25]);
    let back = view.world_to_px(p);
    assert!(close(back[0], 100.5) && close(back[1], 20.25), "{back:?}");
}

#[test]
fn a_window_that_is_not_whole_pixels_ends_where_its_last_pixel_does() {
    // 10.1 x 3.3 units at 10 px a unit is 101 x 33 pixels exactly; at 3 px
    // a unit it is 30.3 x 9.9, so 30 x 10 pixels, and the window they cover
    // is 10 x 3.333 units from the same top-left corner.
    let window = Rect::new(0.0, 0.0, 10.1, 3.3);
    let view = View::of(window, 3.0).expect("a view");
    assert_eq!((view.width, view.height), (30, 10));
    let covered = view.window();
    assert_eq!((covered.min_x, covered.max_y), (0.0, 3.3));
    assert!(close(covered.max_x, 10.0) && close(covered.min_y, 3.3 - 10.0 / 3.0));
    // No scale, no view.
    assert!(View::of(window, 0.0).is_none());
    assert!(View::of(window, f64::NAN).is_none());
    assert!(View::of(Rect::new(0.0, 0.0, 1e30, 1.0), 1.0).is_none());
}

#[test]
fn a_png_says_which_pixels_it_drew() {
    let db = frame(0.0, 0.0);
    let svg = to_svg(&db, all());
    for size in [PngSize::Scale(4.0), PngSize::FitLongEdge(500)] {
        let result = to_png(
            &db,
            ToPngOptions {
                svg: all(),
                size,
                ..ToPngOptions::default()
            },
        )
        .expect("renders");
        let view = result.view;
        assert_eq!(png_size(&result.png), (view.width, view.height));
        // Its corner is the document's viewBox corner, in world units.
        assert_eq!((view.left, view.top), (-1.0, 11.0));
        assert_eq!(
            (view.left, view.top),
            (svg.view_box.min_x, svg.view_box.max_y)
        );
        // And it covers the viewBox to within half a pixel.
        let covered = view.window();
        let half = 0.5 / view.px_per_unit;
        assert!((covered.max_x - svg.view_box.max_x).abs() <= half + 1e-9);
        assert!((covered.min_y - svg.view_box.min_y).abs() <= half + 1e-9);
    }
}

#[test]
fn a_scene_draws_exactly_the_pixels_its_view_asks_for() {
    let db = frame(0.0, 0.0);
    let scene = Scene::new(&db, all());
    // A window that is not whole pixels at this scale: the picture is the
    // view's size all the same.
    let view = View {
        left: 3.0,
        top: 9.5,
        px_per_unit: 7.3,
        width: 111,
        height: 37,
    };
    let png = scene
        .png(&view, 1.25, &Fonts::System, Background::White, |_| true)
        .expect("renders");
    assert_eq!(png_size(&png), (111, 37));
    // Drawn once, the same bytes every time.
    let again = scene
        .png(&view, 1.25, &Fonts::System, Background::White, |_| true)
        .expect("renders");
    assert_eq!(png, again);
}

#[test]
fn a_far_away_scene_draws_its_views_in_small_numbers() {
    // The drawing 2.5e8 units out: the rasterizer keeps coordinates in
    // `f32`, which cannot tell points 16 units apart there. The scene is
    // written about a point of its own, so a view of it is drawn from
    // small numbers -- the document's viewBox is the view's window moved
    // by the scene's origin -- at the view's size.
    let far = Scene::new(&frame(2.5e8, -2.5e8), all());
    assert!(far.origin.x > 32768.0);
    let view = View::of(
        Rect::new(2.5e8 - 1.0, -2.5e8 - 1.0, 2.5e8 + 41.0, -2.5e8 + 11.0),
        10.0,
    )
    .unwrap();
    assert_eq!((view.width, view.height), (420, 120));
    let svg = far.svg(view.window(), 0.1, |_| true);
    let at = svg.find("viewBox=\"").unwrap() + "viewBox=\"".len();
    let numbers: Vec<f64> = svg[at..at + svg[at..].find('"').unwrap()]
        .split(' ')
        .map(|v| v.parse().unwrap())
        .collect();
    assert!(numbers.iter().all(|v| v.abs() < 100.0), "{numbers:?}");
    let png = far
        .png(&view, 1.0, &Fonts::System, Background::White, |_| true)
        .expect("renders");
    assert_eq!(png_size(&png), (420, 120));
}

#[test]
fn a_view_with_no_pixels_or_too_many_is_refused_before_drawing() {
    let scene = Scene::new(&frame(0.0, 0.0), all());
    let view = |width, height| View {
        left: 0.0,
        top: 10.0,
        px_per_unit: 1.0,
        width,
        height,
    };
    let draw = |v: View| scene.png(&v, 1.0, &Fonts::System, Background::White, |_| true);
    assert!(matches!(draw(view(0, 10)), Err(PngError::EmptyCanvas)));
    assert!(matches!(
        draw(view(DEFAULT_MAX_EDGE + 1, 10)),
        Err(PngError::TooLarge { width, .. }) if width == DEFAULT_MAX_EDGE + 1
    ));
    assert!(draw(view(DEFAULT_MAX_EDGE, 10)).is_ok());
    let unscaled = View {
        px_per_unit: 0.0,
        ..view(10, 10)
    };
    assert!(matches!(draw(unscaled), Err(PngError::EmptyCanvas)));
}
