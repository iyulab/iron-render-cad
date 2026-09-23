//! RAY and XLINE are drawn as far as the picture goes, and no further.
//!
//! They used to be segments 1e6 drawing units long: in a drawing a few
//! thousandths of a unit across that is billions of pixels off the canvas,
//! which tiny-skia's scan converter panics on.

use std::collections::BTreeMap;

use iron_render_cad::{to_png, to_svg, Space, ToPngOptions, ToSvgOptions};
use uncad_model::model::{
    Confidence, Entity, EntityCommon, EntityId, InsertEntity, LineEntity, Origin, Point3D,
    RayEntity, Ref,
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

fn ray(id: u64, xline: bool, at: (f64, f64), dir: (f64, f64)) -> Entity {
    let r = RayEntity {
        common: common(id),
        point: xyz(at.0, at.1),
        vector: xyz(dir.0, dir.1),
    };
    if xline {
        Entity::XLine(r)
    } else {
        Entity::Ray(r)
    }
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

fn all() -> ToSvgOptions {
    ToSvgOptions {
        space: Space::All,
        padding: 0.0,
        stroke_width: Some(0.0),
        ..ToSvgOptions::default()
    }
}

/// A 10 x 10 square, so the picture has an extent to cut to.
fn square() -> Vec<Entity> {
    vec![
        line(0x1, (0.0, 0.0), (10.0, 0.0)),
        line(0x2, (10.0, 0.0), (10.0, 10.0)),
        line(0x3, (10.0, 10.0), (0.0, 10.0)),
        line(0x4, (0.0, 10.0), (0.0, 0.0)),
    ]
}

/// The four coordinates of every dashed `<line>` (the construction lines).
fn dashed_lines(svg: &str) -> Vec<[f64; 4]> {
    svg.split("<line ")
        .skip(1)
        .filter(|el| el.contains("stroke-dasharray=\"4,2\""))
        .map(|el| {
            let attr = |name: &str| -> f64 {
                let at = el.find(&format!("{name}=\"")).unwrap() + name.len() + 2;
                el[at..at + el[at..].find('"').unwrap()].parse().unwrap()
            };
            [attr("x1"), attr("y1"), attr("x2"), attr("y2")]
        })
        .collect()
}

#[test]
fn an_xline_and_a_ray_stop_at_the_edge_of_the_picture() {
    let mut entities = square();
    // Horizontal through the middle, both ways: x from edge to edge.
    entities.push(ray(0x10, true, (5.0, 5.0), (1.0, 0.0)));
    // Upwards from the middle: from its base point to the top edge.
    entities.push(ray(0x11, false, (5.0, 5.0), (0.0, 1.0)));
    let result = to_svg(&db(entities, BTreeMap::new()), all());

    assert!(!result.svg.contains("1000000"), "{}", result.svg);
    let lines = dashed_lines(&result.svg);
    assert_eq!(lines.len(), 2, "{}", result.svg);
    // The window is the viewBox (0,-10)-(10,0) grown by a percent of its
    // diagonal, 0.1414 units, on every side.
    let m = 10.0_f64.hypot(10.0) * 0.01;
    let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
    let [x1, y1, x2, y2] = lines[0];
    assert!(close(x1, -m) && close(x2, 10.0 + m), "{:?}", lines[0]);
    assert!(close(y1, -5.0) && close(y2, -5.0), "{:?}", lines[0]);
    let [x1, y1, x2, y2] = lines[1];
    assert!(close(x1, 5.0) && close(x2, 5.0), "{:?}", lines[1]);
    assert!(close(y1, -5.0) && close(y2, -10.0 - m), "{:?}", lines[1]);
}

#[test]
fn a_ray_that_misses_the_picture_draws_nothing() {
    // A ray based far beside the square and pointing away from it: the
    // outlier trim keeps its base point out of the viewBox, and nothing of
    // the ray is inside the picture.
    let mut entities = square();
    entities.push(ray(0x10, false, (1000.0, 5.0), (1.0, 0.0)));
    let result = to_svg(
        &db(entities.clone(), BTreeMap::new()),
        ToSvgOptions {
            outlier_trim: true,
            ..all()
        },
    );
    assert!(dashed_lines(&result.svg).is_empty(), "{}", result.svg);
    assert!(!result.svg.contains("@@"), "no placeholder is left behind");

    // Without the trim its base point is in the picture, and the ray is
    // drawn from there to the edge.
    let result = to_svg(
        &db(entities, BTreeMap::new()),
        ToSvgOptions {
            outlier_trim: false,
            ..all()
        },
    );
    let lines = dashed_lines(&result.svg);
    assert_eq!(lines.len(), 1, "{}", result.svg);
    let [x1, _, x2, _] = lines[0];
    assert_eq!(x1, 1000.0);
    assert!(x2 > 1000.0 && x2 < 1011.0, "{x2}");
}

#[test]
fn an_xline_inside_a_placed_block_is_cut_in_the_documents_frame() {
    // The block turns its contents a quarter turn and moves them to (5, 5):
    // its local horizontal XLINE through (0, 0) is the world's vertical line
    // x = 5, which the picture cuts at its top and bottom edges.
    let mut blocks = BTreeMap::new();
    blocks.insert(
        "B".to_string(),
        BlockRecord {
            name: "B".to_string(),
            entities: vec![ray(0x50, true, (0.0, 0.0), (1.0, 0.0))],
        },
    );
    let mut entities = square();
    entities.push(Entity::Insert(InsertEntity {
        common: common(0x10),
        block_name: Ref::Resolved("B".to_string()),
        insertion_point: xyz(5.0, 5.0),
        scale: Point3D {
            x: 1.0,
            y: 1.0,
            z: 1.0,
        },
        rotation: std::f64::consts::FRAC_PI_2,
        attribs: Vec::new(),
    }));
    let result = to_svg(&db(entities, blocks), all());

    let lines = dashed_lines(&result.svg);
    assert_eq!(lines.len(), 1, "{}", result.svg);
    // Written in the block's own frame (y flipped): along its x axis. Its
    // group is matrix(0 -1 1 0 5 -5), so local (u, v) is document
    // (5 + v, -5 - u).
    let [x1, y1, x2, y2] = lines[0];
    assert!(y1.abs() < 1e-9 && y2.abs() < 1e-9, "{:?}", lines[0]);
    let m = 10.0_f64.hypot(10.0) * 0.01;
    let ends = [-5.0 - x1, -5.0 - x2];
    assert!(
        (ends[0] - m).abs() < 1e-9 && (ends[1] - (-10.0 - m)).abs() < 1e-9,
        "document y of the ends: {ends:?}"
    );
}

#[test]
fn a_construction_line_in_a_tiny_drawing_rasterizes() {
    // A drawing 0.002 units across drawn 784,000 pixels a unit: the old
    // million-unit segment was 8e11 pixels long here.
    let entities = vec![
        line(0x1, (0.0, 0.0), (0.002, 0.0)),
        line(0x2, (0.0, 0.0), (0.0, 0.002)),
        ray(0x10, true, (0.001, 0.001), (1.0, 0.3)),
    ];
    let png = to_png(
        &db(entities, BTreeMap::new()),
        ToPngOptions {
            svg: ToSvgOptions {
                space: Space::All,
                padding: 0.0,
                ..ToSvgOptions::default()
            },
            scale: 784_000.0,
            ..ToPngOptions::default()
        },
    )
    .expect("the construction line is cut to the picture and rasterizes");
    assert!(png.png.starts_with(b"\x89PNG"));
}
