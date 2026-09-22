//! G2: block nesting three deep with a rotation and a scale at each level.
//! The renderer composes the transforms; the drawing's world-space bounds
//! -- which is what the viewBox is computed from -- must land where the
//! spec says the one line ends up: from (110, 100) to (110, 120).

use iron_render_cad::{to_svg, Space, ToSvgOptions};
use uncad_model::CadDatabase;

fn g2() -> CadDatabase {
    serde_json::from_str(include_str!("golden/g2.expected.json"))
        .expect("the golden model deserializes")
}

/// The four numbers of the SVG's `viewBox` attribute.
fn view_box(svg: &str) -> [f64; 4] {
    let start = svg.find("viewBox=\"").expect("a viewBox") + "viewBox=\"".len();
    let end = svg[start..].find('"').expect("closed attribute") + start;
    let parts: Vec<f64> = svg[start..end]
        .split_whitespace()
        .map(|n| n.parse().expect("a number"))
        .collect();
    parts.try_into().expect("four numbers")
}

#[test]
fn three_nested_transforms_compose_to_the_declared_world_position() {
    let db = g2();
    let padding = 5.0;
    let result = to_svg(
        &db,
        ToSvgOptions {
            padding,
            space: Space::All,
            ..ToSvgOptions::default()
        },
    );
    assert!(
        result.unsupported_types.is_empty(),
        "{:?}",
        result.unsupported_types
    );
    assert!(result.empty_blocks.is_empty(), "{:?}", result.empty_blocks);

    // One line, three levels down: three nested groups around it.
    assert_eq!(result.svg.matches("<line ").count(), 1, "{}", result.svg);
    assert_eq!(result.svg.matches("<g transform=\"matrix(").count(), 3);

    // World bounds: x from 110 to 110, y from 100 to 120 (SVG's y is
    // flipped, so the box runs from -120 to -100), plus the padding.
    let [x, y, w, h] = view_box(&result.svg);
    let close = |a: f64, b: f64| (a - b).abs() < 1e-6;
    assert!(close(x, 110.0 - padding), "viewBox x {x}");
    assert!(close(y, -120.0 - padding), "viewBox y {y}");
    assert!(close(w, 2.0 * padding), "viewBox width {w}");
    assert!(close(h, 20.0 + 2.0 * padding), "viewBox height {h}");
}
