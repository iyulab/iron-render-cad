//! A control character in a label must never make the SVG unparseable.
//!
//! XML 1.0 forbids U+0000..U+0008, U+000B, U+000C, U+000E..U+001F, U+FFFE
//! and U+FFFF outright, and every PNG is rendered by parsing the SVG with
//! an XML parser: one stray byte in one label used to fail `to_png` with
//! `InvalidSvg`. The model keeps what the file said; the renderer writes a
//! U+FFFD where such a character was.

use iron_render_cad::{to_png, to_svg, Space, ToPngOptions, ToSvgOptions};
use uncad_model::model::{
    Confidence, Entity, EntityCommon, EntityId, MTextEntity, Origin, Point2D, Point3D, Ref,
    TextEntity,
};
use uncad_model::tables::Tables;
use uncad_model::{CadDatabase, ReadDiagnostics};

fn is_xml_illegal(c: char) -> bool {
    matches!(
        c,
        '\u{0}'..='\u{8}' | '\u{B}' | '\u{C}' | '\u{E}'..='\u{1F}' | '\u{FFFE}' | '\u{FFFF}'
    )
}

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

#[test]
fn a_control_character_in_a_label_reaches_neither_the_svg_nor_the_png() {
    let db = CadDatabase {
        entities: vec![
            Entity::Text(TextEntity {
                common: common(0x80),
                start_point: Point2D { x: 0.0, y: 0.0 },
                text_height: 2.5,
                text: "ZE\u{1}\u{B}RO".to_string(),
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
            Entity::MText(MTextEntity {
                common: common(0x81),
                insertion_point: Point3D {
                    x: 0.0,
                    y: 10.0,
                    z: 0.0,
                },
                text: "A\u{1F}B\u{FFFE}".to_string(),
                text_height: 2.5,
                rotation: 0.0,
                line_spacing_factor: 1.0,
                attachment: None,
                reference_width: 0.0,
                extents_width: None,
                extents_height: None,
                style_name: uncad_model::Ref::Absent,
            }),
        ],
        tables: Tables::default(),
        read_diagnostics: ReadDiagnostics::default(),
    };
    let options = ToSvgOptions {
        space: Space::All,
        ..ToSvgOptions::default()
    };

    let svg = to_svg(&db, options).svg;
    assert!(
        !svg.chars().any(is_xml_illegal),
        "the SVG holds no character XML forbids"
    );
    assert!(svg.contains("ZE\u{FFFD}\u{FFFD}RO"), "{svg}");
    assert!(svg.contains("A\u{FFFD}B\u{FFFD}"), "{svg}");

    let png = to_png(
        &db,
        ToPngOptions {
            svg: options,
            ..ToPngOptions::default()
        },
    )
    .expect("to_png must not fail on a control character");
    assert!(png.png.starts_with(b"\x89PNG"));
}

/// The renderer writes a few values into the document as placeholders it
/// fills in once the whole picture is known (`@@SW@@...@@`, `@@IL@@...@@`).
/// A label that happens to hold the same characters must stay a label: not
/// be taken for a placeholder, and not swallow what follows it.
#[test]
fn a_label_that_looks_like_a_placeholder_is_drawn_as_written() {
    let text = |id, y: f64, s: &str| {
        Entity::Text(TextEntity {
            common: common(id),
            start_point: Point2D { x: 0.0, y },
            text_height: 2.5,
            text: s.to_string(),
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
        })
    };
    let db = CadDatabase {
        entities: vec![
            text(0x90, 0.0, "@@SW@@"),
            text(0x91, 10.0, "5@@IL@@200"),
            text(0x92, 20.0, "AFTER"),
        ],
        tables: Tables::default(),
        read_diagnostics: ReadDiagnostics::default(),
    };
    let options = ToSvgOptions {
        space: Space::All,
        ..ToSvgOptions::default()
    };
    let svg = to_svg(&db, options).svg;
    assert!(
        svg.contains("AFTER"),
        "nothing after the label is lost: {svg}"
    );
    assert_eq!(svg.matches("<text").count(), 3, "{svg}");
    let png = to_png(
        &db,
        ToPngOptions {
            svg: options,
            ..ToPngOptions::default()
        },
    );
    assert!(png.is_ok(), "{:?}", png.err());
}
