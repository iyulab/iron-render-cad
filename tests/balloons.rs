//! Numbered balloons drawn on top of the original.
//!
//! The drawing is the first golden case (`tests/golden/`): a plate with four
//! holes and a title block. What must never happen is tested first: the
//! original layer is exactly the original render, and no balloon vanishes
//! without a reason.

use iron_render_cad::{
    balloons_to_svg, to_svg, Balloon, BalloonOptions, BalloonSize, BalloonState, BalloonTarget,
    NotPlacedReason, ProposalColor, Scene, ToSvgOptions,
};
use serde_json::Value;
use uncad_model::model::{EntityId, Point2D};
use uncad_model::CadDatabase;

fn g1_json() -> Value {
    serde_json::from_str(include_str!("golden/g1.expected.json"))
        .expect("the golden model deserializes")
}

fn model(v: &Value) -> CadDatabase {
    serde_json::from_value(v.clone()).expect("a model deserializes")
}

fn g1() -> CadDatabase {
    model(&g1_json())
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

fn on(label: &str, id: u64) -> Balloon {
    Balloon::new(label, BalloonTarget::Entity(EntityId::new(id)))
}

fn at(label: &str, x: f64, y: f64) -> Balloon {
    Balloon::new(label, BalloonTarget::Point(Point2D { x, y }))
}

/// The centre of the box the render says entity `id` covers.
fn centre_of(db: &CadDatabase, id: u64) -> Point2D {
    let scene = Scene::new(db, ToSvgOptions::default());
    let part = scene
        .parts()
        .iter()
        .find(|p| p.id == EntityId::new(id))
        .expect("a top-level entity");
    let r = part.extent.expect("it drew something");
    Point2D {
        x: (r.min_x + r.max_x) / 2.0,
        y: (r.min_y + r.max_y) / 2.0,
    }
}

#[test]
fn the_original_layer_is_the_original_render_byte_for_byte() {
    let db = g1();
    let alone = to_svg(&db, ToSvgOptions::default()).svg;
    let result = balloons_to_svg(
        &db,
        &[on("1", 289), on("2", 290)],
        BalloonOptions::default(),
    );

    let original = format!("<g id=\"original\">\n  {}\n</g>", body(&alone));
    assert!(
        result.svg.contains(&original),
        "the original layer differs from the original render"
    );
    // Both balloons sit inside the original's window, so the picture is framed as it was.
    let header = |s: &str| s.lines().next().unwrap().to_string();
    assert_eq!(header(&result.svg), header(&alone));
    assert_eq!(result.placed.len(), 2);
    assert!(result.not_placed.is_empty());
}

#[test]
fn the_same_input_gives_the_same_bytes() {
    let db = g1();
    let balloons = [
        on("1", 289),
        on("2", 290),
        on("2.1", 291),
        at("3", 5.0, 5.0),
    ];
    let first = balloons_to_svg(&db, &balloons, BalloonOptions::default());
    let second = balloons_to_svg(&db, &balloons, BalloonOptions::default());
    assert_eq!(first.svg, second.svg);
    assert_eq!(first.placed, second.placed);
}

#[test]
fn a_balloon_points_at_the_centre_of_what_its_entity_covers_and_shows_the_label_as_given() {
    let db = g1();
    let result = balloons_to_svg(&db, &[on("5.1", 289)], BalloonOptions::default());
    let [placed] = result.placed.as_slice() else {
        panic!("one balloon is placed: {:?}", result.placed);
    };
    let expected = centre_of(&db, 289);
    assert_eq!(placed.leader_end, expected);
    assert_eq!(placed.label, "5.1");
    assert!(!placed.crowded);
    assert!(placed.via.is_empty());
    // The circle is drawn at its centre and radius, with the label in it.
    assert!(result.svg.contains("<g id=\"balloons\">"));
    assert!(result.svg.contains(">5.1</text>"));
    assert!(result.svg.contains(&format!("r=\"{}\"", placed.radius)));
    // The circle does not sit on its target: the leader has length.
    let distance = (placed.center.x - expected.x).hypot(placed.center.y - expected.y);
    assert!(distance > placed.radius);
}

#[test]
fn a_label_is_text_not_markup() {
    let db = g1();
    let result = balloons_to_svg(&db, &[at("<A&B>", 1.0, 1.0)], BalloonOptions::default());
    assert!(result.svg.contains(">&lt;A&amp;B&gt;</text>"));
    assert!(!result.svg.contains("<A&B>"));
}

#[test]
fn a_dropped_balloon_stands_on_a_point_and_looks_different() {
    let db = g1();
    let active = balloons_to_svg(&db, &[at("7", 10.0, 10.0)], BalloonOptions::default());
    let dropped = balloons_to_svg(
        &db,
        &[Balloon {
            state: BalloonState::Dropped,
            ..at("7", 10.0, 10.0)
        }],
        BalloonOptions::default(),
    );
    assert!(active.svg.contains("class=\"balloon\""));
    assert!(dropped.svg.contains("class=\"balloon dropped\""));
    assert_eq!(dropped.placed[0].leader_end, Point2D { x: 10.0, y: 10.0 });
    assert_ne!(active.svg, dropped.svg);
}

#[test]
fn a_balloon_outside_the_drawing_grows_the_picture_instead_of_leaving() {
    let db = g1();
    let alone = to_svg(&db, ToSvgOptions::default());
    let result = balloons_to_svg(&db, &[at("9", 5000.0, 5000.0)], BalloonOptions::default());
    assert_eq!(result.placed.len(), 1);
    assert!(result.view_box.max_x > alone.view_box.max_x);
    let p = &result.placed[0];
    assert!(result.view_box.max_x >= p.center.x + p.radius);
    assert!(result.view_box.max_y >= p.center.y + p.radius);
    // The leader's far end is in the picture too, not only its circle.
    // (The circle sits 2.5 radii up from its target, so the leader's end is further out than the
    // circle's own padding reaches -- a window grown by circles alone would cut it off.)
    for x in [p.leader_end.x, p.center.x] {
        assert!(result.view_box.min_x <= x && x <= result.view_box.max_x);
    }
    for y in [p.leader_end.y, p.center.y] {
        assert!(result.view_box.min_y <= y && y <= result.view_box.max_y);
    }
}

#[test]
fn a_balloon_that_cannot_be_drawn_is_reported_by_name() {
    let mut v = g1_json();
    // Hide hole 290: the render draws nothing for it.
    fn hide(v: &mut Value) {
        match v {
            Value::Object(map) => {
                if map
                    .get("common")
                    .and_then(|c| c.get("id"))
                    .and_then(Value::as_str)
                    == Some("290")
                {
                    v["common"]["invisible"] = true.into();
                    return;
                }
                for child in map.values_mut() {
                    hide(child);
                }
            }
            Value::Array(items) => items.iter_mut().for_each(hide),
            _ => {}
        }
    }
    hide(&mut v);
    let db = model(&v);
    let nowhere = Balloon::new(
        "3",
        BalloonTarget::Point(Point2D {
            x: f64::NAN,
            y: 0.0,
        }),
    );
    let result = balloons_to_svg(
        &db,
        &[on("1", 289), on("2", 290), on("3", 987_654), nowhere],
        BalloonOptions::default(),
    );
    assert_eq!(
        result.placed.iter().map(|p| p.index).collect::<Vec<_>>(),
        [0]
    );
    let reasons: Vec<_> = result
        .not_placed
        .iter()
        .map(|n| (n.index, n.reason))
        .collect();
    assert_eq!(
        reasons,
        [
            (1, NotPlacedReason::EntityNotDrawn),
            (2, NotPlacedReason::NotTopLevel),
            (3, NotPlacedReason::NoPoint),
        ]
    );
}

#[test]
fn a_balloon_on_an_entity_in_a_block_is_drawn_at_every_reference_that_places_it() {
    let db = g1();
    // The title block's frame (261) lives in TITLEBLOCK; INSERT 297 places it once.
    let result = balloons_to_svg(&db, &[on("A", 261)], BalloonOptions::default());
    let [placed] = result.placed.as_slice() else {
        panic!("one reference places the block: {:?}", result);
    };
    assert_eq!(placed.via, [EntityId::new(297)]);
    assert_eq!(placed.index, 0);
}

#[test]
fn a_balloon_is_never_left_out_for_lack_of_room() {
    let db = g1();
    // More balloons on one point than there are spots around it (8 x 3).
    let balloons: Vec<Balloon> = (0..30).map(|i| at(&i.to_string(), 40.0, 40.0)).collect();
    let result = balloons_to_svg(&db, &balloons, BalloonOptions::default());
    assert_eq!(result.placed.len(), 30);
    assert!(result.not_placed.is_empty());
    // The first has room; the crowd ends in overlap, and says so.
    assert!(!result.placed[0].crowded);
    assert!(result.placed.iter().any(|p| p.crowded));
    // A balloon that has room does not overlap another that has.
    for (i, a) in result.placed.iter().enumerate().filter(|(_, p)| !p.crowded) {
        for b in result.placed.iter().skip(i + 1).filter(|p| !p.crowded) {
            let d = (a.center.x - b.center.x).hypot(a.center.y - b.center.y);
            assert!(
                d >= a.radius + b.radius,
                "{} and {} overlap",
                a.label,
                b.label
            );
        }
    }
}

#[test]
fn the_size_and_the_colour_follow_the_options() {
    let db = g1();
    let result = balloons_to_svg(
        &db,
        &[at("1", 10.0, 10.0)],
        BalloonOptions {
            size: BalloonSize::Units(8.0),
            color: ProposalColor::Given([0x00, 0x60, 0xff]),
            ..BalloonOptions::default()
        },
    );
    assert_eq!(result.placed[0].radius, 4.0);
    assert_eq!(result.color, "#0060ff");
    assert!(result.svg.contains("#balloons *{stroke:#0060ff;fill:none}"));
}

#[test]
fn a_balloons_leader_end_is_in_the_picture_even_when_its_circle_stands_nearer_the_drawing() {
    let db = g1();
    // Below and left of the drawing the circle (placed up from its target) is nearer the drawing
    // than the target is: a window grown by the circle alone would stop short of the leader's end.
    let result = balloons_to_svg(&db, &[at("9", -5000.0, -5000.0)], BalloonOptions::default());
    let p = &result.placed[0];
    assert!(result.view_box.min_x <= p.leader_end.x);
    assert!(result.view_box.min_y <= p.leader_end.y);
}
