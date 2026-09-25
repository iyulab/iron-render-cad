//! A block reference that draws nothing is reported, and one that draws is
//! not. The test pairs the two so a check that only looked for the signal
//! could not tell "reported when it should be" from "reported always".

use std::collections::BTreeMap;

use iron_render_cad::{to_svg, Space, ToSvgOptions};
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
        color_index: 256,
        true_color: None,
        invisible: false,
        linetype: uncad_model::model::EntityLinetype::ByLayer,
        linetype_scale: 1.0,
        lineweight: Some(-1),
        transparency: Some(0),
    }
}

fn p3(x: f64, y: f64, z: f64) -> Point3D {
    Point3D { x, y, z }
}

fn insert(id: u64, block_name: &str) -> Entity {
    Entity::Insert(InsertEntity {
        common: common(id),
        block_name: Ref::Resolved(block_name.to_string()),
        insertion_point: p3(0.0, 0.0, 0.0),
        scale: p3(1.0, 1.0, 1.0),
        rotation: 0.0,
        attribs: Vec::new(),
        extrusion: Point3D {
            x: 0.0,
            y: 0.0,
            z: 1.0,
        },
    })
}

fn block(name: &str, entities: Vec<Entity>) -> (String, BlockRecord) {
    (
        name.to_string(),
        BlockRecord {
            base_point: Default::default(),
            name: name.to_string(),
            entities,
        },
    )
}

#[test]
fn a_block_reference_that_draws_nothing_is_named_and_one_that_draws_is_not() {
    let line = Entity::Line(LineEntity {
        common: common(0x10),
        start_point: p3(0.0, 0.0, 0.0),
        end_point: p3(10.0, 0.0, 0.0),
    });
    let mut block_records = BTreeMap::new();
    block_records.extend([
        block("EMPTY", Vec::new()),
        block("FULL", vec![line.clone()]),
    ]);
    let db = CadDatabase {
        entities: vec![
            insert(0x20, "EMPTY"),
            insert(0x21, "EMPTY"), // referenced twice: reported once
            insert(0x22, "FULL"),
            line,
        ],
        tables: Tables {
            block_records,
            ..Tables::default()
        },
        read_diagnostics: ReadDiagnostics::default(),
    };

    // Space::All: this synthetic model has no *Model_Space record to select by.
    let result = to_svg(
        &db,
        ToSvgOptions {
            space: Space::All,
            ..ToSvgOptions::default()
        },
    );
    assert_eq!(result.empty_blocks, vec!["EMPTY".to_string()]);
    assert!(
        result.svg.contains("<line") || result.svg.contains("<path"),
        "the FULL block must still draw: {}",
        result.svg
    );

    // Control: the same drawing without the empty references reports nothing.
    let db = CadDatabase {
        entities: db.entities[2..].to_vec(),
        ..db
    };
    let result = to_svg(
        &db,
        ToSvgOptions {
            space: Space::All,
            ..ToSvgOptions::default()
        },
    );
    assert!(result.empty_blocks.is_empty(), "{:?}", result.empty_blocks);
}
