//! What a curved entity counts towards the picture's extent is its own box:
//! an arc's, not its whole circle's, mirrored or not; the ellipse arc that is
//! drawn, not the whole ellipse; and a circle or a polyline's arc inside a
//! rotated block all of itself.
//!
//! Every expected number is worked out from the geometry, not read off this
//! crate's output. The viewBox is the extent (padding 0, no outlier trim),
//! written as `x, -max_y, width, height`.

use std::collections::BTreeMap;
use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};

use iron_render_cad::limits::Cap;
use iron_render_cad::{to_svg, Space, ToSvgOptions};
use uncad_model::model::{
    ArcEntity, CircleEntity, Confidence, EllipseEntity, Entity, EntityCommon, EntityId,
    InsertEntity, LineEntity, LwPolylineEntity, Origin, Point2D, Point3D, PointEntity,
    PolylineVertex, Ref,
};
use uncad_model::tables::{BlockRecord, Tables};
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

fn xyz(x: f64, y: f64, z: f64) -> Point3D {
    Point3D { x, y, z }
}

fn arc(id: u64, start: f64, end: f64) -> Entity {
    Entity::Arc(ArcEntity {
        common: common(id),
        center: xyz(0.0, 0.0, 0.0),
        radius: 10.0,
        start_angle: start,
        end_angle: end,
        extrusion: uncad_model::Point3D {
            x: 0.0,
            y: 0.0,
            z: 1.0,
        },
    })
}

fn ellipse(start: f64, end: f64, normal_z: f64) -> Entity {
    // Major axis (20, 0), ratio 0.5: the point at parameter t is
    // (20 cos t, 10 sin t), or (20 cos t, -10 sin t) mirrored.
    Entity::Ellipse(EllipseEntity {
        common: common(0x20),
        center: xyz(0.0, 0.0, 0.0),
        major_axis_endpoint: xyz(20.0, 0.0, 0.0),
        axis_ratio: 0.5,
        start_angle: start,
        end_angle: end,
        extrusion: xyz(0.0, 0.0, normal_z),
    })
}

fn db(entities: Vec<Entity>, blocks: BTreeMap<String, BlockRecord>) -> CadDatabase {
    CadDatabase {
        entities,
        tables: Tables {
            block_records: blocks,
            ..Tables::default()
        },
        read_diagnostics: ReadDiagnostics::default(),
    }
}

