//! A TOLERANCE frame whose file states no height is drawn at its dimension
//! style's text height, and at 1 only when the style states none either.
//! (An R2000+ feature control frame stores no height of its own.)

use std::collections::BTreeMap;

use iron_render_cad::{to_svg, Space, ToSvgOptions, DEFAULT_CAP_HEIGHT};
use uncad_model::model::{
    Confidence, Entity, EntityCommon, EntityId, Origin, Point3D, Ref, ToleranceEntity,
};
use uncad_model::tables::{DimStyleRecord, Tables};
use uncad_model::{CadDatabase, ReadDiagnostics};

fn tolerance(text_height: Option<f64>, style: Ref<String>) -> Entity {
    Entity::Tolerance(ToleranceEntity {
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
        insertion_point: Point3D {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        },
        text_height,
        text_value: "{\\Fgdt;j}%%v0.1".to_string(),
        direction: None,
        style_name: style,
    })
}

/// The CAD height the frame is drawn at, read back from its `font-size`.
fn drawn_height(entity: Entity) -> f64 {
    let mut dim_styles = BTreeMap::new();
    dim_styles.insert(
        "ISO-25".to_string(),
        DimStyleRecord {
            name: "ISO-25".to_string(),
            text_height: Some(2.5),
            ..DimStyleRecord::default()
        },
    );
    dim_styles.insert(
        "BARE".to_string(),
        DimStyleRecord {
            name: "BARE".to_string(),
            ..DimStyleRecord::default()
        },
    );
    let db = CadDatabase {
        entities: vec![entity],
        tables: Tables {
            dim_styles,
            ..Tables::default()
        },
        read_diagnostics: ReadDiagnostics::default(),
    };
    let svg = to_svg(
        &db,
        ToSvgOptions {
            space: Space::All,
            ..ToSvgOptions::default()
        },
    )
    .svg;
    let at = svg.find("font-size=\"").unwrap() + "font-size=\"".len();
    let size: f64 = svg[at..at + svg[at..].find('"').unwrap()].parse().unwrap();
    size * DEFAULT_CAP_HEIGHT
}

fn close(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-12, "{a} vs {b}");
}

#[test]
fn a_frame_without_a_height_takes_its_styles() {
    let iso = || Ref::Resolved("ISO-25".to_string());
    // Stated: used as stated.
    close(drawn_height(tolerance(Some(3.5), iso())), 3.5);
    // Not stated: the style's DIMTXT.
    close(drawn_height(tolerance(None, iso())), 2.5);
    // A style that states no height, or no style at all: 1.
    close(
        drawn_height(tolerance(None, Ref::Resolved("BARE".to_string()))),
        1.0,
    );
    close(drawn_height(tolerance(None, Ref::Absent)), 1.0);
    close(
        drawn_height(tolerance(None, Ref::Unresolved("2A".to_string()))),
        1.0,
    );
}
