//! Pen colors on a light page: a color whose contrast against white is
//! below 3:1 is drawn darker, its hue kept, so that it stays legible; the
//! colors that already pass are drawn as the file states them, and a dark
//! page is not touched. The contrast is measured on the drawn color values
//! (WCAG 2 relative luminance), not judged from a picture.

mod common;

use std::collections::BTreeMap;

use iron_render_cad::{to_png, to_svg, Paper, PngSize, Space, ToPngOptions, ToSvgOptions};
use uncad_model::model::{
    Entity, HatchBoundaryPath, HatchEntity, InsertEntity, LineEntity, Point2D, Point3D,
    PolylineVertex, Ref,
};
use uncad_model::tables::Tables;
use uncad_model::{CadDatabase, ReadDiagnostics};

/// WCAG 2's threshold for graphical objects.
const MIN_CONTRAST: f64 = 3.0;

fn xyz(x: f64, y: f64) -> Point3D {
    Point3D { x, y, z: 0.0 }
}

fn line_at(id: u64, color_index: i16, true_color: Option<u32>, y: f64) -> Entity {
    let mut common = common::common_on(id, "0", color_index);
    common.true_color = true_color;
    Entity::Line(LineEntity {
        common,
        start_point: xyz(0.0, y),
        end_point: xyz(40.0, y),
    })
}

fn drawing(entities: Vec<Entity>, tables: Tables) -> CadDatabase {
    CadDatabase {
        entities,
        tables,
        header: Default::default(),
        read_diagnostics: ReadDiagnostics::default(),
    }
}

fn options(paper: Paper) -> ToSvgOptions {
    ToSvgOptions {
        space: Space::All,
        padding: 1.0,
        paper,
        ..ToSvgOptions::default()
    }
}

/// The `stroke` of the `<line>` drawn at height `y`.
fn stroke_at(svg: &str, y: f64) -> String {
    let y1 = format!("y1=\"{}\"", -y + 0.0);
    let element = svg
        .lines()
        .find(|l| l.contains("<line") && l.contains(&y1))
        .unwrap_or_else(|| panic!("no line at y = {y} in\n{svg}"));
    let start = element.find(" stroke=\"").unwrap() + " stroke=\"".len();
    element[start..start + element[start..].find('"').unwrap()].to_string()
}

fn channels(hex: &str) -> [u8; 3] {
    let packed = u32::from_str_radix(hex.strip_prefix('#').unwrap(), 16).unwrap();
    [(packed >> 16) as u8, (packed >> 8) as u8, packed as u8]
}

/// WCAG 2 contrast ratio of `hex` against white.
fn contrast(hex: &str) -> f64 {
    let linear = |c: u8| {
        let v = f64::from(c) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let [r, g, b] = channels(hex);
    1.05 / (0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b) + 0.05)
}

/// One line per ACI color of interest, each at its own height.
fn pens() -> CadDatabase {
    let rows: [(i16, Option<u32>); 10] = [
        (2, None),
        (3, None),
        (4, None),
        (9, None),
        (7, None),
        (1, None),
        (5, None),
        (6, None),
        (8, None),
        (256, Some(0xffff7f)),
    ];
    let entities = rows
        .iter()
        .enumerate()
        .map(|(i, (aci, true_color))| line_at(i as u64 + 1, *aci, *true_color, i as f64))
        .collect();
    drawing(entities, Tables::default())
}

#[test]
fn on_a_light_page_every_pen_is_drawn_at_three_to_one_or_better() {
    let result = to_svg(&pens(), options(Paper::Light));
    let expected = [
        "#9a9a00", "#00ad00", "#00a5a5", "#949494", "#000000", "#ff0000", "#0000ff", "#ff00ff",
        "#808080",
    ];
    for (i, want) in expected.iter().enumerate() {
        assert_eq!(stroke_at(&result.svg, i as f64), *want, "row {i}");
    }
    // The true color row: pale yellow, darkened.
    let pale = stroke_at(&result.svg, 9.0);
    assert!(contrast("#ffff7f") < MIN_CONTRAST);
    assert!(contrast(&pale) >= MIN_CONTRAST, "{pale}");
    for i in 0..10 {
        let drawn = stroke_at(&result.svg, f64::from(i));
        assert!(contrast(&drawn) >= MIN_CONTRAST, "row {i}: {drawn}");
    }
    // ACI 2, 3, 4, 9 and the true color row.
    assert_eq!(result.darkened, 5);
}

