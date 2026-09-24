//! A block reference that points at a block the model does not hold draws
//! nothing, and says so -- by the ID of the reference, whichever of the
//! three ways it fails: the reference never resolved, the file points at
//! nothing, or the name it resolved to has no definition. A reference whose
//! block is there is not reported, and one whose block is empty is reported
//! as an empty block instead.

use std::collections::BTreeMap;

use iron_render_cad::{to_png, to_svg, Space, ToPngOptions, ToSvgOptions};
use uncad_model::model::{
    Confidence, Entity, EntityCommon, EntityId, InsertEntity, LineEntity, Origin, Point3D, Ref,
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

fn insert(id: u64, block_name: Ref<String>) -> Entity {
    Entity::Insert(InsertEntity {
        common: common(id),
        block_name,
        insertion_point: xyz(0.0, 0.0),
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
    })
}

fn drawing(entities: Vec<Entity>) -> CadDatabase {
    let mut block_records = BTreeMap::new();
    block_records.insert(
        "DOOR".to_string(),
        BlockRecord {
            name: "DOOR".to_string(),
            entities: vec![Entity::Line(LineEntity {
                common: common(0x70),
                start_point: xyz(0.0, 0.0),
                end_point: xyz(1.0, 1.0),
            })],
        },
    );
    block_records.insert(
        "TWICE".to_string(),
        BlockRecord {
            name: "TWICE".to_string(),
            entities: vec![insert(0x71, Ref::Resolved("NOWHERE".to_string()))],
        },
    );
    block_records.insert(
        "EMPTY".to_string(),
        BlockRecord {
            name: "EMPTY".to_string(),
            entities: Vec::new(),
        },
    );
    CadDatabase {
        entities,
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
fn a_reference_to_a_block_the_model_does_not_hold_is_named() {
    let db = drawing(vec![
        insert(0x13, Ref::Resolved("WINDOW".to_string())),
        insert(0x11, Ref::Unresolved("2A".to_string())),
        insert(0x12, Ref::Absent),
        insert(0x14, Ref::Resolved("DOOR".to_string())),
        insert(0x15, Ref::Resolved("EMPTY".to_string())),
        // A block holding a dead reference, placed twice: the reference is
        // met twice and reported once.
        insert(0x16, Ref::Resolved("TWICE".to_string())),
        insert(0x17, Ref::Resolved("TWICE".to_string())),
    ]);

    let result = to_svg(&db, all());

    assert_eq!(
        result.unresolved_block_refs,
        vec![
            EntityId::new(0x11),
            EntityId::new(0x12),
            EntityId::new(0x13),
            EntityId::new(0x71)
        ]
    );
    assert_eq!(result.empty_blocks, vec!["EMPTY".to_string()]);
    assert_eq!(result.svg.matches("<line ").count(), 1, "DOOR is drawn");

    let png = to_png(
        &db,
        ToPngOptions {
            svg: all(),
            ..ToPngOptions::default()
        },
    )
    .expect("renders");
    assert_eq!(png.unresolved_block_refs, result.unresolved_block_refs);
}

#[test]
fn a_drawing_whose_references_all_resolve_reports_none() {
    let db = drawing(vec![insert(0x14, Ref::Resolved("DOOR".to_string()))]);
    let result = to_svg(&db, all());
    assert!(result.unresolved_block_refs.is_empty());
    assert!(result.empty_blocks.is_empty());
}
