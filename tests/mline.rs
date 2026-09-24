//! An MLINE is drawn at its own scale: its style's offsets are in the
//! style's units, and the MLINE's scale (DXF 40) puts them in drawing
//! units. The offset lines are what is drawn, so they are what the extent
//! covers.
//!
//! The expected numbers are offset x scale and nothing else, from the DXF
//! definition of group 40.

use std::collections::BTreeMap;

use iron_render_cad::{to_svg, Crop, Space, ToSvgOptions, ToSvgResult};
use uncad_model::model::{
    Confidence, Entity, EntityCommon, EntityId, MLineEntity, MLineVertex, Origin, Point3D, Ref,
};
use uncad_model::tables::Tables;
use uncad_model::{CadDatabase, ReadDiagnostics};

/// A 200-unit wall along the x axis in the STANDARD style (offsets +-0.5)
/// at `scale`.
fn wall(scale: Option<f64>) -> ToSvgResult {
    let vertex = |x: f64| MLineVertex {
        point: Point3D { x, y: 0.0, z: 0.0 },
        miter_direction: Point3D {
            x: 0.0,
            y: 1.0,
            z: 0.0,
        },
    };
    let mline = Entity::MLine(MLineEntity {
        common: EntityCommon {
            id: EntityId::new(0x10),
            origin: Origin::Vector,
            confidence: Confidence::High,
            source_handle: Ref::Resolved("10".to_string()),
            layer: Ref::Resolved("0".to_string()),
            color_index: 7,
            true_color: None,
            invisible: false,
            linetype: uncad_model::model::EntityLinetype::ByLayer,
            linetype_scale: 1.0,
            lineweight: Some(-1),
            transparency: Some(0),
        },
        vertices: vec![vertex(0.0), vertex(200.0)],
        closed: false,
        mlinestyle_name: Ref::Resolved("STANDARD".to_string()),
        scale,
    });
    let db = CadDatabase {
        entities: vec![mline],
        tables: Tables {
            mlinestyles: BTreeMap::from([("STANDARD".to_string(), vec![0.5, -0.5])]),
            ..Tables::default()
        },
        read_diagnostics: ReadDiagnostics::default(),
    };
    to_svg(
        &db,
        ToSvgOptions {
            space: Space::All,
            padding: 0.0,
            crop: Crop::Everything,
            ..ToSvgOptions::default()
        },
    )
}

/// The viewBox's four numbers.
fn view_box(svg: &str) -> Vec<f64> {
    let start = svg.find("viewBox=\"").unwrap() + "viewBox=\"".len();
    let end = svg[start..].find('"').unwrap() + start;
    svg[start..end]
        .split_whitespace()
        .map(|n| n.parse().unwrap())
        .collect()
}

#[test]
fn a_wall_is_as_thick_as_its_scale_says_and_measured_by_its_lines() {
    let result = wall(Some(200.0));
    assert!(
        result.svg.contains("points=\"0,-100 200,-100\""),
        "{}",
        result.svg
    );
    assert!(
        result.svg.contains("points=\"0,100 200,100\""),
        "{}",
        result.svg
    );
    assert_eq!(view_box(&result.svg), [0.0, -100.0, 200.0, 200.0]);
}

#[test]
fn a_model_not_given_the_scale_draws_the_styles_own_offsets() {
    let result = wall(None);
    assert!(
        result.svg.contains("points=\"0,-0.5 200,-0.5\""),
        "{}",
        result.svg
    );
    assert_eq!(view_box(&result.svg), [0.0, -0.5, 200.0, 1.0]);
}

#[test]
fn a_scale_that_is_not_a_number_leaves_the_mline_out_and_says_so() {
    let result = wall(Some(f64::NAN));
    assert!(!result.svg.contains("<polyline"), "{}", result.svg);
    assert_eq!(result.limits.unreadable_entities, 1);
}
