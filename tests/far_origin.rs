//! A drawing far from the world origin is written about a point near its
//! own middle, so no number the rasterizer reads is large.
//!
//! usvg and tiny-skia keep path points in `f32`, which at 2.5e8 -- a plan in
//! millimetres at projected map coordinates -- cannot tell two points 16
//! units apart. Writing world coordinates straight into the SVG quantized
//! every such drawing; a DIMENSION, whose block is placed through an
//! identity because its children already hold world coordinates, was
//! quantized even inside its group.

use std::collections::BTreeMap;

use iron_render_cad::{to_png, to_svg, Space, ToPngOptions, ToSvgOptions};
use uncad_model::model::{
    Confidence, DimensionEntity, DimensionPoints, Entity, EntityCommon, EntityId, InsertEntity,
    LineEntity, Origin, Point2D, Point3D, Ref, TextOverride,
};
use uncad_model::tables::{BlockRecord, Tables};
use uncad_model::{CadDatabase, ReadDiagnostics};

const FAR: f64 = 2.5e8;

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

fn xyz(x: f64, y: f64) -> Point3D {
    Point3D { x, y, z: 0.0 }
}

fn line(id: u64, from: (f64, f64), to: (f64, f64)) -> Entity {
    Entity::Line(LineEntity {
        common: common(id),
        start_point: xyz(from.0, from.1),
        end_point: xyz(to.0, to.1),
    })
}

/// Every number in every attribute of `svg`.
fn numbers(svg: &str) -> Vec<f64> {
    svg.split('"')
        .skip(1)
        .step_by(2)
        .flat_map(|value| {
            value
                .replace(['(', ')', ','], " ")
                .split_whitespace()
                .filter_map(|t| t.parse::<f64>().ok())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The far drawing: a LINE, a block placed by an INSERT, and a DIMENSION
/// whose block holds a line in world coordinates.
fn far_drawing() -> CadDatabase {
    let mut block_records = BTreeMap::new();
    block_records.insert(
        "TICK".to_string(),
        BlockRecord {
            name: "TICK".to_string(),
            entities: vec![line(0x50, (0.0, 0.0), (2.0, 2.0))],
        },
    );
    block_records.insert(
        "*D1".to_string(),
        BlockRecord {
            name: "*D1".to_string(),
            entities: vec![line(0x51, (FAR + 10.0, FAR), (FAR + 90.0, FAR))],
        },
    );
    CadDatabase {
        entities: vec![
            line(0x10, (FAR + 10.0, FAR + 10.0), (FAR + 90.0, FAR + 50.0)),
            Entity::Insert(InsertEntity {
                common: common(0x11),
                block_name: Ref::Resolved("TICK".to_string()),
                insertion_point: xyz(FAR + 40.0, FAR + 20.0),
                scale: Point3D {
                    x: 1.0,
                    y: 1.0,
                    z: 1.0,
                },
                rotation: 0.0,
                attribs: Vec::new(),
                extrusion: Point3D {
                    x: 0.0,
                    y: 0.0,
                    z: 1.0,
                },
            }),
            Entity::Dimension(DimensionEntity {
                common: common(0x12),
                block_name: Ref::Resolved("*D1".to_string()),
                kind: None,
                measurement: None,
                text_override: TextOverride::Measured,
                definition_point: None,
                text_midpoint: Point2D {
                    x: FAR + 50.0,
                    y: FAR,
                },
                points: DimensionPoints::default(),
                rotation: 0.0,
                text_rotation: 0.0,
                style_name: Ref::Absent,
                ordinate_axis: None,
            }),
        ],
        tables: Tables {
            block_records,
            ..Tables::default()
        },
        read_diagnostics: ReadDiagnostics::default(),
    }
}

fn all() -> ToSvgOptions {
    ToSvgOptions {
        space: Space::All,
        ..ToSvgOptions::default()
    }
}

#[test]
fn a_far_away_drawing_is_written_about_its_own_middle() {
    let result = to_svg(&far_drawing(), all());

    // The median of the three reference points, rounded: x of (FAR + 10,
    // FAR + 40, FAR + 50) and y of (FAR + 10, FAR + 20, FAR).
    assert_eq!(
        result.origin,
        Point2D {
            x: FAR + 40.0,
            y: FAR + 10.0
        }
    );
    let largest = numbers(&result.svg)
        .into_iter()
        .map(f64::abs)
        .fold(0.0, f64::max);
    assert!(
        largest < 1000.0,
        "a number of {largest} reached the SVG: {}",
        result.svg
    );
    // The top-level LINE, relative to the origin and y-flipped.
    assert!(
        result
            .svg
            .contains("x1=\"-30\" y1=\"0\" x2=\"50\" y2=\"-40\""),
        "{}",
        result.svg
    );
    // The DIMENSION's block line, at world (FAR + 10, FAR): its interior is
    // written in the render's own frame, and its group moves nothing.
    assert!(
        result
            .svg
            .contains("x1=\"-30\" y1=\"10\" x2=\"50\" y2=\"10\""),
        "{}",
        result.svg
    );
    assert!(result.svg.contains("matrix(1 0 0 1 0 0)"), "{}", result.svg);
    assert!(!result.limits.engaged(), "{:?}", result.limits);
}

#[test]
fn the_origin_maps_the_viewbox_back_to_world_coordinates() {
    let result = to_svg(&far_drawing(), all());
    let start = result.svg.find("viewBox=\"").unwrap() + "viewBox=\"".len();
    let end = result.svg[start..].find('"').unwrap() + start;
    let v: Vec<f64> = result.svg[start..end]
        .split_whitespace()
        .map(|n| n.parse().unwrap())
        .collect();
    // World extent: x from FAR + 10 to FAR + 90, y from FAR to FAR + 50,
    // padded by 5. An SVG unit (u, v) is the world point (origin.x + u,
    // origin.y - v).
    let (padding, o) = (5.0, result.origin);
    assert_eq!(o.x + v[0], FAR + 10.0 - padding);
    assert_eq!(o.x + v[0] + v[2], FAR + 90.0 + padding);
    assert_eq!(o.y - v[1], FAR + 50.0 + padding);
    assert_eq!(o.y - v[1] - v[3], FAR - padding);
}

#[test]
fn a_drawing_near_the_origin_is_written_in_world_units() {
    let db = CadDatabase {
        entities: vec![line(0x10, (10.0, 10.0), (20.0, 30.0))],
        tables: Tables::default(),
        read_diagnostics: ReadDiagnostics::default(),
    };
    let result = to_svg(&db, all());
    assert_eq!(result.origin, Point2D { x: 0.0, y: 0.0 });
    assert!(result
        .svg
        .contains("x1=\"10\" y1=\"-10\" x2=\"20\" y2=\"-30\""));
}

#[test]
fn a_far_away_drawing_rasterizes() {
    let png = to_png(
        &far_drawing(),
        ToPngOptions {
            svg: all(),
            ..ToPngOptions::default()
        },
    )
    .expect("the far drawing rasterizes");
    assert!(png.png.starts_with(b"\x89PNG"));
}
