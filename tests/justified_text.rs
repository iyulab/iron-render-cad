//! A TEXT or ATTRIB is placed the way its file justifies it: by its
//! alignment point unless it is left/baseline, anchored at the start, the
//! middle or the end of its baseline, hung from its baseline, its middle,
//! the top of its capitals or the bottom of its descenders; its characters
//! stretched by their width factor and slanted by their oblique angle; and
//! measured by the box it fills, not by its anchor point alone.
//!
//! The expected numbers are worked out from the justification rules and
//! the renderer's two stated assumptions -- capitals 0.7 of the em (the
//! default `cap_height`), descenders 0.2 of it, 0.6 of it per character --
//! not read off this crate's output.

use std::collections::BTreeMap;
use std::f64::consts::{FRAC_PI_2, FRAC_PI_4};

use iron_render_cad::{to_svg, Crop, Space, ToSvgOptions};
use uncad_model::model::{
    Confidence, Entity, EntityCommon, EntityId, HorizontalJustification, Origin, Point2D, Point3D,
    Ref, TextEntity, VerticalJustification,
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
        linetype: uncad_model::model::EntityLinetype::ByLayer,
        linetype_scale: 1.0,
        lineweight: Some(-1),
        transparency: Some(0),
    }
}

struct Text {
    at: (f64, f64),
    align: Option<(f64, f64)>,
    h: HorizontalJustification,
    v: VerticalJustification,
    rotation: f64,
    width_factor: f64,
    oblique: f64,
}

impl Default for Text {
    fn default() -> Self {
        Text {
            at: (10.0, 20.0),
            align: None,
            h: HorizontalJustification::Left,
            v: VerticalJustification::Baseline,
            rotation: 0.0,
            width_factor: 1.0,
            oblique: 0.0,
        }
    }
}

impl Text {
    fn entity(&self) -> Entity {
        let p = |(x, y): (f64, f64)| Point2D { x, y };
        Entity::Text(TextEntity {
            common: common(0x10),
            start_point: p(self.at),
            text_height: 2.0,
            text: "ABCD".to_string(),
            rotation: self.rotation,
            horizontal_justification: self.h,
            vertical_justification: self.v,
            alignment_point: self.align.map(p),
            width_factor: self.width_factor,
            oblique_angle: self.oblique,
            style_name: Ref::Absent,
            elevation: 0.0,
            extrusion: Point3D {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            },
        })
    }
}

fn render(db: &CadDatabase) -> String {
    to_svg(
        db,
        ToSvgOptions {
            space: Space::All,
            padding: 0.0,
            crop: Crop::Everything,
            ..ToSvgOptions::default()
        },
    )
    .svg
}

fn render_one(entity: Entity) -> String {
    render(&CadDatabase {
        entities: vec![entity],
        tables: Tables::default(),
        read_diagnostics: ReadDiagnostics::default(),
    })
}

/// The extent the viewBox shows, as `[min_x, min_y, max_x, max_y]`.
fn extent(svg: &str) -> [f64; 4] {
    let start = svg.find("viewBox=\"").unwrap() + "viewBox=\"".len();
    let end = svg[start..].find('"').unwrap() + start;
    let v: Vec<f64> = svg[start..end]
        .split_whitespace()
        .map(|n| n.parse().unwrap())
        .collect();
    [v[0], -v[1] - v[3], v[0] + v[2], -v[1]]
}

fn close(actual: [f64; 4], expected: [f64; 4]) {
    for (a, e) in actual.iter().zip(expected) {
        assert!((a - e).abs() < 1e-9, "{actual:?} vs {expected:?}");
    }
}

/// Every `<text>` element's attributes, by the string it shows.
fn texts(svg: &str) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for element in svg.split("<text ").skip(1) {
        let (attrs, rest) = element.split_once('>').unwrap();
        let shown = rest.split("</text>").next().unwrap().to_string();
        let mut map = BTreeMap::new();
        for pair in attrs.split("\" ") {
            let (k, v) = pair.split_once("=\"").unwrap();
            map.insert(k.trim().to_string(), v.trim_end_matches('"').to_string());
        }
        out.insert(shown, map);
    }
    out
}

/// A text 2 high of four characters is 4 x 0.6 em wide at an em of
/// 2 / 0.7: 48 / 7.
const WIDTH: f64 = 4.0 * 0.6 * 2.0 / 0.7;

#[test]
fn a_left_baseline_text_fills_the_box_to_its_right_and_above_its_baseline() {
    close(
        extent(&render_one(Text::default().entity())),
        [10.0, 20.0, 10.0 + WIDTH, 22.0],
    );
}

#[test]
fn a_right_top_text_hangs_below_and_left_of_its_alignment_point() {
    let svg = render_one(
        Text {
            align: Some((30.0, 20.0)),
            h: HorizontalJustification::Right,
            v: VerticalJustification::Top,
            ..Text::default()
        }
        .entity(),
    );
    // Anchored at its end, the baseline one height (2) below the point.
    let t = &texts(&svg)["ABCD"];
    assert_eq!(t["text-anchor"], "end");
    assert_eq!((t["x"].as_str(), t["y"].as_str()), ("30", "-18"));
    close(extent(&svg), [30.0 - WIDTH, 18.0, 30.0, 20.0]);
}

