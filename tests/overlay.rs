//! A change set drawn on top of the original.
//!
//! The drawing is the first golden case (`tests/golden/`): a plate with four
//! holes. The second state is the same model with its JSON edited -- a hole
//! made smaller, one taken away, one added -- and the change set is what
//! the change-set library says the difference is.
//!
//! What must never happen is tested first: the original layer is exactly
//! the original render, and no change is dropped without a reason.

use iron_diff_cad::{diff, Change, ChangeSet, DiffOptions, Unknown};
use iron_render_cad::{
    overlay_to_svg, svg_to_png, to_svg, MarkKind, NotMarkedReason, OverlayOptions, ToSvgOptions,
    CONFLICT_DELTA_E,
};
use serde_json::Value;
use uncad_model::model::EntityId;
use uncad_model::CadDatabase;

fn g1_json() -> Value {
    serde_json::from_str(include_str!("golden/g1.expected.json"))
        .expect("the golden model deserializes")
}

fn model(v: &Value) -> CadDatabase {
    serde_json::from_value(v.clone()).expect("an edited model deserializes")
}

/// Applies `edit` to every entity with reference ID `id`, wherever it sits
/// (the top-level list and the block that owns it both carry it).
fn edit(v: &mut Value, id: u64, edit: &dyn Fn(&mut Value)) {
    match v {
        Value::Object(map) => {
            let is_it = map
                .get("common")
                .and_then(|c| c.get("id"))
                .and_then(Value::as_u64)
                == Some(id);
            if is_it {
                edit(v);
                return;
            }
            for child in map.values_mut() {
                self::edit(child, id, edit);
            }
        }
        Value::Array(items) => {
            for child in items {
                self::edit(child, id, edit);
            }
        }
        _ => {}
    }
}

/// Removes every entity with reference ID `id`.
fn remove(v: &mut Value, id: u64) {
    match v {
        Value::Object(map) => {
            for child in map.values_mut() {
                remove(child, id);
            }
        }
        Value::Array(items) => {
            items.retain(|e| {
                e.get("common")
                    .and_then(|c| c.get("id"))
                    .and_then(Value::as_u64)
                    != Some(id)
            });
            for child in items {
                remove(child, id);
            }
        }
        _ => {}
    }
}

