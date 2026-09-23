//! A polyline's bulges are arcs: drawn as exact SVG arcs turning the way
//! the bulge's sign says, measured by each arc's own box, and a bulge so
//! small its arc is a straight line in all but name drawn as that line.
//!
//! Every expected number is worked out from the geometry (a bulge is
//! `tan(theta / 4)` of the arc's included angle), not read off this crate's
//! output. The viewBox is the extent (padding 0, no outlier trim), written
//! as `x, -max_y, width, height`.

use iron_render_cad::{to_svg, Crop, Space, ToSvgOptions, ToSvgResult};
use uncad_model::model::{
    Confidence, Entity, EntityCommon, EntityId, LwPolylineEntity, Origin, Point2D, Point3D,
    PolylineVertex, Ref,
};
use uncad_model::tables::Tables;
use uncad_model::{CadDatabase, ReadDiagnostics};

fn common(id: u64) -> EntityCommon {
    EntityCommon {
        id: EntityId::new(id),
        origin: Origin::Vector,
        confidence: Confidence::High,
        source_handle: Ref::Resolved(format!("{id:X}")),
        layer: Ref::Resolved("0".to_string()),
        color_index: 7,
        true_color: None,
        invisible: false,
    }
}

/// A polyline whose vertex `i` has `bulges[i]`, or 0 past the list's end.
fn polyline(vertices: &[(f64, f64)], bulges: &[f64], closed: bool) -> Entity {
    Entity::LwPolyline(LwPolylineEntity {
        common: common(0x10),
        vertices: vertices
            .iter()
            .enumerate()
            .map(|(i, &(x, y))| PolylineVertex {
                point: Point2D { x, y },
                bulge: bulges.get(i).copied().unwrap_or(0.0),
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

fn render(db: &CadDatabase) -> ToSvgResult {
    to_svg(
        db,
        ToSvgOptions {
            space: Space::All,
            padding: 0.0,
            crop: Crop::Everything,
            ..ToSvgOptions::default()
        },
    )
}

fn render_one(entity: Entity) -> ToSvgResult {
    render(&CadDatabase {
        entities: vec![entity],
        tables: Tables::default(),
        read_diagnostics: ReadDiagnostics::default(),
    })
}

/// The extent the viewBox shows, as `[min_x, min_y, max_x, max_y]`.
fn extent(svg: &str) -> [f64; 4] {
    let start = svg.find("viewBox=\"").unwrap() + "viewBox=\"".len();
    let end = svg[start..].find('"').unwrap() + start;
    let v: Vec<f64> = svg[start..end]
        .split_whitespace()
        .map(|n| n.parse().unwrap())
        .collect();
    [v[0], -v[1] - v[3], v[0] + v[2], -v[1]]
}

fn close(actual: [f64; 4], expected: [f64; 4]) {
    for (a, e) in actual.iter().zip(expected) {
        assert!((a - e).abs() < 1e-9, "{actual:?} vs {expected:?}");
    }
}

/// Every arc command of the one `<path>`: `[rx, ry, rotation, large,
/// sweep, x, y]`.
fn arcs(svg: &str) -> Vec<[f64; 7]> {
    let d = svg.split("<path d=\"").nth(1).expect("a path");
    let d = &d[..d.find('"').unwrap()];
    d.split(" A ")
        .skip(1)
        .map(|arc| {
            let numbers: Vec<f64> = arc
                .split_whitespace()
                .take(7)
                .map(|n| n.parse().unwrap())
                .collect();
            numbers.try_into().expect("seven numbers")
        })
        .collect()
}

#[test]
fn a_positive_bulge_turns_counter_clockwise_and_counts_its_arc_towards_the_extent() {
    // (0,0) -> (10,0) with bulge 1: a half circle about (5,0) running
    // counter-clockwise, which takes it below the chord to (5,-5). SVG's y
    // points down, so that is sweep flag 0.
    let result = render_one(polyline(&[(0.0, 0.0), (10.0, 0.0)], &[1.0, 0.0], false));
    assert!(
        result.svg.contains("<path d=\"M 0 0 A 5 5 0 0 0 10 0\""),
        "{}",
        result.svg
    );
    close(extent(&result.svg), [0.0, -5.0, 10.0, 0.0]);
}

#[test]
fn a_negative_bulge_turns_clockwise() {
    let result = render_one(polyline(&[(0.0, 0.0), (10.0, 0.0)], &[-1.0], false));
    assert!(
        result.svg.contains("<path d=\"M 0 0 A 5 5 0 0 1 10 0\""),
        "{}",
        result.svg
    );
    close(extent(&result.svg), [0.0, 0.0, 10.0, 5.0]);
}

#[test]
fn a_bulge_past_a_half_circle_takes_the_large_arc() {
    // Bulge 2 over a chord of 10: radius 10 (1 + 4) / 8 = 6.25, centre
    // 3.75 below the chord's middle, the arc from (0,0) round through the
    // -x, -y and +x directions to (10,0).
    let result = render_one(polyline(&[(0.0, 0.0), (10.0, 0.0)], &[2.0], false));
    let [rx, _, _, large, sweep, x, y] = arcs(&result.svg)[0];
    assert!((rx - 6.25).abs() < 1e-12, "{rx}");
    assert_eq!((large, sweep, x, y), (1.0, 0.0, 10.0, 0.0));
    close(extent(&result.svg), [-1.25, -10.0, 11.25, 0.0]);
}

#[test]
fn an_arc_too_flat_to_rasterize_is_drawn_as_its_chord() {
    // A bulge of 1e-160 over ten units is an arc of radius 6e160: handed to
    // the rasterizer as an arc it converts to curves at a scale unrelated
    // to the segment's. It is its chord in all but name.
    let result = render_one(polyline(&[(0.0, 0.0), (10.0, 5.0)], &[1e-160], false));
    assert!(arcs(&result.svg).is_empty(), "{}", result.svg);
    assert!(
        result.svg.contains("<path d=\"M 0 0 L 10 -5\""),
        "{}",
        result.svg
    );
    close(extent(&result.svg), [0.0, 0.0, 10.0, 5.0]);
}

#[test]
fn a_bulge_that_is_not_a_number_leaves_the_polyline_out_and_says_so() {
    let result = render_one(polyline(&[(0.0, 0.0), (10.0, 0.0)], &[f64::NAN], false));
    assert!(!result.svg.contains("<path"), "{}", result.svg);
    assert_eq!(result.limits.unreadable_entities, 1);
}

#[test]
fn g12_draws_its_slot_donut_and_rounded_corner_as_arcs() {
    let db: CadDatabase = serde_json::from_str(include_str!("golden/g12.expected.json"))
        .expect("the golden model deserializes");
    let result = render(&db);
    let svg = &result.svg;
    // The slot: two straight sides and a half circle at each end, both
    // counter-clockwise (bulge 1), the right one out to x = 45, the left one
    // out to x = -5.
    assert!(
        svg.contains("<path d=\"M 0 0 L 40 0 A 5 5 0 0 0 40 -10 L 0 -10 A 5 5 0 0 0 0 0 Z\""),
        "{svg}"
    );
    // The DONUT: two vertices, two half circles, one circle about (65, 20).
    assert!(
        svg.contains("<path d=\"M 60 -20 A 5 5 0 0 0 70 -20 A 5 5 0 0 0 60 -20 Z\""),
        "{svg}"
    );
    // The rounded corner turns clockwise (a negative bulge): a quarter
    // circle of radius 10 about (50, 50).
    let corner = svg
        .split("<path d=\"M 0 -50")
        .nth(1)
        .expect("the corner polyline is a path");
    let corner = &corner[..corner.find('"').unwrap()];
    let a = corner.split(" A ").nth(1).expect("an arc");
    let n: Vec<f64> = a
        .split_whitespace()
        .take(7)
        .map(|v| v.parse().unwrap())
        .collect();
    assert!((n[0] - 10.0).abs() < 1e-9, "{n:?}");
    assert_eq!(&n[3..], &[0.0, 1.0, 50.0, -60.0], "{n:?}");
    // Five arcs in all, and the two polylines without bulges -- the
    // tapered arrow and the one whose zero bulges read as none -- stay
    // polylines. A polyline's width is not drawn: the arrow is its
    // centreline at the ordinary stroke (see CAVEATS).
    assert_eq!(svg.matches(" A ").count(), 5, "{svg}");
    assert_eq!(svg.matches("<polyline ").count(), 2, "{svg}");
    assert!(svg.contains("<polyline points=\"0,-30 30,-30 40,-30\" fill=\"none\""));
    // The extent reaches the slot's left end and the DONUT's right edge.
    close(extent(svg), [-5.0, 0.0, 70.0, 90.0]);
}
