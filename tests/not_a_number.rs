//! A coordinate that is not a number never reaches the SVG, and the entity
//! it belongs to is counted and named rather than drawn somewhere it was not.
//!
//! Rust writes `NaN` and infinity as the literals `NaN` and `inf`, neither
//! of which is in SVG's `<number>` grammar: one such attribute puts the
//! element in error for a conforming reader. They reach a model from a
//! corrupt or half-decoded file.

use std::collections::BTreeMap;

use iron_render_cad::limits::Cap;
use iron_render_cad::{to_png, to_svg, Space, ToPngOptions, ToSvgOptions};
use uncad_model::model::{
    CircleEntity, Confidence, Entity, EntityCommon, EntityId, InsertEntity, LineEntity, Origin,
    Point3D, Ref, TextEntity,
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
        linetype: uncad_model::model::EntityLinetype::ByLayer,
        linetype_scale: 1.0,
        lineweight: Some(-1),
        transparency: Some(0),
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

fn all() -> ToSvgOptions {
    ToSvgOptions {
        space: Space::All,
        ..ToSvgOptions::default()
    }
}

#[test]
fn a_non_finite_coordinate_never_reaches_an_svg_attribute() {
    let (nan, inf) = (f64::NAN, f64::INFINITY);
    let mut block_records = BTreeMap::new();
    block_records.insert(
        "B".to_string(),
        BlockRecord {
            base_point: Default::default(),
            name: "B".to_string(),
            entities: vec![line(0x50, (0.0, 0.0), (10.0, 10.0))],
        },
    );
    let db = CadDatabase {
        entities: vec![
            line(0x10, (0.0, 0.0), (10.0, 10.0)),
            line(0x11, (nan, nan), (10.0, 10.0)),
            line(0x12, (0.0, 0.0), (inf, -inf)),
            Entity::Circle(CircleEntity {
                common: common(0x13),
                center: xyz(0.0, 0.0),
                radius: inf,
                extrusion: uncad_model::Point3D {
                    x: 0.0,
                    y: 0.0,
                    z: 1.0,
                },
            }),
            Entity::Text(TextEntity {
                common: common(0x14),
                start_point: uncad_model::Point2D { x: 1.0, y: nan },
                text_height: 2.5,
                text: "LOST".to_string(),
                rotation: 0.0,
                horizontal_justification: Default::default(),
                vertical_justification: Default::default(),
                alignment_point: None,
                width_factor: 1.0,
                oblique_angle: 0.0,
                style_name: uncad_model::Ref::Absent,
                elevation: 0.0,
                extrusion: uncad_model::Point3D {
                    x: 0.0,
                    y: 0.0,
                    z: 1.0,
                },
            }),
            // A block reference whose placement is not a number: nothing
            // drawn under it would land anywhere.
            Entity::Insert(InsertEntity {
                common: common(0x15),
                block_name: Ref::Resolved("B".to_string()),
                insertion_point: xyz(0.0, 0.0),
                scale: Point3D {
                    x: nan,
                    y: inf,
                    z: 1.0,
                },
                rotation: 0.0,
                attribs: Vec::new(),
                extrusion: uncad_model::Point3D {
                    x: 0.0,
                    y: 0.0,
                    z: 1.0,
                },
            }),
        ],
        tables: Tables {
            block_records,
            ..Tables::default()
        },
        read_diagnostics: ReadDiagnostics::default(),
    };

    let result = to_svg(&db, all());

    assert!(!result.svg.contains("NaN"), "{}", result.svg);
    assert!(!result.svg.contains("inf"), "{}", result.svg);
    // Only the one sound LINE is drawn.
    assert_eq!(result.svg.matches("<line ").count(), 1, "{}", result.svg);
    assert!(!result.svg.contains("<circle"));
    assert!(!result.svg.contains("LOST"));
    assert!(!result.svg.contains("<g "));
    // And every one of the other five is counted and named.
    assert_eq!(result.limits.unreadable_entities, 5, "{:?}", result.limits);
    let named: Vec<(u64, &str)> = result
        .limits
        .dropped
        .iter()
        .map(|d| {
            assert_eq!(d.cap, Cap::NotANumber);
            (d.id.value(), d.type_name.as_str())
        })
        .collect();
    assert_eq!(
        named,
        vec![
            (0x11, "LINE"),
            (0x12, "LINE"),
            (0x13, "CIRCLE"),
            (0x14, "TEXT"),
            (0x15, "INSERT")
        ]
    );

    // The PNG path parses that SVG; it must not fail on it.
    let png = to_png(
        &db,
        ToPngOptions {
            svg: all(),
            ..ToPngOptions::default()
        },
    )
    .expect("the sound part rasterizes");
    assert_eq!(png.limits.unreadable_entities, 5);
}

#[test]
fn a_drawing_with_only_real_numbers_reports_nothing() {
    let db = CadDatabase {
        entities: vec![line(0x10, (0.0, 0.0), (10.0, 10.0))],
        tables: Tables::default(),
        read_diagnostics: ReadDiagnostics::default(),
    };
    let result = to_svg(&db, all());
    assert!(!result.limits.engaged(), "{:?}", result.limits);
}

#[test]
fn a_hatch_boundary_bulge_that_is_not_a_number_leaves_the_hatch_out() {
    use uncad_model::model::{HatchBoundaryPath, HatchEntity, Point2D, PolylineVertex};
    let hatch = |id: u64, bulge: f64| {
        let v = |x, y| PolylineVertex::straight(Point2D { x, y });
        Entity::Hatch(HatchEntity {
            common: common(id),
            boundary_paths: vec![HatchBoundaryPath::Polyline(vec![
                v(0.0, 0.0),
                PolylineVertex {
                    bulge,
                    ..v(10.0, 0.0)
                },
                v(10.0, 10.0),
            ])],
            solid_fill: true,
            gradient: None,
            pattern_lines: Vec::new(),
            elevation: 0.0,
            extrusion: uncad_model::model::Point3D {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            },
            style: None,
        })
    };
    let db = CadDatabase {
        entities: vec![hatch(0x10, 0.5), hatch(0x11, f64::NAN)],
        tables: Tables::default(),
        read_diagnostics: ReadDiagnostics::default(),
    };
    let result = to_svg(&db, all());
    assert!(!result.svg.contains("NaN"), "{}", result.svg);
    // The sound one is drawn, outline and fill; the other is named.
    assert_eq!(result.svg.matches("<path ").count(), 1, "{}", result.svg);
    assert_eq!(result.limits.unreadable_entities, 1, "{:?}", result.limits);
    assert_eq!(result.limits.dropped[0].id, EntityId::new(0x11));
    assert_eq!(result.limits.dropped[0].cap, Cap::NotANumber);
}