/// A copy of hole 291 moved right, as a new entity with ID `id`, in the
/// drawing's entity list and under the model space block that owns it.
fn add_hole(v: &mut Value, id: u64) {
    let hole = v["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["common"]["id"] == 291)
        .unwrap()
        .clone();
    let mut hole = hole;
    hole["common"]["id"] = id.into();
    hole["center"]["x"] = 100.0.into();
    v["entities"].as_array_mut().unwrap().push(hole.clone());
    v["tables"]["block_records"]["*Model_Space"]["entities"]
        .as_array_mut()
        .unwrap()
        .push(hole);
}

/// The body `to_svg` writes: what sits between the root element's opening
/// tag (and the pattern definitions, if any) and its end.
fn body(document: &str) -> &str {
    let start = document.find("\n  ").unwrap() + 3;
    let rest = &document[start..];
    let rest = match rest.find("</defs>\n  ") {
        Some(i) if rest.starts_with("<defs>") => &rest[i + "</defs>\n  ".len()..],
        _ => rest,
    };
    rest.strip_suffix("\n</svg>").unwrap()
}

#[test]
fn the_original_layer_is_the_original_render_byte_for_byte() {
    let before_json = g1_json();
    let mut after_json = before_json.clone();
    edit(&mut after_json, 289, &|e| e["radius"] = 4.0.into());
    let (before, after) = (model(&before_json), model(&after_json));
    let changes = diff(&before, &after, DiffOptions::default());

    let alone = to_svg(&before, ToSvgOptions::default()).svg;
    let overlay = overlay_to_svg(&before, &after, &changes, OverlayOptions::default());
    let original = format!("<g id=\"original\">\n  {}\n</g>", body(&alone));
    assert!(
        overlay.svg.contains(&original),
        "the original layer differs from the original render"
    );
    // The same viewBox and stroke width as the original render.
    let header = |s: &str| s.lines().next().unwrap().to_string();
    assert_eq!(header(&overlay.svg), header(&alone));
}

#[test]
fn a_smaller_hole_is_drawn_as_it_became_with_a_cloud_around_both() {
    let before_json = g1_json();
    let mut after_json = before_json.clone();
    edit(&mut after_json, 289, &|e| e["radius"] = 4.0.into());
    let (before, after) = (model(&before_json), model(&after_json));
    let changes = diff(&before, &after, DiffOptions::default());
    let overlay = overlay_to_svg(&before, &after, &changes, OverlayOptions::default());

    assert!(overlay.not_marked.is_empty(), "{:?}", overlay.not_marked);
    let [mark] = overlay.marked.as_slice() else {
        panic!("one mark expected: {:?}", overlay.marked)
    };
    assert_eq!(mark.kind, MarkKind::Modified);
    assert_eq!(mark.before, Some(EntityId::new(289)));
    assert_eq!(mark.after, Some(EntityId::new(289)));
    // The cloud takes in the larger of the two states: the original hole.
    assert!((mark.cloud.min_x - 15.0).abs() < 1e-9, "{:?}", mark.cloud);
    assert!((mark.cloud.max_x - 25.0).abs() < 1e-9, "{:?}", mark.cloud);

    let changes_layer = &overlay.svg[overlay.svg.find("<g id=\"changes\">").unwrap()..];
    assert!(changes_layer.contains("<circle"), "the new hole is drawn");
    assert!(changes_layer.contains(" r=\"4\""), "at its new radius");
    assert_eq!(changes_layer.matches("class=\"cloud\"").count(), 1);
}

#[test]
fn every_kind_of_change_is_marked_or_reported() {
    let before_json = g1_json();
    let mut after_json = before_json.clone();
    edit(&mut after_json, 289, &|e| e["radius"] = 4.0.into());
    remove(&mut after_json, 290);
    add_hole(&mut after_json, 301);
    // Inside the first dimension's block: placed wherever the dimension is.
    edit(&mut after_json, 267, &|e| {
        e["end_point"]["x"] = (e["end_point"]["x"].as_f64().unwrap() + 1.0).into()
    });
    let (before, after) = (model(&before_json), model(&after_json));
    let mut changes = diff(&before, &after, DiffOptions::default());
    // A change whose counterpart is not decided, and one about an entity
    // neither state has at the top level.
    changes.changes.push(Change::Unknown(Unknown {
        id: EntityId::new(291),
        candidates: vec![EntityId::new(292)],
        reason: "test".into(),
    }));
    let overlay = overlay_to_svg(&before, &after, &changes, OverlayOptions::default());

    assert_eq!(
        overlay.marked.len() + overlay.not_marked.len(),
        changes.changes.len(),
        "every change is accounted for"
    );
    let kind_of = |id: u64| {
        overlay
            .marked
            .iter()
            .find(|m| m.before == Some(EntityId::new(id)) || m.after == Some(EntityId::new(id)))
            .map(|m| m.kind)
    };
    assert_eq!(kind_of(289), Some(MarkKind::Modified));
    assert_eq!(kind_of(290), Some(MarkKind::Removed));
    assert_eq!(kind_of(301), Some(MarkKind::Added));
    let unknown = overlay
        .marked
        .iter()
        .find(|m| m.kind == MarkKind::Unknown)
        .unwrap();
    assert_eq!(
        unknown.after, None,
        "an undecided counterpart is never drawn"
    );
    // Its cloud takes in the entity and its candidate: both holes on the right.
    assert!(
        unknown.cloud.max_x > 180.0 && unknown.cloud.min_x < 25.0,
        "{:?}",
        unknown.cloud
    );

    let inside = overlay
        .not_marked
        .iter()
        .find(|n| n.id == EntityId::new(267))
        .expect("the change inside a block is reported");
    assert_eq!(inside.reason, NotMarkedReason::NotTopLevel);

    let layer = &overlay.svg[overlay.svg.find("<g id=\"changes\">").unwrap()..];
    assert!(layer.contains("<g class=\"removed\">"));
    assert_eq!(layer.matches("class=\"cloud unknown\"").count(), 1);
    assert_eq!(layer.matches("class=\"cloud\"").count(), 3);
}

#[test]
fn a_change_the_render_draws_nothing_for_is_reported() {
    let before = model(&g1_json());
    let changes = ChangeSet {
        changes: vec![Change::Removed(iron_diff_cad::EntityRecord {
            id: EntityId::new(9999),
            entity_type: "LINE".into(),
            provenance: uncad_model::model::Origin::Vector,
            confidence: uncad_model::model::Confidence::High,
        })],
        ..diff(&before, &before, DiffOptions::default())
    };
    let overlay = overlay_to_svg(&before, &before, &changes, OverlayOptions::default());
    assert!(overlay.marked.is_empty());
    assert_eq!(overlay.not_marked.len(), 1);
    assert_eq!(overlay.not_marked[0].reason, NotMarkedReason::NotTopLevel);
    // Nothing to draw: the change layer is empty.
    assert!(overlay.svg.contains("<g id=\"changes\">\n</g>"));
}

#[test]
fn the_overlay_is_deterministic_and_takes_the_proposal_colour() {
    let before_json = g1_json();
    let mut after_json = before_json.clone();
    edit(&mut after_json, 289, &|e| e["radius"] = 4.0.into());
    remove(&mut after_json, 290);
    let (before, after) = (model(&before_json), model(&after_json));
    let changes = diff(&before, &after, DiffOptions::default());
    let options = OverlayOptions {
        proposal_color: [0x00, 0x66, 0xcc],
        ..OverlayOptions::default()
    };
    let first = overlay_to_svg(&before, &after, &changes, options);
    for _ in 0..8 {
        assert_eq!(overlay_to_svg(&before, &after, &changes, options), first);
    }
    assert!(first.svg.contains("#changes *{stroke:#0066cc;fill:none}"));
}

#[test]
fn an_addition_outside_the_drawing_grows_the_view() {
    let before_json = g1_json();
    let mut after_json = before_json.clone();
    add_hole(&mut after_json, 301);
    edit(&mut after_json, 301, &|e| e["center"]["x"] = 1000.0.into());
    let (before, after) = (model(&before_json), model(&after_json));
    let changes = diff(&before, &after, DiffOptions::default());
    let alone = to_svg(&before, ToSvgOptions::default());
    let overlay = overlay_to_svg(&before, &after, &changes, OverlayOptions::default());
    assert!(overlay.view_box.max_x > 1000.0, "{:?}", overlay.view_box);
    assert!(alone.view_box.max_x < 1000.0);
    assert_eq!(overlay.origin, alone.origin);
}

#[test]
fn the_overlay_renders_to_png() {
    let before_json = g1_json();
    let mut after_json = before_json.clone();
    edit(&mut after_json, 289, &|e| e["radius"] = 4.0.into());
    let (before, after) = (model(&before_json), model(&after_json));
    let changes = diff(&before, &after, DiffOptions::default());
    let overlay = overlay_to_svg(&before, &after, &changes, OverlayOptions::default());
    let png = svg_to_png(&overlay.svg, 1.0).expect("the overlay rasterizes");
    assert!(png.starts_with(b"\x89PNG"));
}

#[test]
fn the_change_layer_is_drawn_in_the_proposal_colour_when_rasterized() {
    use resvg::usvg;
    let before_json = g1_json();
    let mut after_json = before_json.clone();
    edit(&mut after_json, 289, &|e| e["radius"] = 4.0.into());
    let (before, after) = (model(&before_json), model(&after_json));
    let changes = diff(&before, &after, DiffOptions::default());
    let overlay = overlay_to_svg(&before, &after, &changes, OverlayOptions::default());

    // The tree the rasterizer draws from: the style rule has to have
    // reached every stroke under the change layer.
    let tree = usvg::Tree::from_str(&overlay.svg, &usvg::Options::default()).unwrap();
    fn find<'a>(g: &'a usvg::Group, id: &str) -> Option<&'a usvg::Group> {
        if g.id() == id {
            return Some(g);
        }
        g.children().iter().find_map(|n| match n {
            usvg::Node::Group(c) => find(c, id),
            _ => None,
        })
    }
    fn strokes(g: &usvg::Group, out: &mut Vec<(u8, u8, u8)>) {
        for n in g.children() {
            match n {
                usvg::Node::Group(c) => strokes(c, out),
                usvg::Node::Path(p) => {
                    if let Some(usvg::Paint::Color(c)) = p.stroke().map(|s| s.paint()) {
                        out.push((c.red, c.green, c.blue));
                    }
                    assert!(p.fill().is_none(), "the change layer draws outlines only");
                }
                _ => {}
            }
        }
    }
    let changes_group = find(tree.root(), "changes").expect("the change layer survives parsing");
    let mut found = Vec::new();
    strokes(changes_group, &mut found);
    assert!(found.len() >= 2, "the hole and its cloud: {found:?}");
    assert!(found.iter().all(|&c| c == (0xe4, 0x00, 0x2b)), "{found:?}");
}

