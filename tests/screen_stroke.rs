//! A stroke width in screen pixels: the document carries one style rule that
//! draws every stroke that many pixels wide whatever the zoom, and nothing
//! else about it changes.

mod common;

use iron_render_cad::{svg_to_png, to_svg, Space, ToSvgOptions};
use uncad_model::model::{Entity, LineEntity, Point2D, Point3D, TextEntity};
use uncad_model::{CadDatabase, ReadDiagnostics};

const RULE: &str = "<style>*{vector-effect:non-scaling-stroke;stroke-width:1.5px}</style>";

fn drawing() -> CadDatabase {
    CadDatabase {
        entities: vec![
            Entity::Line(LineEntity {
                common: common::common_on(0x1, "0", 1),
                start_point: Point3D {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                end_point: Point3D {
                    x: 400.0,
                    y: 300.0,
                    z: 0.0,
                },
            }),
            Entity::Text(TextEntity {
                common: common::common_on(0x2, "0", 2),
                start_point: Point2D { x: 10.0, y: 10.0 },
                text_height: 5.0,
                text: "A".to_string(),
                rotation: 0.0,
                horizontal_justification: Default::default(),
                vertical_justification: Default::default(),
                alignment_point: None,
                width_factor: 1.0,
                oblique_angle: 0.0,
                style_name: uncad_model::Ref::Absent,
                elevation: 0.0,
                extrusion: Point3D {
                    x: 0.0,
                    y: 0.0,
                    z: 1.0,
                },
            }),
        ],
        tables: Default::default(),
        header: Default::default(),
        read_diagnostics: ReadDiagnostics::default(),
    }
}

fn with(screen_stroke: Option<f64>) -> String {
    to_svg(
        &drawing(),
        ToSvgOptions {
            screen_stroke,
            space: Space::All,
            ..ToSvgOptions::default()
        },
    )
    .svg
}

#[test]
fn a_screen_stroke_is_one_rule_right_after_the_root_tag() {
    let plain = with(None);
    let screen = with(Some(1.5));

    let root_end = plain.find('>').unwrap() + 1;
    assert_eq!(&screen[..root_end], &plain[..root_end]);
    assert_eq!(
        screen[root_end..].strip_prefix(&format!("\n  {RULE}")),
        Some(&plain[root_end..])
    );
    assert_eq!(screen.matches("<style>").count(), 1);
    // The document still parses and draws.
    svg_to_png(&screen, 1.0).unwrap();
}

#[test]
fn a_text_keeps_drawing_without_a_stroke() {
    let screen = with(Some(1.5));
    let text = screen
        .split("<text")
        .nth(1)
        .unwrap_or_else(|| panic!("the text is drawn: {screen}"));
    assert!(
        text.split('>').next().unwrap().contains("stroke=\"none\""),
        "{screen}"
    );
}

#[test]
fn a_width_that_is_not_a_positive_number_writes_no_rule() {
    let plain = with(None);
    for px in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert_eq!(with(Some(px)), plain, "{px}");
    }
}
