//! The page a drawing is drawn on. On a light page (the default) pure white
//! is drawn black; on a dark page pure white stays white, pure black is
//! drawn white, and the document starts with a black rectangle covering its
//! view. Every other color is what the file says on either page.

mod common;

use std::collections::BTreeMap;

use iron_diff_cad::{diff, DiffOptions};
use iron_render_cad::{
    overlay_to_svg, to_svg, OverlayOptions, Paper, Rect, Scene, Space, ToSvgOptions,
};
use uncad_model::model::{Entity, LineEntity, Point3D};
use uncad_model::tables::{LayerRecord, Tables};
use uncad_model::{CadDatabase, ReadDiagnostics, Ref};

fn line(id: u64, layer: &str, color_index: i16, true_color: Option<u32>, y: f64) -> Entity {
    let mut common = common::common_on(id, layer, color_index);
    common.true_color = true_color;
    Entity::Line(LineEntity {
        common,
        start_point: Point3D { x: 0.0, y, z: 0.0 },
        end_point: Point3D { x: 40.0, y, z: 0.0 },
    })
}

/// One line per way a color is stated, each its own height so its element
/// is found by its `y1`:
/// - y = 0: ACI 7 (white/black)
/// - y = 1: true color 0x000000
/// - y = 2: ACI 2 (yellow)
/// - y = 3: BYLAYER, on a layer whose color is ACI 7
/// - y = 4: BYBLOCK at the top level
/// - y = 5: BYLAYER, on a layer the tables do not hold
fn colors() -> CadDatabase {
    let mut layers = BTreeMap::new();
    layers.insert(
        "WHITE".to_string(),
        LayerRecord {
            name: "WHITE".to_string(),
            color_index: 7,
            off: false,
            frozen: false,
            locked: false,
            plot: None,
            lineweight: None,
            linetype: Ref::Absent,
        },
    );
    CadDatabase {
        entities: vec![
            line(0x1, "0", 7, None, 0.0),
            line(0x2, "0", 256, Some(0x000000), 1.0),
            line(0x3, "0", 2, None, 2.0),
            line(0x4, "WHITE", 256, None, 3.0),
            line(0x5, "0", 0, None, 4.0),
            line(0x6, "MISSING", 256, None, 5.0),
        ],
        tables: Tables {
            layers,
            ..Tables::default()
        },
        header: Default::default(),
        read_diagnostics: ReadDiagnostics::default(),
    }
}

fn on(paper: Paper) -> ToSvgOptions {
    ToSvgOptions {
        space: Space::All,
        padding: 1.0,
        paper,
        ..ToSvgOptions::default()
    }
}

/// The stroke of the `<line>` drawn at height `y` (written `-y`, the
/// document's y running down).
fn stroke_at(svg: &str, y: f64) -> String {
    // `+ 0.0` writes -0 as 0, as the renderer does.
    let y1 = format!("y1=\"{}\"", -y + 0.0);
    let element = svg
        .lines()
        .find(|l| l.contains("<line") && l.contains(&y1))
        .unwrap_or_else(|| panic!("no line at y = {y} in\n{svg}"));
    attribute(element, "stroke")
}

fn attribute(element: &str, name: &str) -> String {
    let key = format!(" {name}=\"");
    let start = element
        .find(&key)
        .unwrap_or_else(|| panic!("no {name} in {element}"))
        + key.len();
    let len = element[start..].find('"').unwrap();
    element[start..start + len].to_string()
}

/// The root's viewBox as four numbers.
fn view_box(svg: &str) -> [f64; 4] {
    let root = svg.lines().next().unwrap();
    let values: Vec<f64> = attribute(root, "viewBox")
        .split(' ')
        .map(|v| v.parse().unwrap())
        .collect();
    values.try_into().unwrap()
}

/// The first element after the root's opening tag that draws something:
/// not a `<style>` or a `<defs>` block.
fn first_drawn(svg: &str) -> &str {
    svg.lines()
        .skip(1)
        .map(str::trim)
        .find(|l| !l.starts_with("<style") && !l.starts_with("<defs"))
        .unwrap()
}

/// That `element` is a black rectangle covering exactly `view_box`.
fn assert_page(element: &str, view_box: [f64; 4]) {
    assert!(element.starts_with("<rect"), "{element}");
    let rect: Vec<f64> = ["x", "y", "width", "height"]
        .iter()
        .map(|n| attribute(element, n).parse().unwrap())
        .collect();
    assert_eq!(rect, view_box, "{element}");
    assert_eq!(attribute(element, "fill"), "#000000");
    assert_eq!(attribute(element, "stroke"), "none");
}

