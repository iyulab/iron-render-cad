//! Where each text of a picture is. Every `<text>` carries the path of the
//! entity that drew it -- the text's reference ID after those of the block
//! references it is drawn inside -- as its `id`, and a [`Scene`] reports
//! every text it drew with the box the renderer estimated and, laid out
//! with a set of fonts, the box its glyphs fill. A label's real extent is
//! the fonts', not the estimate's: a hit test, a label-collision check or
//! a caller placing its own marks next to a text needs it.

use std::collections::BTreeMap;
use std::sync::Arc;

use iron_render_cad::{to_svg, Fonts, Rect, Scene, Space, TextBox, ToSvgOptions};
use uncad_model::model::{
    Confidence, Entity, EntityCommon, EntityId, HorizontalJustification, InsertEntity, MTextEntity,
    Origin, Point2D, Point3D, Ref, TextEntity, VerticalJustification,
};
use uncad_model::tables::{BlockRecord, Tables};
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

fn z() -> Point3D {
    Point3D {
        x: 0.0,
        y: 0.0,
        z: 1.0,
    }
}

fn text(id: u64, at: (f64, f64), height: f64, s: &str) -> Entity {
    Entity::Text(TextEntity {
        common: common(id),
        start_point: Point2D { x: at.0, y: at.1 },
        text_height: height,
        text: s.to_string(),
        rotation: 0.0,
        horizontal_justification: HorizontalJustification::Left,
        vertical_justification: VerticalJustification::Baseline,
        alignment_point: None,
        width_factor: 1.0,
        oblique_angle: 0.0,
        style_name: Ref::Absent,
        elevation: 0.0,
        extrusion: z(),
    })
}

fn mtext(id: u64, at: (f64, f64), s: &str) -> Entity {
    Entity::MText(MTextEntity {
        common: common(id),
        insertion_point: Point3D {
            x: at.0,
            y: at.1,
            z: 0.0,
        },
        text: s.to_string(),
        text_height: 2.0,
        rotation: 0.0,
        line_spacing_factor: 1.0,
        attachment: None,
        reference_width: 0.0,
        extents_width: None,
        extents_height: None,
        style_name: Ref::Absent,
    })
}

fn insert(id: u64, block: &str, at: (f64, f64), scale: f64) -> Entity {
    Entity::Insert(InsertEntity {
        common: common(id),
        block_name: Ref::Resolved(block.to_string()),
        insertion_point: Point3D {
            x: at.0,
            y: at.1,
            z: 0.0,
        },
        scale: Point3D {
            x: scale,
            y: scale,
            z: 1.0,
        },
        rotation: 0.0,
        attribs: Vec::new(),
        extrusion: z(),
    })
}

