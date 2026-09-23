//! A malformed drawing may cost a missing entity and a note saying so; it
//! may never cost the process.
//!
//! Each test builds the shape a corrupt file produces -- a block that
//! references itself, a count or a spacing no real drawing has -- and checks
//! three things: the render returns, what it emits is bounded, and the
//! result says what was left out. The numbers are the caps documented in
//! `iron_render_cad::limits`.

use std::collections::BTreeMap;

use iron_render_cad::limits::Cap;
use iron_render_cad::{to_png, to_svg, Space, ToPngOptions, ToSvgOptions};
use uncad_model::model::{
    Confidence, Entity, EntityCommon, EntityId, HatchBoundaryPath, HatchEntity, HatchPatternLine,
    InsertEntity, LineEntity, LwPolylineEntity, Origin, Point2D, Point3D, Ref,
};
use uncad_model::tables::{BlockRecord, Tables};
use uncad_model::{CadDatabase, ReadDiagnostics};

/// The document's drawing-body budget, `limits::MAX_SVG_BODY_BYTES`.
const DOCUMENT_BYTES: usize = 64 * 1024 * 1024;
/// One top-level entity's share of it, `limits::MAX_ENTITY_SVG_BYTES`.
const ENTITY_BYTES: usize = DOCUMENT_BYTES / 4;
/// How far past a byte budget a finished document may still be: the budget
/// is checked before each entity, so the last one drawn, the `<g>` wrappers
/// closing above it and the `<svg>` element itself all land on top of it.
const SLACK: usize = DOCUMENT_BYTES / 2;

fn common(id: u64) -> EntityCommon {
    EntityCommon {
        id: EntityId::new(id),
        origin: Origin::Vector,
        confidence: Confidence::High,
        source_handle: Ref::Resolved(format!("{id:X}")),
        layer: Ref::Resolved("0".to_string()),
        color_index: 256,
        true_color: None,
        invisible: false,
    }
}

fn xyz(x: f64, y: f64) -> Point3D {
    Point3D { x, y, z: 0.0 }
}

fn xy(x: f64, y: f64) -> Point2D {
    Point2D { x, y }
}

fn line(id: u64, x: f64, y: f64) -> Entity {
    Entity::Line(LineEntity {
        common: common(id),
        start_point: xyz(x, y),
        end_point: xyz(x + 1.0, y + 1.0),
    })
}

fn insert(id: u64, block: &str, x: f64) -> Entity {
    Entity::Insert(InsertEntity {
        common: common(id),
        block_name: Ref::Resolved(block.to_string()),
        insertion_point: xyz(x, 0.0),
        scale: Point3D {
            x: 1.0,
            y: 1.0,
            z: 1.0,
        },
        rotation: 0.0,
        attribs: Vec::new(),
    })
}

fn block(name: &str, entities: Vec<Entity>) -> (String, BlockRecord) {
    (
        name.to_string(),
        BlockRecord {
            name: name.to_string(),
            entities,
        },
    )
}

fn db(entities: Vec<Entity>, blocks: Vec<(String, BlockRecord)>) -> CadDatabase {
    let block_records: BTreeMap<String, BlockRecord> = blocks.into_iter().collect();
    CadDatabase {
        entities,
        tables: Tables {
            block_records,
            ..Tables::default()
        },
        read_diagnostics: ReadDiagnostics::default(),
    }
}

/// Every entity of the model, whatever space it is in: these synthetic
/// models have no `*Model_Space` record to select by.
fn all() -> ToSvgOptions {
    ToSvgOptions {
        space: Space::All,
        ..ToSvgOptions::default()
    }
}