#[test]
fn on_a_light_page_white_is_drawn_black_and_nothing_else_changes() {
    let svg = to_svg(&colors(), on(Paper::Light)).svg;
    assert_eq!(stroke_at(&svg, 0.0), "#000000");
    assert_eq!(stroke_at(&svg, 1.0), "#000000");
    assert_eq!(stroke_at(&svg, 2.0), "#ffff00");
    assert_eq!(stroke_at(&svg, 3.0), "#000000");
    assert_eq!(stroke_at(&svg, 4.0), "#000000");
    assert_eq!(stroke_at(&svg, 5.0), "#000000");
    assert!(!svg.contains("<rect"), "a light page has no rectangle");
    assert!(svg.lines().next().unwrap().contains(" stroke=\"black\""));
}

#[test]
fn the_default_page_is_light() {
    assert_eq!(ToSvgOptions::default().paper, Paper::Light);
    assert_eq!(
        to_svg(&colors(), on(Paper::Light)).svg,
        to_svg(
            &colors(),
            ToSvgOptions {
                space: Space::All,
                padding: 1.0,
                ..ToSvgOptions::default()
            }
        )
        .svg
    );
}

#[test]
fn on_a_dark_page_white_stays_white_and_black_is_drawn_white() {
    let svg = to_svg(&colors(), on(Paper::Dark)).svg;
    assert_eq!(stroke_at(&svg, 0.0), "#ffffff", "ACI 7");
    assert_eq!(stroke_at(&svg, 1.0), "#ffffff", "true color black");
    assert_eq!(
        stroke_at(&svg, 2.0),
        "#ffff00",
        "ACI 2 is what the file says"
    );
    assert_eq!(stroke_at(&svg, 3.0), "#ffffff", "a white layer");
    assert_eq!(stroke_at(&svg, 4.0), "#ffffff", "BYBLOCK at the top level");
    assert_eq!(
        stroke_at(&svg, 5.0),
        "#ffffff",
        "an unknown layer's fallback"
    );
    assert!(svg.lines().next().unwrap().contains(" stroke=\"white\""));
}

#[test]
fn a_dark_page_is_the_first_thing_drawn_and_covers_the_view() {
    let svg = to_svg(&colors(), on(Paper::Dark)).svg;
    assert_page(first_drawn(&svg), view_box(&svg));
    assert_eq!(svg.matches("<rect").count(), 1);
}

#[test]
fn a_scenes_window_on_a_dark_page_is_covered_too() {
    let scene = Scene::new(&colors(), on(Paper::Dark));
    let window = Rect::new(10.0, -2.0, 30.0, 8.0);
    let svg = scene.svg(window, 0.1, |_| true);
    assert_page(first_drawn(&svg), view_box(&svg));
    // The window, written relative to the scene's origin, y down.
    let [x, y, w, h] = view_box(&svg);
    assert_eq!(
        [
            x + scene.origin.x,
            scene.origin.y - y - h,
            x + w + scene.origin.x,
            scene.origin.y - y
        ],
        [window.min_x, window.min_y, window.max_x, window.max_y]
    );
}

#[test]
fn a_redline_on_a_dark_page_starts_with_the_page_and_does_not_count_it() {
    let before = colors();
    let mut after = colors();
    let Entity::Line(l) = &mut after.entities[2] else {
        unreachable!()
    };
    l.end_point.x = 30.0;
    let changes = diff(&before, &after, DiffOptions::default());
    let options = OverlayOptions {
        svg: on(Paper::Dark),
        // Black: were the page counted as a color of the original, it would
        // be reported as indistinguishable from the proposal color.
        proposal_color: [0, 0, 0],
        ..OverlayOptions::default()
    };
    let overlay = overlay_to_svg(&before, &after, &changes, options);
    assert!(!overlay.marked.is_empty());
    assert_page(first_drawn(&overlay.svg), view_box(&overlay.svg));
    assert!(
        overlay
            .proposal_color_conflicts
            .iter()
            .all(|c| c.color != "#000000"),
        "{:?}",
        overlay.proposal_color_conflicts
    );
    // The original layer is the dark render.
    let original = &overlay.svg[overlay.svg.find("<g id=\"original\">").unwrap()..];
    assert_eq!(stroke_at(original, 0.0), "#ffffff");
    assert_eq!(stroke_at(original, 2.0), "#ffff00");
}
