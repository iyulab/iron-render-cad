//! Geometry on layer 0 inside a block takes the layer of the block
//! reference that places it: the window, door and fixture symbols a CAD
//! office draws on layer 0 take their color from the layer they are
//! inserted on. Every other layer inside a block is used as stated.

use std::collections::BTreeMap;

use iron_render_cad::{to_svg, Space, ToSvgOptions};
use uncad_model::model::{
    Confidence, Entity, EntityCommon, EntityId, InsertEntity, LineEntity, Origin, Point3D, Ref,
};
use uncad_model::tables::{BlockRecord, LayerRecord, Tables};
use uncad_model::{CadDatabase, ReadDiagnostics};

/// A BYLAYER entity on `layer`.
fn common(id: u64, layer: &str) -> EntityCommon {
    EntityCommon {
        id: EntityId::new(id),
        origin: Origin::Vector,
        confidence: Confidence::High,
        source_handle: Ref::Resolved(format!("{id:X}")),
        layer: Ref::Resolved(layer.to_string()),
        color_index: 256,
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

fn line(id: u64, layer: &str, y: f64) -> Entity {
    Entity::Line(LineEntity {
        common: common(id, layer),
        start_point: xyz(0.0, y),
        end_point: xyz(10.0, y),
    })
}

fn insert(id: u64, layer: &str, block: &str) -> Entity {
    Entity::Insert(InsertEntity {
        common: common(id, layer),
        block_name: Ref::Resolved(block.to_string()),
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

/// Layers 0 (ACI 2, yellow), RED (ACI 1) and BLUE (ACI 5); block SYMBOL holds
/// a layer-0 line, a BLUE line and a layer-0 reference to block INNER, which
/// holds one layer-0 line.
fn drawing(entities: Vec<Entity>) -> CadDatabase {
    let mut layers = BTreeMap::new();
    for (name, color_index) in [("0", 2), ("RED", 1), ("BLUE", 5)] {
        layers.insert(
            name.to_string(),
            LayerRecord {
                name: name.to_string(),
                color_index,
                off: false,
                frozen: false,
                locked: false,
                plot: None,
                lineweight: None,
                linetype: uncad_model::Ref::Absent,
            },
        );
    }
    let mut block_records = BTreeMap::new();
    block_records.insert(
        "SYMBOL".to_string(),
        BlockRecord {
            base_point: Default::default(),
            name: "SYMBOL".to_string(),
            entities: vec![
                line(0x50, "0", 1.0),
                line(0x51, "BLUE", 2.0),
                insert(0x52, "0", "INNER"),
            ],
        },
    );
    block_records.insert(
        "INNER".to_string(),
        BlockRecord {
            base_point: Default::default(),
            name: "INNER".to_string(),
            entities: vec![line(0x60, "0", 3.0)],
        },
    );
    CadDatabase {
        entities,
        tables: Tables {
            layers,
            block_records,
            ..Tables::default()
        },
        read_diagnostics: ReadDiagnostics::default(),
    }
}

/// The `stroke` of the `<line>` drawn at `y` (SVG y is flipped).
fn stroke_at(svg: &str, y: f64) -> String {
    let at = svg
        .find(&format!("y1=\"{}\"", -y))
        .unwrap_or_else(|| panic!("a line at y = {y}: {svg}"));
    let rest = &svg[at..];
    let s = rest.find("stroke=\"").unwrap() + "stroke=\"".len();
    rest[s..s + rest[s..].find('"').unwrap()].to_string()
}

#[test]
fn layer_zero_in_a_block_draws_in_the_references_layer() {
    let db = drawing(vec![insert(0x10, "RED", "SYMBOL")]);
    let svg = to_svg(
        &db,
        ToSvgOptions {
            space: Space::All,
            ..ToSvgOptions::default()
        },
    )
    .svg;
    // The layer-0 line takes RED from the reference.
    assert_eq!(stroke_at(&svg, 1.0), "#ff0000");
    // A named layer inside the block is used as stated.
    assert_eq!(stroke_at(&svg, 2.0), "#0000ff");
    // A layer-0 reference inside a layer-0 reference ends at the outermost
    // reference's layer.
    assert_eq!(stroke_at(&svg, 3.0), "#ff0000");
}

#[test]
fn layer_zero_at_the_top_level_is_layer_zero() {
    let db = drawing(vec![line(0x10, "0", 1.0), insert(0x11, "0", "INNER")]);
    let svg = to_svg(
        &db,
        ToSvgOptions {
            space: Space::All,
            ..ToSvgOptions::default()
        },
    )
    .svg;
    // Layer 0's own color, yellow, both at the top level and inside a
    // reference that is itself on layer 0.
    assert_eq!(stroke_at(&svg, 1.0), "#ffff00");
    assert_eq!(stroke_at(&svg, 3.0), "#ffff00");
}