#[test]
fn a_grown_view_draws_the_original_as_the_same_render_of_the_larger_window() {
    use iron_render_cad::{LeftOutReason, Scene};
    let before_json = g1_json();
    let mut after_json = before_json.clone();
    add_hole(&mut after_json, 301);
    edit(&mut after_json, 301, &|e| e["center"]["x"] = 1000.0.into());
    let (before, after) = (model(&before_json), model(&after_json));
    let changes = diff(&before, &after, DiffOptions::default());
    let overlay = overlay_to_svg(&before, &after, &changes, OverlayOptions::default());

    let scene = Scene::new(&before, ToSvgOptions::default());
    let alone = scene.svg(overlay.view_box, scene.auto_stroke_width, |p| {
        !matches!(
            p.left_out,
            Some(LeftOutReason::ScaleOutlier | LeftOutReason::FarOutlier)
        )
    });
    let original = format!("<g id=\"original\">\n  {}\n</g>", body(&alone));
    assert!(overlay.svg.contains(&original));
}

#[test]
fn framing_the_changes_shows_them_at_a_readable_size() {
    use iron_render_cad::{LeftOutReason, OverlayFrame, Scene};
    let before_json = g1_json();
    let mut after_json = before_json.clone();
    edit(&mut after_json, 289, &|e| e["radius"] = 4.0.into());
    let (before, after) = (model(&before_json), model(&after_json));
    let changes = diff(&before, &after, DiffOptions::default());
    let whole = overlay_to_svg(&before, &after, &changes, OverlayOptions::default());
    let framed = overlay_to_svg(
        &before,
        &after,
        &changes,
        OverlayOptions {
            frame: OverlayFrame::Changes,
            ..OverlayOptions::default()
        },
    );
    let cloud = framed.marked[0].cloud;
    let v = framed.view_box;
    // The window holds the change and some of the drawing around it...
    assert!(
        v.min_x < cloud.min_x && cloud.max_x < v.max_x,
        "{v:?} {cloud:?}"
    );
    assert!(
        v.min_y < cloud.min_y && cloud.max_y < v.max_y,
        "{v:?} {cloud:?}"
    );
    // ...and is far smaller than the whole drawing, at a finer stroke.
    assert!(v.width() < whole.view_box.width() / 4.0, "{v:?}");
    assert!(framed.stroke_width < whole.stroke_width);

    // The original layer is the same render, written for that window.
    let scene = Scene::new(&before, ToSvgOptions::default());
    let alone = scene.svg(v, framed.stroke_width, |p| {
        !matches!(
            p.left_out,
            Some(LeftOutReason::ScaleOutlier | LeftOutReason::FarOutlier)
        )
    });
    assert!(framed
        .svg
        .contains(&format!("<g id=\"original\">\n  {}\n</g>", body(&alone))));
}