#[test]
fn a_bottom_justified_text_stands_on_its_descenders() {
    let svg = render_one(
        Text {
            align: Some((10.0, 20.0)),
            v: VerticalJustification::Bottom,
            ..Text::default()
        }
        .entity(),
    );
    // The baseline is a 0.2 em descender above the point: 0.2 / 0.7 of the
    // height.
    let descender = 2.0 * 0.2 / 0.7;
    let y: f64 = texts(&svg)["ABCD"]["y"].parse().unwrap();
    assert!((y - (-20.0 - descender)).abs() < 1e-9, "{y}");
    close(
        extent(&svg),
        [10.0, 20.0, 10.0 + WIDTH, 20.0 + descender + 2.0],
    );
}

#[test]
fn a_turned_justified_text_turns_about_its_alignment_point() {
    let svg = render_one(
        Text {
            align: Some((10.0, 20.0)),
            h: HorizontalJustification::Middle,
            rotation: FRAC_PI_2,
            ..Text::default()
        }
        .entity(),
    );
    let t = &texts(&svg)["ABCD"];
    // Written half a height below the point, and turned about the point
    // itself, not about the baseline.
    assert_eq!(t["y"], "-19");
    assert_eq!(t["transform"], "rotate(-90 10 -20)");
    // Turned a quarter, the text runs up through the point and its capitals
    // point left: x from 9 to 11, y half its width either side of 20.
    close(
        extent(&svg),
        [9.0, 20.0 - WIDTH / 2.0, 11.0, 20.0 + WIDTH / 2.0],
    );
}

#[test]
fn a_width_factor_stretches_the_characters_and_the_box() {
    let svg = render_one(
        Text {
            width_factor: 0.5,
            ..Text::default()
        }
        .entity(),
    );
    let t = &texts(&svg)["ABCD"];
    assert_eq!((t["x"].as_str(), t["y"].as_str()), ("0", "0"));
    assert_eq!(t["transform"], "matrix(0.5 0 0 1 10 -20)");
    close(extent(&svg), [10.0, 20.0, 10.0 + WIDTH / 2.0, 22.0]);
}

#[test]
fn an_oblique_angle_slants_the_characters_and_the_box() {
    // 45 degrees: the top of a capital 2 high leans 2 along the baseline.
    let svg = render_one(
        Text {
            oblique: FRAC_PI_4,
            ..Text::default()
        }
        .entity(),
    );
    let t = &texts(&svg)["ABCD"];
    let m: Vec<f64> = t["transform"]
        .trim_start_matches("matrix(")
        .trim_end_matches(')')
        .split(' ')
        .map(|v| v.parse().unwrap())
        .collect();
    // SVG's y points down, so the slant is -tan(45 degrees) there.
    let expected = [1.0, 0.0, -1.0, 1.0, 10.0, -20.0];
    assert!(
        m.iter().zip(expected).all(|(a, e)| (a - e).abs() < 1e-12),
        "{m:?}"
    );
    close(extent(&svg), [10.0, 20.0, 12.0 + WIDTH, 22.0]);
}

#[test]
fn g13_places_each_text_by_its_own_justification() {
    let db: CadDatabase = serde_json::from_str(include_str!("golden/g13.expected.json"))
        .expect("the golden model deserializes");
    let texts = texts(&render(&db));
    let at = |shown: &str| {
        let t = &texts[shown];
        (
            t.get("text-anchor").map_or("start", String::as_str),
            t["x"].as_str(),
            t["y"].as_str(),
            t.get("transform").map_or("", String::as_str),
        )
    };
    // Left/baseline: the start point, as before.
    assert_eq!(at("LEFT"), ("start", "0", "0", ""));
    // Centre: centred on the alignment point (50, 0), not at the start
    // point (43, 0).
    assert_eq!(at("CENTER"), ("middle", "50", "0", ""));
    // Right/top: ending at (100, 0), the baseline one height (2.5) below.
    assert_eq!(at("RIGHT TOP"), ("end", "100", "2.5", ""));
    // Middle: centred both ways on (50, 20), its characters 0.8 wide.
    assert_eq!(
        at("MIDDLE"),
        ("middle", "0", "1.25", "matrix(0.8 0 0 1 50 -20)")
    );
    // Aligned and fit: centred between their two points, (30, 40) and
    // (30, 60); fit states its stretch, aligned its 15-degree slant.
    assert_eq!(at("FIT"), ("middle", "0", "0", "matrix(2.5 0 0 1 30 -60)"));
    let (anchor, _, _, aligned) = at("ALIGNED");
    assert_eq!(anchor, "middle");
    assert!(
        aligned.starts_with("matrix(1 0 -0.2679491924311"),
        "{aligned}"
    );
    assert!(aligned.ends_with(" 1 30 -40)"), "{aligned}");
    // Left/bottom: standing on its descenders at (0, 80).
    let (_, x, y, _) = at("LEFT BOTTOM");
    assert_eq!(x, "0");
    let y: f64 = y.parse().unwrap();
    assert!((y - (-80.0 - 2.5 * 0.2 / 0.7)).abs() < 1e-9, "{y}");
    // The attribute that is shown is drawn.
    assert!(texts.contains_key("D-101"));
}