#[test]
fn the_colors_that_pass_are_what_the_file_states() {
    let svg = to_svg(&pens(), options(Paper::Light)).svg;
    for (row, want) in [
        (5, "#ff0000"),
        (6, "#0000ff"),
        (7, "#ff00ff"),
        (8, "#808080"),
    ] {
        assert_eq!(stroke_at(&svg, f64::from(row)), want);
        assert!(contrast(want) >= MIN_CONTRAST);
    }
    assert_eq!(stroke_at(&svg, 4.0), "#000000", "white is drawn black");
}

#[test]
fn a_dark_page_draws_the_pens_as_the_file_states_them() {
    let result = to_svg(&pens(), options(Paper::Dark));
    for (row, want) in [
        (0, "#ffff00"),
        (1, "#00ff00"),
        (2, "#00ffff"),
        (3, "#c0c0c0"),
        (4, "#ffffff"),
        (9, "#ffff7f"),
    ] {
        assert_eq!(stroke_at(&result.svg, f64::from(row)), want);
    }
    assert_eq!(result.darkened, 0);
}

#[test]
fn drawing_twice_gives_the_same_bytes() {
    let a = to_svg(&pens(), options(Paper::Light)).svg;
    let b = to_svg(&pens(), options(Paper::Light)).svg;
    assert_eq!(a, b);
}

#[test]
fn a_block_reference_and_what_it_passes_down_are_darkened_once() {
    let insert = Entity::Insert(InsertEntity {
        common: common::common_on(0x10, "0", 2),
        block_name: Ref::Resolved("SYMBOL".to_string()),
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
        array: None,
    });
    // A BYBLOCK line inside the block takes the reference's drawn color.
    let mut block_records = BTreeMap::new();
    block_records.insert(
        "SYMBOL".to_string(),
        common::block_record("SYMBOL", vec![line_at(0x11, 0, None, 1.0)]),
    );
    let tables = Tables {
        block_records,
        ..Tables::default()
    };
    let result = to_svg(&drawing(vec![insert], tables), options(Paper::Light));
    assert_eq!(stroke_at(&result.svg, 1.0), "#9a9a00");
    assert_eq!(result.darkened, 1, "the reference, not what inherits");
}

#[test]
fn a_solid_fill_takes_the_same_color_as_a_line() {
    let square = |x: f64, y: f64| Point2D { x, y };
    let hatch = Entity::Hatch(HatchEntity {
        common: common::common_on(0x20, "0", 2),
        boundary_paths: vec![HatchBoundaryPath::Polyline(
            [
                square(0.0, 0.0),
                square(10.0, 0.0),
                square(10.0, 10.0),
                square(0.0, 10.0),
            ]
            .map(PolylineVertex::straight)
            .to_vec(),
        )],
        solid_fill: true,
        gradient: None,
        pattern_lines: Vec::new(),
        elevation: 0.0,
        extrusion: Point3D {
            x: 0.0,
            y: 0.0,
            z: 1.0,
        },
        style: None,
    });
    let svg = to_svg(
        &drawing(vec![hatch], Tables::default()),
        options(Paper::Light),
    )
    .svg;
    assert!(svg.contains("#9a9a00"), "{svg}");
    assert!(!svg.contains("#ffff00"), "{svg}");
}

#[test]
fn the_picture_is_drawn_in_the_corrected_color_too() {
    // The picture of a drawing in ACI 2 is, byte for byte, the picture of
    // the same drawing in the color ACI 2 is corrected to: the PNG is drawn
    // from the same colors as the SVG, so no pixel need be read.
    let drawing_in = |aci: i16, true_color: Option<u32>| {
        drawing(
            vec![
                line_at(1, aci, true_color, 5.0),
                line_at(2, 7, None, 0.0),
                line_at(3, 7, None, 10.0),
            ],
            Tables::default(),
        )
    };
    let picture = |db: &CadDatabase, paper: Paper| {
        to_png(
            db,
            ToPngOptions {
                svg: options(paper),
                size: PngSize::Scale(10.0),
                stroke_px: Some(6.0),
                ..ToPngOptions::default()
            },
        )
        .expect("renders")
    };
    let faint = picture(&drawing_in(2, None), Paper::Light);
    let corrected = picture(&drawing_in(256, Some(0x9a9a00)), Paper::Light);
    assert_eq!(faint.png, corrected.png);
    assert_eq!(faint.darkened, 1);
    assert_eq!(corrected.darkened, 0);
    // The file's yellow is not what was drawn, and a dark page draws it.
    let literal = picture(&drawing_in(256, Some(0xffff00)), Paper::Dark);
    assert_ne!(faint.png, literal.png);
    assert_eq!(literal.darkened, 0);
}
