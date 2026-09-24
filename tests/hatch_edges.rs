//! A HATCH boundary's spline edge is drawn along the curve its degree, knots
//! and weights define -- not along its control polygon, and not as though
//! its weights were all 1.
//!
//! The drawing is the golden case G17 (see `golden/README.md`). Its first
//! hatch closes with a rational quadratic from (0, 20) to (0, 0) over the
//! control point (-8, 10), weights 1, 0.5, 1. The expected numbers are
//! worked out from that definition, not read off this crate's output: at
//! the middle of its parameter range the curve is at
//! x = 2 * 0.25 * 0.5 * (-8) / (0.25 + 2 * 0.25 * 0.5 + 0.25) = -8/3, the
//! furthest left it gets. The control polygon would reach -8 there, and the
//! same curve unweighted -4.

use iron_render_cad::{to_svg, Crop, Space, ToSvgOptions};
use uncad_model::CadDatabase;

fn golden() -> CadDatabase {
    serde_json::from_str(include_str!("golden/g17.expected.json"))
        .expect("the golden model deserializes")
}

/// Every `x y` pair of every straight-segment `<path d="...">` in `svg`, y
/// as the drawing has it (the SVG's is negated). A hatch boundary is drawn
/// as chords; a path with an arc command (the bulged outline the hatches
/// were picked from) is left out.
fn path_points(svg: &str) -> Vec<Vec<(f64, f64)>> {
    svg.split("<path d=\"")
        .skip(1)
        .map(|rest| &rest[..rest.find('"').expect("a closed attribute")])
        .filter(|d| !d.contains('A'))
        .map(|d| {
            let numbers: Vec<f64> = d
                .split(|c: char| c.is_ascii_alphabetic() || c.is_whitespace() || c == ',')
                .filter(|s| !s.is_empty())
                .map(|s| s.parse().expect("a number"))
                .collect();
            numbers.chunks(2).map(|p| (p[0], -p[1])).collect()
        })
        .collect()
}

#[test]
fn a_rational_spline_edge_follows_its_weighted_curve() {
    let result = to_svg(
        &golden(),
        ToSvgOptions {
            space: Space::All,
            padding: 0.0,
            crop: Crop::Everything,
            ..ToSvgOptions::default()
        },
    );
    let paths = path_points(&result.svg);
    // The first hatch's boundary is the path that reaches left of x = 0:
    // only its spline edge does.
    let first = paths
        .iter()
        .find(|p| p.iter().any(|&(x, _)| x < 0.0))
        .unwrap_or_else(|| panic!("a boundary left of x = 0: {}", result.svg));
    let leftmost = first.iter().map(|&(x, _)| x).fold(f64::INFINITY, f64::min);
    assert!(
        (leftmost - (-8.0 / 3.0)).abs() < 1e-9,
        "leftmost x {leftmost}, the weighted curve's is -8/3"
    );
    // It is sampled, not a corner: points between its ends, all on the
    // curve's side of the chord x = 0.
    let on_spline: Vec<_> = first.iter().filter(|&&(x, _)| x < 0.0).collect();
    assert!(on_spline.len() > 3, "{on_spline:?}");
}

#[test]
fn a_non_rational_spline_edge_stays_inside_its_control_polygon() {
    let result = to_svg(
        &golden(),
        ToSvgOptions {
            space: Space::All,
            padding: 0.0,
            crop: Crop::Everything,
            ..ToSvgOptions::default()
        },
    );
    // The second hatch's spline edge runs from (100, 0) to (100, 20) over
    // control points reaching x = 112; a clamped B-spline lies inside the
    // convex hull of its control points and touches the corner only at
    // its ends, so no drawn point reaches 112 and some pass x = 100.
    let paths = path_points(&result.svg);
    let rightmost = paths
        .iter()
        .flatten()
        .map(|&(x, _)| x)
        .fold(f64::NEG_INFINITY, f64::max);
    assert!(
        rightmost > 100.0 && rightmost < 112.0,
        "rightmost x {rightmost}"
    );
}