#[test]
fn a_block_that_references_itself_is_cut_short_and_named() {
    // A block that draws forty lines and references itself eight times:
    // every level of the walk adds to the document. Before the caps, the
    // same shape with one line expanded a million references into a
    // 139 MB SVG. Rendering stops at the 16 MiB one entity may emit; what it
    // drew by then is kept, and the part is reported as truncated.
    let mut children: Vec<Entity> = (0..40).map(|i| line(0x100 + i, i as f64, 0.0)).collect();
    children.extend((0..8).map(|i| insert(0x200 + i, "R", i as f64)));
    let drawing = db(
        vec![insert(0x10, "R", 0.0), line(0x11, 0.0, 0.0)],
        vec![block("R", children)],
    );

    let result = to_svg(&drawing, all());

    assert_eq!(result.limits.truncated_parts, 1, "{:?}", result.limits);
    assert!(
        result
            .limits
            .dropped
            .iter()
            .any(|d| d.id == EntityId::new(0x10) && d.cap == Cap::EntityBytes),
        "the truncated INSERT is named: {:?}",
        result.limits.dropped
    );
    assert!(
        result.svg.len() < ENTITY_BYTES + SLACK,
        "the document grew to {} bytes",
        result.svg.len()
    );
    assert!(
        result.svg.ends_with("</svg>"),
        "the cut is at an entity boundary, so the document stays well-formed"
    );
}

#[test]
fn a_drawing_that_is_one_insert_of_one_big_block_renders_whole() {
    // A bound reference or an imported survey is a single top-level INSERT
    // of one large block. 90,000 lines is ~5.8 MB of body, under what one
    // entity may emit: every line is drawn and nothing is reported.
    const LINES: usize = 90_000;
    let children: Vec<Entity> = (0..LINES)
        .map(|i| {
            line(
                0x1000 + i as u64,
                (i % 300) as f64 * 2.0,
                (i / 300) as f64 * 2.0,
            )
        })
        .collect();
    let drawing = db(
        vec![insert(0x10, "PLAN", 0.0)],
        vec![block("PLAN", children)],
    );

    let result = to_svg(&drawing, all());

    assert_eq!(result.svg.matches("<line ").count(), LINES);
    assert!(!result.limits.engaged(), "{:?}", result.limits);
}

#[test]
fn many_ordinary_entities_still_stop_at_the_documents_budget() {
    // Nothing here is individually oversized -- a block of 4,000 lines is a
    // part of ~220 KB -- but 400 references to it come to ~88 MB, past the
    // document's 64 MiB.
    let children: Vec<Entity> = (0..4_000)
        .map(|i| line(0x1000 + i, i as f64, 0.0))
        .collect();
    let tops: Vec<Entity> = (0..400).map(|i| insert(0x10 + i, "B", i as f64)).collect();
    let drawing = db(tops, vec![block("B", children)]);

    let result = to_svg(&drawing, all());

    assert_eq!(result.limits.truncated_parts, 0, "{:?}", result.limits);
    assert!(result.limits.entities_dropped > 0, "{:?}", result.limits);
    assert!(
        result
            .limits
            .dropped
            .iter()
            .any(|d| d.cap == Cap::DocumentBytes),
        "{:?}",
        result.limits.dropped
    );
    assert!(
        result.svg.len() < DOCUMENT_BYTES + SLACK,
        "the document grew to {} bytes",
        result.svg.len()
    );
}

#[test]
fn a_chain_of_blocks_deeper_than_the_cap_stops_at_the_cap() {
    // B0 references B1 references B2 ..., each level drawing one line, so
    // the number of lines drawn is the number of levels followed. Nothing
    // fans out, so only the depth cap can stop it: the top-level reference
    // and the 20 nested inside it are followed, the 21st nested one is not.
    let depth = 60;
    let blocks: Vec<(String, BlockRecord)> = (0..depth)
        .map(|i| {
            let mut entities = vec![line(0x1000 + i, i as f64, 0.0)];
            if i + 1 < depth {
                entities.push(insert(0x2000 + i, &format!("B{}", i + 1), 0.0));
            }
            block(&format!("B{i}"), entities)
        })
        .collect();
    let drawing = db(vec![insert(0x10, "B0", 0.0)], blocks);

    let result = to_svg(&drawing, all());

    assert_eq!(result.svg.matches("<line ").count(), 21);
    assert_eq!(result.limits.block_refs_dropped, 1, "{:?}", result.limits);
    assert_eq!(result.limits.dropped.len(), 1);
    assert_eq!(result.limits.dropped[0].cap, Cap::BlockRefs);
    assert_eq!(result.limits.dropped[0].type_name, "INSERT");
}