#[test]
fn framing_no_changes_shows_the_whole_drawing() {
    use iron_render_cad::OverlayFrame;
    let before = model(&g1_json());
    let changes = diff(&before, &before, DiffOptions::default());
    let framed = overlay_to_svg(
        &before,
        &before,
        &changes,
        OverlayOptions {
            frame: OverlayFrame::Changes,
            ..OverlayOptions::default()
        },
    );
    let alone = to_svg(&before, ToSvgOptions::default());
    assert_eq!(framed.view_box, alone.view_box);
    assert!(framed.svg.contains(body(&alone.svg)));
}

/// G1's holes are drawn in pure red, the default proposal color is a red
/// too: the report names the clash -- the color, how much of the original
/// uses it, how close it is -- so a caller can pick another color. With a
/// blue proposal color nothing in G1 is close and the report is empty.
#[test]
fn an_original_color_close_to_the_proposal_color_is_reported() {
    let before_json = g1_json();
    let mut after_json = before_json.clone();
    edit(&mut after_json, 289, &|e| e["radius"] = 4.0.into());
    let (before, after) = (model(&before_json), model(&after_json));
    let changes = diff(&before, &after, DiffOptions::default());

    let red = overlay_to_svg(&before, &after, &changes, OverlayOptions::default());
    let [conflict] = red.proposal_color_conflicts.as_slice() else {
        panic!("one clash expected: {:?}", red.proposal_color_conflicts)
    };
    assert_eq!(conflict.color, "#ff0000");
    assert_eq!(conflict.uses, 4, "the four holes");
    assert!(conflict.delta_e < CONFLICT_DELTA_E);
    assert!(
        (conflict.delta_e - 23.9).abs() < 0.1,
        "{}",
        conflict.delta_e
    );

    let blue = overlay_to_svg(
        &before,
        &after,
        &changes,
        OverlayOptions {
            proposal_color: [0x00, 0x66, 0xcc],
            ..OverlayOptions::default()
        },
    );
    assert!(
        blue.proposal_color_conflicts.is_empty(),
        "{:?}",
        blue.proposal_color_conflicts
    );
}
