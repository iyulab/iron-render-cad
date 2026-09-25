//! A block reference nested inside another block draws its attribute
//! values, and draws each of them once.
//!
//! A top-level INSERT's ATTRIBs are also top-level entities of the model,
//! so they are drawn like any other. A nested INSERT's are not: they hang
//! off the INSERT's own `attribs`, and a file written as DXF may also list
//! them among the enclosing block's entities. Both shapes occur; either way
//! the value is drawn exactly once.

use std::collections::BTreeMap;

use iron_render_cad::{to_svg, Space, ToSvgOptions};
use uncad_model::model::{
    AttribEntity, CircleEntity, Confidence, Entity, EntityCommon, EntityId, InsertEntity, Origin,
    Point2D, Point3D, Ref,
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

fn insert(id: u64, block: &str, at: (f64, f64), attribs: Vec<AttribEntity>) -> Entity {
    Entity::Insert(InsertEntity {
        common: common(id),
        block_name: Ref::Resolved(block.to_string()),
        insertion_point: xyz(at.0, at.1),
        scale: Point3D {
            x: 1.0,
            y: 1.0,
            z: 1.0,
        },
        rotation: 0.0,
        attribs,
        extrusion: uncad_model::Point3D {
            x: 0.0,
            y: 0.0,
            z: 1.0,
        },
    })
}

fn tag_value() -> AttribEntity {
    AttribEntity {
        common: common(0x60),
        start_point: Point2D { x: 3.0, y: 4.0 },
        text_height: 2.5,
        tag: "TAG".to_string(),
        text: "D-101".to_string(),
        rotation: 0.0,
        flags: Default::default(),
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
    }
}

/// A title block (`TITLE`) holding a tag symbol (`TAG`, a circle) placed
/// with an attribute value, the title block placed once at the top level.
/// `dxf_shape` also lists the ATTRIB among the title block's own entities.
fn drawing(dxf_shape: bool) -> CadDatabase {
    let mut title = vec![insert(0x50, "TAG", (3.0, 3.0), vec![tag_value()])];
    if dxf_shape {
        title.push(Entity::Attrib(tag_value()));
    }
    let mut block_records = BTreeMap::new();
    block_records.insert(
        "TITLE".to_string(),
        BlockRecord {
            base_point: Default::default(),
            name: "TITLE".to_string(),
            entities: title,
        },
    );
    block_records.insert(
        "TAG".to_string(),
        BlockRecord {
            base_point: Default::default(),
            name: "TAG".to_string(),
            entities: vec![Entity::Circle(CircleEntity {
                common: common(0x70),
                center: xyz(0.0, 0.0),
                radius: 1.0,
                extrusion: uncad_model::Point3D {
                    x: 0.0,
                    y: 0.0,
                    z: 1.0,
                },
            })],
        },
    );
    CadDatabase {
        entities: vec![insert(0x10, "TITLE", (100.0, 0.0), Vec::new())],
        tables: Tables {
            block_records,
            ..Tables::default()
        },
        read_diagnostics: ReadDiagnostics::default(),
    }
}

fn render(db: &CadDatabase) -> String {
    to_svg(
        db,
        ToSvgOptions {
            space: Space::All,
            ..ToSvgOptions::default()
        },
    )
    .svg
}

#[test]
fn a_nested_references_attribute_value_is_drawn_once() {
    for dxf_shape in [false, true] {
        let svg = render(&drawing(dxf_shape));
        assert_eq!(
            svg.matches(">D-101</text>").count(),
            1,
            "dxf_shape {dxf_shape}: {svg}"
        );
        // In the title block's own coordinates, where the tag's INSERT
        // itself is: (3, 4), y flipped.
        assert!(svg.contains("x=\"3\" y=\"-4\""), "{svg}");
        // And the symbol it belongs to is drawn too.
        assert_eq!(svg.matches("<circle").count(), 1);
    }
}