#[test]
fn a_block_fanning_out_below_the_depth_cap_stops_at_the_expansion_budget() {
    // Twelve self-references per level is 12^20 expansions without ever
    // passing the depth cap. The block draws nothing, so no byte budget can
    // be what stops it: only the expansion budget can.
    let children: Vec<Entity> = (0..12).map(|i| insert(0x100 + i, "F", i as f64)).collect();
    let drawing = db(vec![insert(0x10, "F", 0.0)], vec![block("F", children)]);

    let result = to_svg(&drawing, all());

    assert!(result.limits.block_refs_dropped > 0, "{:?}", result.limits);
    assert_eq!(result.limits.entities_dropped, 0, "{:?}", result.limits);
    assert_eq!(result.limits.truncated_parts, 0, "{:?}", result.limits);
}

#[test]
fn a_polyline_with_more_vertices_than_the_cap_is_left_out_whole() {
    let vertices: Vec<Point2D> = (0..=100_000)
        .map(|i| xy(i as f64 * 0.001, (i % 7) as f64))
        .collect();
    let huge = Entity::LwPolyline(LwPolylineEntity {
        common: common(0x20),
        vertices,
        closed: false,
    });
    let drawing = db(vec![huge, line(0x21, 0.0, 0.0)], Vec::new());

    let result = to_svg(&drawing, all());

    assert_eq!(result.limits.oversized_entities, 1, "{:?}", result.limits);
    assert_eq!(result.limits.dropped[0].id, EntityId::new(0x20));
    assert_eq!(result.limits.dropped[0].cap, Cap::EntityPoints);
    assert!(!result.svg.contains("<polyline"));
    assert!(result.svg.contains("<line "), "the rest is still drawn");

    // The same polyline one vertex under the cap is drawn.
    let vertices: Vec<Point2D> = (0..100_000).map(|i| xy(i as f64, 0.0)).collect();
    let fits = Entity::LwPolyline(LwPolylineEntity {
        common: common(0x20),
        vertices,
        closed: false,
    });
    let result = to_svg(&db(vec![fits], Vec::new()), all());
    assert!(result.svg.contains("<polyline"));
    assert!(!result.limits.engaged(), "{:?}", result.limits);
}

fn hatch(spacing: f64) -> Entity {
    let square = vec![xy(0.0, 0.0), xy(10.0, 0.0), xy(10.0, 10.0), xy(0.0, 10.0)];
    Entity::Hatch(HatchEntity {
        common: common(0x30),
        boundary_paths: vec![HatchBoundaryPath::Polyline(square)],
        solid_fill: false,
        gradient: None,
        pattern_lines: vec![HatchPatternLine {
            angle: 0.0,
            base_point: xy(0.0, 0.0),
            offset: xy(0.0, spacing),
            dash_pattern: Vec::new(),
        }],
    })
}

#[test]
fn a_hatch_whose_pattern_tile_dwarfs_the_shape_keeps_only_its_outline() {
    // A ten-unit square with a pattern spacing of 1e12: the tile is the
    // rasterizer's pixmap, at the filled element's device scale, so taken at
    // face value this asks for a pixmap 1e11 pixels on a side.
    let sane = to_svg(&db(vec![hatch(0.5)], Vec::new()), all());
    assert!(sane.svg.contains("<pattern"), "a normal hatch still tiles");
    assert!(!sane.limits.engaged(), "{:?}", sane.limits);

    let drawing = db(vec![hatch(1e12)], Vec::new());
    let result = to_svg(&drawing, all());
    assert_eq!(
        result.limits.hatch_patterns_dropped, 1,
        "{:?}",
        result.limits
    );
    assert_eq!(result.limits.dropped[0].cap, Cap::HatchTile);
    assert!(!result.svg.contains("<pattern"));
    assert!(result.svg.contains("<path "), "the outline is still drawn");

    // And what reaches the rasterizer is now cheap to draw.
    let png = to_png(
        &drawing,
        ToPngOptions {
            svg: all(),
            ..ToPngOptions::default()
        },
    )
    .expect("the outline rasterizes");
    assert_eq!(png.limits.hatch_patterns_dropped, 1);
}
