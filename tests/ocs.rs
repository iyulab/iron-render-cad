//! Planar entities are drawn where their plane puts them: CIRCLE, ARC,
//! LWPOLYLINE, TEXT, SOLID and INSERT state their coordinates in the object
//! coordinate system their `extrusion` defines, and the model carries them
//! as stated. A mirrored plane (normal (0, 0, -1)) sends x to -x -- an arc
//! then runs clockwise, a bulge turns the other way, a text reads
//! backwards -- and a tilted plane is seen from above, where a circle is an
//! ellipse.
//!
//! Every expected number is worked out from the DXF reference's arbitrary
//! axis algorithm, not read off this crate's output.

use std::f64::consts::FRAC_1_SQRT_2;

use iron_render_cad::{to_svg, Space, ToSvgOptions, ToSvgResult};
use uncad_model::model::{
    CircleEntity, Confidence, Entity, EntityCommon, EntityId, Origin, Point3D, Ref,
};
use uncad_model::tables::Tables;
use uncad_model::{CadDatabase, ReadDiagnostics};

fn render(db: &CadDatabase) -> ToSvgResult {
    to_svg(
        db,
        ToSvgOptions {
            space: Space::All,
            padding: 0.0,
            outlier_trim: false,
            ..ToSvgOptions::default()
        },
    )
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

fn g11() -> CadDatabase {
    serde_json::from_str(include_str!("golden/g11.expected.json"))
        .expect("the golden model deserializes")
}

#[test]
fn g11_draws_each_mirrored_entity_on_the_other_side_of_the_y_axis() {
    let result = render(&g11());
    let svg = &result.svg;
    // The same circle stated twice: once in the world's plane, once in the
    // mirrored one, where the stated (30, 20) is the world's (-30, 20).
    assert!(
        svg.contains("<circle cx=\"30\" cy=\"-20\" r=\"5\""),
        "{svg}"
    );
    assert!(
        svg.contains("<circle cx=\"-30\" cy=\"-20\" r=\"5\""),
        "{svg}"
    );
    // The quarter arc from 0 to 90 degrees about (30, 50) in the mirrored
    // plane: from the world's (-40, 50) clockwise to (-30, 60) -- sweep
    // flag 1, the way a negative bulge turns.
    assert!(
        svg.contains("<path d=\"M -40 -50 A 10 10 0 0 1 -30 -60\""),
        "{svg}"
    );
    // The slot whose right end bulges out (bulge 1) in its own plane bulges
    // out of its left end in the world: the bulge turns clockwise.
    assert!(
        svg.contains("<path d=\"M -20 0 L -40 0 A 5 5 0 0 1 -40 -10 L -20 -10"),
        "{svg}"
    );
    // The text reads backwards from the world's (-20, -10), as a text seen
    // from behind does.
    assert!(
        svg.contains("transform=\"matrix(-1 0 0 1 -20 10)\">MIRRORED</text>"),
        "{svg}"
    );
    // The SOLID's corners, in its 1-2-4-3 drawing order.
    assert!(
        svg.contains("<polygon points=\"-20,30 -30,30 -30,20 -20,20\""),
        "{svg}"
    );
    // The block reference at the mirrored plane's (50, 0), turned 30
    // degrees there: the world's (-50, 0), the block's x axis pointing up
    // and to the left, (-cos 30, sin 30) -- y flipped in SVG.
    let at = svg
        .find("<g transform=\"matrix(")
        .expect("the block reference")
        + "<g transform=\"matrix(".len();
    let m: Vec<f64> = svg[at..at + svg[at..].find(')').unwrap()]
        .split(' ')
        .map(|v| v.parse().unwrap())
        .collect();
    let (cos, sin) = (30f64.to_radians().cos(), 30f64.to_radians().sin());
    for (got, want) in m.iter().zip([-cos, -sin, -sin, cos, -50.0, 0.0]) {
        assert!((got - want).abs() < 1e-12, "{m:?}");
    }
    // The extent: from the block's circle -- radius 2 about the block's
    // (10, 0), measured through the four corners of its box, the farthest
    // of which, (12, -2), lands at x = -(50 + 12 cos 30 + 2 sin 30) -- to
    // the unmirrored circle's right edge, and from the SOLID's bottom to the
    // arc's top.
    close(
        extent(svg),
        [-(50.0 + 12.0 * cos + 2.0 * sin), -30.0, 35.0, 60.0],
    );
    assert!(!result.limits.engaged());
}

fn circle(normal: Point3D, z: f64) -> Entity {
    Entity::Circle(CircleEntity {
        common: EntityCommon {
            id: EntityId::new(0x10),
            origin: Origin::Vector,
            confidence: Confidence::High,
            source_handle: Ref::Resolved("10".to_string()),
            layer: Ref::Resolved("0".to_string()),
            color_index: 7,
            true_color: None,
            invisible: false,
        },
        center: Point3D { x: 0.0, y: 0.0, z },
        radius: 10.0,
        extrusion: normal,
    })
}

fn render_one(entity: Entity) -> ToSvgResult {
    render(&CadDatabase {
        entities: vec![entity],
        tables: Tables::default(),
        read_diagnostics: ReadDiagnostics::default(),
    })
}

#[test]
fn a_circle_on_a_tilted_plane_is_the_ellipse_seen_from_above() {
    // Normal (1, 0, 1) / sqrt 2: the plane's x axis is z x N, the world's
    // y axis, and its y axis N x (0, 1, 0) = (-1, 0, 1) / sqrt 2. Seen from
    // above, the circle of radius 10 about the plane's origin is an ellipse
    // 10 along y and 10 / sqrt 2 along x, about the world's origin.
    let result = render_one(circle(
        Point3D {
            x: FRAC_1_SQRT_2,
            y: 0.0,
            z: FRAC_1_SQRT_2,
        },
        0.0,
    ));
    assert!(!result.svg.contains("<circle"), "{}", result.svg);
    let points = result
        .svg
        .split("<polygon points=\"")
        .nth(1)
        .expect("drawn through points")
        .split('"')
        .next()
        .unwrap()
        .split(' ')
        .count();
    assert_eq!(points, 64);
    let h = 10.0 * FRAC_1_SQRT_2;
    close(extent(&result.svg), [-h, -10.0, h, 10.0]);
    // Its height in the plane moves it along the normal: at z = 4 the
    // centre is 4 / sqrt 2 along x.
    let lifted = render_one(circle(
        Point3D {
            x: 1.0,
            y: 0.0,
            z: 1.0,
        },
        4.0,
    ));
    let dx = 4.0 * FRAC_1_SQRT_2;
    close(extent(&lifted.svg), [dx - h, -10.0, dx + h, 10.0]);
}

#[test]
fn a_normal_that_is_not_a_number_leaves_the_entity_out_and_says_so() {
    let result = render_one(circle(
        Point3D {
            x: f64::NAN,
            y: 0.0,
            z: 1.0,
        },
        0.0,
    ));
    assert!(!result.svg.contains("<circle"), "{}", result.svg);
    assert_eq!(result.limits.unreadable_entities, 1);
}
