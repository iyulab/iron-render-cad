//! The same model must render to the same bytes, every time, and what could
//! not be drawn must be reported the same way every time.
//!
//! Nothing else in the suite would notice a violation: a picture that is
//! right but whose elements arrive in a different order on the next run
//! passes every ordinary assertion. The usual culprit is a hash-based
//! collection whose iteration order leaks into the output -- `std`'s hasher
//! is seeded per instance, so the order differs between two calls in one
//! process, which is what makes the repetition below able to catch it.
//!
//! The drawing is the first golden case's expected model (`tests/golden/`),
//! deserialized from its JSON: a real drawing's worth of entities, and no
//! parser in the loop.

use iron_render_cad::{
    to_png, to_svg, Background, Crop, Fonts, Rect, Scene, Space, ToPngOptions, ToSvgOptions, View,
};
use uncad_model::CadDatabase;

/// How often each output is regenerated. With four or more entries in a
/// leaked hash set, two consecutive identical orders are already unlikely;
/// this many leave no realistic chance of a false pass.
const RUNS: usize = 24;

fn g1() -> CadDatabase {
    serde_json::from_str(include_str!("golden/g1.expected.json"))
        .expect("the golden model deserializes")
}

#[test]
fn repeated_svg_renders_are_byte_identical() {
    let db = g1();
    for space in [Space::Model, Space::Paper, Space::All] {
        let options = ToSvgOptions {
            space,
            ..ToSvgOptions::default()
        };
        let first = to_svg(&db, options);
        for run in 1..RUNS {
            let again = to_svg(&db, options);
            assert_eq!(first.svg, again.svg, "run {run}, {space:?}: SVG changed");
            assert_eq!(
                first.unsupported_types, again.unsupported_types,
                "run {run}: unsupported_types changed"
            );
            assert_eq!(
                first.empty_blocks, again.empty_blocks,
                "run {run}: empty_blocks changed"
            );
        }
    }
}

#[test]
fn repeated_png_renders_are_byte_identical() {
    let db = g1();
    let first = to_png(&db, ToPngOptions::default()).expect("G1 renders to PNG");
    assert!(first.png.starts_with(b"\x89PNG\r\n\x1a\n"));
    for run in 1..8 {
        let again = to_png(&db, ToPngOptions::default()).expect("G1 renders to PNG");
        assert_eq!(first.png, again.png, "run {run}: PNG bytes changed");
    }
}

#[test]
fn repeated_scenes_give_the_same_parts_windows_tiles_and_texts() {
    let db = g1();
    let options = ToSvgOptions {
        crop: Crop::Guarded { stated: None },
        ..ToSvgOptions::default()
    };
    let window = Rect::new(0.0, 0.0, 60.0, 40.0);
    let view = View::of(window, 8.0).expect("a view");
    let render = || {
        let scene = Scene::new(&db, options);
        let keep = |p: &iron_render_cad::Part| p.extent.is_some_and(|e| e.intersects(&window));
        (
            scene.parts().to_vec(),
            scene.crop.clone(),
            scene.svg(window, 0.2, keep),
            scene
                .png(&view, 1.25, &Fonts::System, Background::White, keep)
                .expect("a tile"),
            scene.text_boxes(&Fonts::System).expect("laid out"),
        )
    };
    let first = render();
    for run in 1..8 {
        assert!(render() == first, "run {run}: a scene's output changed");
    }
}

#[test]
fn unsupported_types_are_sorted_and_g1_draws_everything_it_declares() {
    let db = g1();
    let result = to_svg(&db, ToSvgOptions::default());
    let mut sorted = result.unsupported_types.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(result.unsupported_types, sorted, "reported in sorted order");
    // G1 is built from types the renderer draws (outline, holes, dimensions
    // through their blocks, a title block with attributes); nothing in it
    // may be reported as left out, and nothing in it references an empty
    // block.
    assert!(
        result.unsupported_types.is_empty(),
        "{:?}",
        result.unsupported_types
    );
    assert!(result.empty_blocks.is_empty(), "{:?}", result.empty_blocks);
    // The title block's three attribute values and the outline are drawn.
    for text in ["BP-1042", "SS400"] {
        assert!(result.svg.contains(text), "{text} is drawn");
    }
    assert!(
        result.svg.contains("<polygon"),
        "the closed outline is a polygon"
    );
}