fn db(entities: Vec<Entity>, blocks: Vec<(&str, Vec<Entity>)>) -> CadDatabase {
    CadDatabase {
        entities,
        tables: Tables {
            block_records: blocks
                .into_iter()
                .map(|(name, entities)| {
                    (
                        name.to_string(),
                        BlockRecord {
                            base_point: Default::default(),
                            name: name.to_string(),
                            entities,
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>(),
            ..Tables::default()
        },
        read_diagnostics: ReadDiagnostics::default(),
    }
}

fn all() -> ToSvgOptions {
    ToSvgOptions {
        space: Space::All,
        padding: 0.0,
        ..ToSvgOptions::default()
    }
}

/// A text at the top level, one inside a block placed twice, one inside a
/// block nested in that block, and an MTEXT.
fn nested() -> CadDatabase {
    db(
        vec![
            text(0x10, (0.0, 0.0), 2.5, "TOP"),
            insert(0x20, "OUTER", (100.0, 0.0), 1.0),
            insert(0x30, "OUTER", (200.0, 0.0), 2.0),
            mtext(0x40, (0.0, 50.0), "ONE\\PTWO"),
        ],
        vec![
            (
                "OUTER",
                vec![
                    text(0x21, (0.0, 0.0), 2.5, "IN"),
                    insert(0x22, "INNER", (10.0, 0.0), 1.0),
                ],
            ),
            ("INNER", vec![text(0x23, (0.0, 0.0), 2.5, "DEEP")]),
        ],
    )
}

fn paths(boxes: &[TextBox]) -> Vec<Vec<u64>> {
    boxes
        .iter()
        .map(|b| b.path.iter().map(|id| id.value()).collect())
        .collect()
}

#[test]
fn every_text_carries_its_path_as_its_id() {
    let db = nested();
    let svg = to_svg(&db, all()).svg;
    for id in ["t16", "t32.33", "t32.34.35", "t48.33", "t48.34.35", "t64"] {
        assert_eq!(
            svg.matches(&format!("<text id=\"{id}\"")).count(),
            1,
            "{id} in {svg}"
        );
    }
    // The scene lists the same texts, in drawing order, with what they
    // show.
    let scene = Scene::new(&db, all());
    let boxes = scene
        .text_boxes(&Fonts::Custom(Vec::new()))
        .expect("laid out");
    assert_eq!(
        paths(&boxes),
        [
            vec![0x10],
            vec![0x20, 0x21],
            vec![0x20, 0x22, 0x23],
            vec![0x30, 0x21],
            vec![0x30, 0x22, 0x23],
            vec![0x40],
        ]
    );
    let shown: Vec<&str> = boxes.iter().map(|b| b.text.as_str()).collect();
    assert_eq!(shown, ["TOP", "IN", "DEEP", "IN", "DEEP", "ONE\nTWO"]);
    // No font, nothing laid out: every box is the estimate alone.
    assert!(boxes.iter().all(|b| b.measured.is_none()));
    assert!(boxes.iter().all(|b| b.estimate.is_some()));
}

#[test]
fn the_estimate_is_the_box_the_extent_counts_placed_where_the_text_is() {
    let scene = Scene::new(&nested(), all());
    let boxes = scene
        .text_boxes(&Fonts::Custom(Vec::new()))
        .expect("laid out");
    // "TOP" at height 2.5: 0.6 em a character in a face whose capitals are
    // 0.7 em, three characters, a capital tall.
    let w = 0.6 / 0.7 * 2.5 * 3.0;
    let top = boxes[0].estimate.expect("an estimate");
    let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
    assert!(close(top.min_x, 0.0) && close(top.max_x, w), "{top:?}");
    assert!(close(top.min_y, 0.0) && close(top.max_y, 2.5), "{top:?}");
    // It is what the part's extent is.
    assert_eq!(scene.parts()[0].extent, Some(top));
    // "IN" in the block placed at (200, 0) at twice the size.
    let placed = boxes[3].estimate.expect("an estimate");
    let w = 0.6 / 0.7 * 2.5 * 2.0 * 2.0;
    assert!(close(placed.min_x, 200.0) && close(placed.max_x, 200.0 + w));
    assert!(close(placed.min_y, 0.0) && close(placed.max_y, 5.0));
}

/// A font file from the host, when one of the usual ones is there.
fn host_font() -> Option<Fonts> {
    [
        "C:/Windows/Fonts/arial.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
    ]
    .iter()
    .find_map(|path| std::fs::read(path).ok())
    .map(|bytes| Fonts::Custom(vec![Arc::from(bytes)]))
}

#[test]
fn texts_are_measured_with_the_fonts_they_are_drawn_in() {
    let Some(fonts) = host_font() else {
        eprintln!("no font file found on this host; nothing to measure");
        return;
    };
    let db = db(
        vec![
            text(0x10, (10.0, 10.0), 2.5, "HELLO"),
            // A Hangul syllable, which neither Arial nor DejaVu Sans has.
            text(0x11, (10.0, 30.0), 2.5, "A\u{AC00}"),
            text(0x12, (10.0, 50.0), 2.5, "   "),
        ],
        Vec::new(),
    );
    let scene = Scene::new(&db, all());
    let boxes = scene.text_boxes(&fonts).expect("laid out");
    let hello = boxes[0].measured.expect("measured");
    // The glyphs start at the anchor and stand on the baseline, capitals
    // about the text height tall (the face's own capitals, drawn at a
    // font size of height / 0.7).
    assert!((hello.min_x - 10.0).abs() < 0.5, "{hello:?}");
    assert!((hello.min_y - 10.0).abs() < 0.1, "{hello:?}");
    assert!(
        (hello.height() - 2.5).abs() < 0.3,
        "{} tall: {hello:?}",
        hello.height()
    );
    assert!(hello.width() > 5.0 && hello.width() < 12.0, "{hello:?}");
    assert_eq!(boxes[0].missing_glyphs, 0);
    // A glyph the face lacks is drawn as its missing-glyph shape, and
    // counted.
    assert!(boxes[1].measured.is_some());
    assert_eq!(boxes[1].missing_glyphs, 1);
    // Spaces lay out no outline: no measured box, the estimate stays.
    assert_eq!(boxes[2].measured, None);
    assert!(boxes[2].estimate.is_some());
}

#[test]
fn a_far_away_text_is_measured_where_it_is() {
    let Some(fonts) = host_font() else {
        eprintln!("no font file found on this host; nothing to measure");
        return;
    };
    let db = db(
        vec![
            text(0x10, (2.5e8, -2.5e8), 2.5, "FAR"),
            text(0x11, (2.5e8 + 100.0, -2.5e8), 2.5, "AWAY"),
        ],
        Vec::new(),
    );
    let scene = Scene::new(&db, all());
    assert!(scene.origin.x > 32768.0);
    let boxes = scene.text_boxes(&fonts).expect("laid out");
    let far = boxes[0].measured.expect("measured");
    assert!((far.min_x - 2.5e8).abs() < 0.5, "{far:?}");
    assert!((far.min_y + 2.5e8).abs() < 0.1, "{far:?}");
    let estimate = boxes[0].estimate.expect("an estimate");
    assert!(far.intersects(&estimate));
}

#[test]
fn a_sheet_lists_its_own_texts_and_the_models_it_shows() {
    let mut g14: CadDatabase = serde_json::from_str(include_str!("golden/g14.expected.json"))
        .expect("the golden model deserializes");
    // Two model texts: one where the detail viewport looks, one far from
    // anything a viewport shows.
    let model = g14
        .tables
        .block_records
        .get_mut("*Model_Space")
        .expect("model space");
    for e in [
        text(0x900, (120.0, 0.0), 5.0, "SEEN"),
        text(0x901, (5000.0, 5000.0), 5.0, "UNSEEN"),
    ] {
        model.entities.push(e.clone());
        g14.entities.push(e);
    }
    let scene = Scene::layout(&g14, "Layout1", ToSvgOptions::default()).expect("a sheet");
    let boxes = scene
        .text_boxes(&Fonts::Custom(Vec::new()))
        .expect("laid out");
    // The sheet's title, then the model's text through the detail viewport
    // (272); the one no frame shows is not on the sheet.
    assert_eq!(paths(&boxes), [vec![270], vec![272, 0x900]]);
    assert_eq!(boxes[0].text, "SHEET 1");
    // The model text's estimate is on the paper, inside the frame.
    let frame = Rect::new(50.0, 75.0, 250.0, 225.0);
    let seen = boxes[1].estimate.expect("an estimate");
    assert!(frame.intersects(&seen), "{seen:?}");
    let svg = scene.svg(scene.view_box, scene.auto_stroke_width, |_| true);
    assert!(svg.contains("<text id=\"t272.2304\""), "{svg}");
    assert!(!svg.contains("UNSEEN"), "{svg}");
}

#[test]
fn a_text_nothing_of_which_is_drawn_is_not_listed() {
    // A block reference scaled past the 1e15 units a picture can be built
    // from is left out whole, and reported; the text inside it is not in
    // the picture, so it is not among the scene's texts either.
    let db = db(
        vec![
            text(0x10, (0.0, 0.0), 2.5, "HERE"),
            insert(0x20, "B", (0.0, 0.0), 1e16),
        ],
        vec![("B", vec![text(0x21, (1.0, 1.0), 2.5, "GONE")])],
    );
    let scene = Scene::new(&db, all());
    assert!(
        scene.limits.out_of_range_entities == 1,
        "{:?}",
        scene.limits
    );
    let boxes = scene
        .text_boxes(&Fonts::Custom(Vec::new()))
        .expect("laid out");
    assert_eq!(paths(&boxes), [vec![0x10]]);
}