/// The extent of what `entities` draw, as `[min_x, min_y, max_x, max_y]`.
fn extent(entities: Vec<Entity>, blocks: BTreeMap<String, BlockRecord>) -> [f64; 4] {
    let svg = to_svg(
        &db(entities, blocks),
        ToSvgOptions {
            space: Space::All,
            padding: 0.0,
            outlier_trim: false,
            ..ToSvgOptions::default()
        },
    )
    .svg;
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

#[test]
fn an_arc_counts_its_own_box_not_its_whole_circle() {
    // A quarter from 0 to 90 degrees: (10, 0) to (0, 10), crossing no other
    // axis direction.
    close(
        extent(vec![arc(0x10, 0.0, FRAC_PI_2)], BTreeMap::new()),
        [0.0, 0.0, 10.0, 10.0],
    );
    // Turned by 45 degrees it crosses the +x direction: x reaches 10, y
    // stays within 10 / sqrt 2.
    let h = 10.0 / 2f64.sqrt();
    close(
        extent(vec![arc(0x10, -FRAC_PI_4, FRAC_PI_4)], BTreeMap::new()),
        [h, -h, 10.0, h],
    );
    // 270 degrees from 0 crosses +y, -x and -y: the whole circle's box.
    close(
        extent(vec![arc(0x10, 0.0, 3.0 * FRAC_PI_2)], BTreeMap::new()),
        [-10.0, -10.0, 10.0, 10.0],
    );
}

#[test]
fn an_arc_whose_angle_names_no_direction_is_left_out_and_named() {
    // 1e20 and 1.4e247 (what a fuzzed DWG stored) are finite numbers, but
    // no direction: the render finishes, draws the sane arc, and names the
    // other two.
    let result = to_svg(
        &db(
            vec![
                arc(0x10, 0.0, FRAC_PI_2),
                arc(0x11, 1.0e20, 1.0),
                arc(0x12, 1.4052120271735542e247, 1.0),
            ],
            BTreeMap::new(),
        ),
        ToSvgOptions {
            space: Space::All,
            ..ToSvgOptions::default()
        },
    );
    assert_eq!(result.svg.matches("<path").count(), 1, "{}", result.svg);
    let named: Vec<(u64, Cap)> = result
        .limits
        .dropped
        .iter()
        .map(|d| (d.id.value(), d.cap))
        .collect();
    assert_eq!(
        named,
        vec![(0x11, Cap::NotANumber), (0x12, Cap::NotANumber)]
    );
}

#[test]
fn an_ellipse_arc_counts_the_part_that_is_drawn() {
    // Parameters 0 to pi: the upper half, x -20..20 and y 0..10.
    close(
        extent(vec![ellipse(0.0, PI, 1.0)], BTreeMap::new()),
        [-20.0, 0.0, 20.0, 10.0],
    );
    // Mirrored, the same parameters run the other way: the lower half.
    close(
        extent(vec![ellipse(0.0, PI, -1.0)], BTreeMap::new()),
        [-20.0, -10.0, 20.0, 0.0],
    );
    // pi/2 to pi: the upper-left quarter.
    close(
        extent(vec![ellipse(FRAC_PI_2, PI, 1.0)], BTreeMap::new()),
        [-20.0, 0.0, 0.0, 10.0],
    );
    // The whole ellipse is 40 x 20, not 40 x 40.
    close(
        extent(vec![ellipse(0.0, 2.0 * PI, 1.0)], BTreeMap::new()),
        [-20.0, -10.0, 20.0, 10.0],
    );
}

#[test]
fn a_circle_in_a_rotated_block_counts_all_of_itself() {
    // A circle of radius 1 at the origin, inside a block placed at (5, 5)
    // and turned 45 degrees. Two opposite corners of its box land on one
    // vertical line after the turn; the circle is still 2 x 2 wide.
    let mut blocks = BTreeMap::new();
    blocks.insert(
        "C".to_string(),
        BlockRecord {
            name: "C".to_string(),
            entities: vec![Entity::Circle(CircleEntity {
                common: common(0x50),
                center: xyz(0.0, 0.0, 0.0),
                radius: 1.0,
                extrusion: uncad_model::Point3D {
                    x: 0.0,
                    y: 0.0,
                    z: 1.0,
                },
            })],
        },
    );
    let insert = Entity::Insert(InsertEntity {
        common: common(0x10),
        block_name: Ref::Resolved("C".to_string()),
        insertion_point: xyz(5.0, 5.0, 0.0),
        scale: xyz(1.0, 1.0, 1.0),
        rotation: FRAC_PI_4,
        attribs: Vec::new(),
        extrusion: Point3D {
            x: 0.0,
            y: 0.0,
            z: 1.0,
        },
    });
    let [min_x, min_y, max_x, max_y] = extent(vec![insert], blocks);
    // The turned box is a diamond reaching sqrt 2 from the centre: it
    // contains the circle, which a zero-width box does not.
    assert!(max_x - min_x >= 2.0 && max_y - min_y >= 2.0);
    assert!((max_x - min_x - 2.0 * 2f64.sqrt()).abs() < 1e-9);
    assert!(((min_x + max_x) / 2.0 - 5.0).abs() < 1e-9);
    assert!(((min_y + max_y) / 2.0 - 5.0).abs() < 1e-9);
}

#[test]
fn a_point_counts_itself_not_the_cross_it_is_drawn_as() {
    // The cross is sized in stroke widths, a page quantity: were it part of
    // the extent, the extent would depend on the stroke.
    let entities = vec![
        Entity::Line(LineEntity {
            common: common(0x10),
            start_point: xyz(0.0, 0.0, 0.0),
            end_point: xyz(10.0, 0.0, 0.0),
        }),
        Entity::Point(PointEntity {
            common: common(0x11),
            position: xyz(10.0, 5.0, 0.0),
        }),
    ];
    close(extent(entities, BTreeMap::new()), [0.0, 0.0, 10.0, 5.0]);
}

#[test]
fn a_mirrored_arc_counts_its_own_box_not_its_whole_circle() {
    // A quarter from 0 to 90 degrees about the origin, in a mirror copy's
    // plane: in the world it runs clockwise from (-10, 0) to (0, 10), and
    // its box is the quarter's, not the circle's 20 x 20.
    let Entity::Arc(mut mirrored) = arc(0x10, 0.0, FRAC_PI_2) else {
        unreachable!()
    };
    mirrored.extrusion = xyz(0.0, 0.0, -1.0);
    close(
        extent(vec![Entity::Arc(mirrored)], BTreeMap::new()),
        [-10.0, 0.0, 0.0, 10.0],
    );
}

#[test]
fn a_bulged_polyline_in_a_rotated_block_counts_all_of_its_arc() {
    // A half circle of radius 1 from (-1, 0) to (1, 0) through (0, -1)
    // (bulge 1), inside a block placed at (5, 5) and turned 45 degrees. In
    // the world the arc reaches x = 6 and y = 4, which none of its ends or
    // its local extreme reach; its local box, [-1, 1] x [-1, 0], turned,
    // contains it.
    let mut blocks = BTreeMap::new();
    blocks.insert(
        "P".to_string(),
        BlockRecord {
            name: "P".to_string(),
            entities: vec![Entity::LwPolyline(LwPolylineEntity {
                common: common(0x50),
                vertices: vec![
                    PolylineVertex {
                        point: Point2D { x: -1.0, y: 0.0 },
                        bulge: 1.0,
                        ..PolylineVertex::default()
                    },
                    PolylineVertex::straight(Point2D { x: 1.0, y: 0.0 }),
                ],
                closed: false,
                const_width: 0.0,
                elevation: 0.0,
                extrusion: xyz(0.0, 0.0, 1.0),
            })],
        },
    );
    let insert = Entity::Insert(InsertEntity {
        common: common(0x10),
        block_name: Ref::Resolved("P".to_string()),
        insertion_point: xyz(5.0, 5.0, 0.0),
        scale: xyz(1.0, 1.0, 1.0),
        rotation: FRAC_PI_4,
        attribs: Vec::new(),
        extrusion: xyz(0.0, 0.0, 1.0),
    });
    let [min_x, min_y, max_x, max_y] = extent(vec![insert], blocks);
    let h = std::f64::consts::FRAC_1_SQRT_2;
    assert!(min_x <= 5.0 - h + 1e-9 && max_x >= 6.0, "{min_x} {max_x}");
    assert!(min_y <= 4.0 && max_y >= 5.0 + h - 1e-9, "{min_y} {max_y}");
    // Exactly the turned box's: its corners (1, -1) and (-1, -1) land at
    // x = 5 + sqrt 2 and y = 5 - sqrt 2.
    close(
        [min_x, min_y, max_x, max_y],
        [5.0 - h, 5.0 - 2f64.sqrt(), 5.0 + 2f64.sqrt(), 5.0 + h],
    );
}
