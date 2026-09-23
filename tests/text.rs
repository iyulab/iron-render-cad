//! What a TEXT or an MTEXT shows is what its codes mean, not the codes.
//!
//! The model keeps the string the file wrote; the renderer decodes it. The
//! expected strings are what AutoCAD displays for each code.

use iron_render_cad::{to_svg, Space, ToSvgOptions};
use uncad_model::model::{
    Confidence, Entity, EntityCommon, EntityId, MTextAttachment, MTextEntity, Origin, Point2D,
    Point3D, Ref, TextEntity,
};
use uncad_model::tables::Tables;
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

fn text(raw: &str, height: f64) -> Entity {
    Entity::Text(TextEntity {
        common: common(0x10),
        start_point: Point2D { x: 0.0, y: 0.0 },
        text_height: height,
        text: raw.to_string(),
        rotation: 0.0,
    })
}

fn mtext(raw: &str, attachment: Option<MTextAttachment>) -> Entity {
    Entity::MText(MTextEntity {
        common: common(0x11),
        insertion_point: Point3D {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        },
        text: raw.to_string(),
        text_height: 10.0,
        rotation: 0.0,
        line_spacing_factor: 1.0,
        attachment,
    })
}

fn svg(entity: Entity) -> String {
    let db = CadDatabase {
        entities: vec![entity],
        tables: Tables::default(),
        read_diagnostics: ReadDiagnostics::default(),
    };
    to_svg(
        &db,
        ToSvgOptions {
            space: Space::All,
            ..ToSvgOptions::default()
        },
    )
    .svg
}

/// Every `dy="..."` in `svg`, in order.
fn dys(svg: &str) -> Vec<f64> {
    svg.match_indices("dy=\"")
        .map(|(at, _)| {
            let after = &svg[at + 4..];
            after[..after.find('"').unwrap()].parse().unwrap()
        })
        .collect()
}

#[test]
fn a_texts_symbol_codes_are_drawn_as_their_symbols() {
    assert!(svg(text("%%c50", 2.5)).contains(">\u{2205}50</text>"));
    assert!(svg(text("108%%d", 2.5)).contains(">108\u{00B0}</text>"));
    assert!(svg(text("%%p0.5", 2.5)).contains(">\u{00B1}0.5</text>"));
    // In a TEXT a backslash is a backslash.
    assert!(svg(text(r"C:\Temp", 2.5)).contains(r">C:\Temp</text>"));
}

#[test]
fn an_mtext_fraction_keeps_its_number_readable() {
    // Three and a half inches, not thirty-one halves.
    let out = svg(mtext(r#"3{\H0.7x;\S1#2;}""#, None));
    assert!(out.contains(r#">3 1/2"</tspan>"#), "{out}");
}

#[test]
fn an_mtext_blank_line_keeps_its_line_height() {
    // `ALPHA\P\PBRAVO` is three lines, the middle one empty: BRAVO's
    // baseline is two line heights below ALPHA's, exactly where it is when
    // the middle line holds text.
    let blank = svg(mtext(r"ALPHA\P\PBRAVO", Some(MTextAttachment::TopLeft)));
    let filled = svg(mtext(
        r"ALPHA\PXXXXX\PBRAVO",
        Some(MTextAttachment::TopLeft),
    ));
    assert_eq!(blank.matches("<tspan").count(), 2, "{blank}");
    assert_eq!(filled.matches("<tspan").count(), 3, "{filled}");
    let (blank, filled) = (dys(&blank), dys(&filled));
    assert!(filled[1] > 0.0);
    assert!((blank[1] - (filled[1] + filled[2])).abs() < 1e-9);
    // A blank line counts towards the block's height too, so a
    // bottom-attached block puts its first line where the filled one does.
    let blank = svg(mtext(r"ALPHA\P\PBRAVO", Some(MTextAttachment::BottomLeft)));
    let filled = svg(mtext(
        r"ALPHA\PXXXXX\PBRAVO",
        Some(MTextAttachment::BottomLeft),
    ));
    let first_y = |s: &str| -> f64 {
        let at = s.find(" y=\"").unwrap() + 4;
        s[at..at + s[at..].find('"').unwrap()].parse().unwrap()
    };
    assert_eq!(first_y(&blank), first_y(&filled));
    // Text that is nothing but blank lines draws nothing.
    assert!(!svg(mtext(r"\P\P", None)).contains("<text"));
}

#[test]
fn a_text_whose_file_stores_height_zero_is_still_drawn() {
    // A stored 0 means "the style's height", a style the model does not
    // carry; `font-size="0"` would make the text vanish without a trace.
    let out = svg(text("ZERO", 0.0));
    assert!(out.contains(">ZERO</text>"), "{out}");
    assert!(!out.contains("font-size=\"0\""), "{out}");
}
