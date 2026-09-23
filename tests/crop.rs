//! What the picture shows, and what it says it left out. The crop chooses
//! the world rectangle a render frames from the extents its entities
//! measured -- the dominant cluster (the default), everything, everything
//! but a few outliers (the guard, which may take an extent the caller
//! states instead), or a window -- and every result lists the entities the
//! picture does not show, with the reason. An entity is never dropped
//! without being named.

use std::collections::BTreeMap;

use iron_render_cad::{
    layout_to_svg, to_png, to_svg, Crop, LeftOutReason, Rect, Scene, Space, ToPngOptions,
    ToSvgOptions,
};
use uncad_model::model::{
    Confidence, Entity, EntityCommon, EntityId, InsertEntity, LineEntity, Origin, Point3D,
    RayEntity, Ref,
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
    }
}

fn xyz(x: f64, y: f64) -> Point3D {
    Point3D { x, y, z: 0.0 }
}

fn line(id: u64, from: (f64, f64), to: (f64, f64)) -> Entity {
    Entity::Line(LineEntity {
        common: common(id),
        start_point: xyz(from.0, from.1),
        end_point: xyz(to.0, to.1),
    })
}

/// A 10 x 10 square at the origin.
fn square() -> Vec<Entity> {
    vec![
        line(0x1, (0.0, 0.0), (10.0, 0.0)),
        line(0x2, (10.0, 0.0), (10.0, 10.0)),
        line(0x3, (10.0, 10.0), (0.0, 10.0)),
        line(0x4, (0.0, 10.0), (0.0, 0.0)),
    ]
}

/// A dot-sized line at (x, y).
fn dot(id: u64, x: f64, y: f64) -> Entity {
    line(id, (x, y), (x + 1.0, y + 1.0))
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

fn options(crop: Crop) -> ToSvgOptions {
    ToSvgOptions {
        space: Space::All,
        padding: 0.0,
        crop,
        ..ToSvgOptions::default()
    }
}

fn guarded() -> Crop {
    Crop::Guarded { stated: None }
}

/// The square and a block reference to a 10-unit line scaled 3256 times --
/// the shape of the corrupt reference in LibreDWG's `example_2018.dwg`,
/// which made the drawing's picture 3.4 million units wide.
fn with_a_giant() -> CadDatabase {
    let mut entities = square();
    entities.push(Entity::Insert(InsertEntity {
        common: common(0x10),
        block_name: Ref::Resolved("BIG".to_string()),
        insertion_point: xyz(5.0, 5.0),
        scale: Point3D {
            x: 3256.0,
            y: 3256.0,
            z: 1.0,
        },
        rotation: 0.0,
        attribs: Vec::new(),
        extrusion: Point3D {
            x: 0.0,
            y: 0.0,
            z: 1.0,
        },
    }));
    db(
        entities,
        vec![("BIG", vec![line(0x11, (0.0, 0.0), (10.0, 10.0))])],
    )
}

#[test]
fn the_guard_leaves_a_giant_out_of_the_picture_and_names_it() {
    let db = with_a_giant();
    let result = to_svg(&db, options(guarded()));
    // The picture is the square, not the giant.
    assert_eq!(result.view_box, Rect::new(0.0, 0.0, 10.0, 10.0));
    assert_eq!(result.crop.content, Some(Rect::new(0.0, 0.0, 10.0, 10.0)));
    assert!(!result.crop.stated_taken);
    // It is named, with its extent and why...
    assert_eq!(result.crop.left_out.len(), 1, "{:?}", result.crop.left_out);
    let giant = &result.crop.left_out[0];
    assert_eq!(giant.id, EntityId::new(0x10));
    assert_eq!(giant.type_name, "INSERT");
    assert_eq!(giant.reason, LeftOutReason::ScaleOutlier);
    assert_eq!(giant.extent, Rect::new(5.0, 5.0, 32565.0, 32565.0));
    // ...and not drawn: clipped by the viewBox it would still cross the
    // whole picture.
    assert!(!result.svg.contains("<g transform"), "{}", result.svg);
    assert_eq!(result.svg.matches("<line ").count(), 4);

    // The scene keeps it, marked, for a caller that wants it anyway.
    let scene = Scene::new(&db, options(guarded()));
    let part = &scene.parts()[4];
    assert_eq!(part.left_out, Some(LeftOutReason::ScaleOutlier));
    assert!(part.drawn);
    let everything = scene.svg(scene.view_box, 0.1, |_| true);
    assert!(everything.contains("<g transform"), "{everything}");

    // Without the guard, the picture is the giant's.
    let all = to_svg(&db, options(Crop::Everything));
    assert_eq!(all.view_box, Rect::new(0.0, 0.0, 32565.0, 32565.0));
    assert!(all.crop.left_out.is_empty());
}

#[test]
fn the_guard_takes_a_stated_extent_that_holds_more() {
    // Twelve short lines along y = 0 and a dot a million units away: the
    // guard sets the dot aside...
    let mut entities: Vec<Entity> = (0..12u32)
        .map(|i| {
            let x = 10.0 * f64::from(i);
            line(0x100 + u64::from(i), (x, 0.0), (x + 8.0, 0.0))
        })
        .collect();
    entities.push(dot(0x200, 1e6, 1e6));
    let db = db(entities, Vec::new());
    let alone = to_svg(&db, options(guarded()));
    assert_eq!(alone.crop.left_out.len(), 1);
    assert_eq!(alone.crop.left_out[0].reason, LeftOutReason::FarOutlier);
    // A frame with no height is given a canvas one unit tall, below it.
    assert_eq!(alone.crop.content, Some(Rect::new(0.0, 0.0, 118.0, 0.0)));
    assert_eq!(alone.view_box, Rect::new(0.0, -1.0, 118.0, 0.0));

    // ...unless the caller states an extent -- a file header's, say --
    // that holds it and the rest: then that is the picture, and the dot is
    // in it.
    let stated = Rect::new(-1.0, -1.0, 1e6 + 2.0, 1e6 + 2.0);
    let told = to_svg(
        &db,
        options(Crop::Guarded {
            stated: Some(stated),
        }),
    );
    assert!(told.crop.stated_taken);
    assert_eq!(told.crop.content, Some(stated));
    assert_eq!(told.view_box, stated);
    assert!(told.crop.left_out.is_empty(), "{:?}", told.crop.left_out);

    // A stated extent that is not a rectangle is not taken.
    let nonsense = to_svg(
        &db,
        options(Crop::Guarded {
            stated: Some(Rect::new(f64::NAN, 0.0, 1.0, 1.0)),
        }),
    );
    assert!(!nonsense.crop.stated_taken);
    assert_eq!(nonsense.crop.left_out.len(), 1);
}

#[test]
fn a_window_frames_exactly_itself() {
    let mut entities = square();
    entities.push(line(0x5, (200.0, 0.0), (210.0, 0.0)));
    let window = Rect::new(0.0, 0.0, 100.0, 100.0);
    let result = to_svg(&db(entities, Vec::new()), options(Crop::Window(window)));
    assert_eq!(result.view_box, window);
    assert_eq!(result.crop.content, Some(window));
    assert!(
        result.svg.contains("viewBox=\"0 -100 100 100\""),
        "{}",
        &result.svg[..200]
    );
    // The line beyond the window is not in the picture, and says so; it is
    // still in the document, outside what the viewBox shows.
    assert_eq!(result.crop.left_out.len(), 1);
    assert_eq!(result.crop.left_out[0].id, EntityId::new(0x5));
    assert_eq!(result.crop.left_out[0].reason, LeftOutReason::OutsideView);
    assert!(result.svg.contains("x1=\"200\""), "{}", result.svg);
    // Padding still applies around a window.
    let padded = to_svg(
        &db(square(), Vec::new()),
        ToSvgOptions {
            padding: 2.0,
            ..options(Crop::Window(window))
        },
    );
    assert_eq!(padded.view_box, Rect::new(-2.0, -2.0, 102.0, 102.0));
}

#[test]
fn the_default_trim_names_what_it_leaves_outside_the_view() {
    // The square and a stray dot far away: the dominant cluster is the
    // square, and the dot -- still in the document, as it always was --
    // is outside the picture, and now said to be.
    let mut entities = square();
    entities.push(dot(0x9, 1e6, 1e6));
    let result = to_svg(
        &db(entities, Vec::new()),
        ToSvgOptions {
            space: Space::All,
            padding: 0.0,
            ..ToSvgOptions::default()
        },
    );
    assert_eq!(result.view_box, Rect::new(0.0, 0.0, 10.0, 10.0));
    assert_eq!(result.crop.left_out.len(), 1);
    assert_eq!(result.crop.left_out[0].id, EntityId::new(0x9));
    assert_eq!(result.crop.left_out[0].reason, LeftOutReason::OutsideView);
    assert!(result.svg.contains("x1=\"1000000\""), "{}", result.svg);
}

#[test]
fn a_construction_line_is_never_left_out() {
    // A RAY whose base point is a million units away, pointing back
    // across the square: its base point is no reason to stretch the
    // picture, and the ray itself is in it.
    let mut entities = square();
    entities.push(Entity::Ray(RayEntity {
        common: common(0x30),
        point: xyz(1e6, 5.0),
        vector: xyz(-1.0, 0.0),
    }));
    let result = to_svg(&db(entities, Vec::new()), options(guarded()));
    assert_eq!(result.view_box, Rect::new(0.0, 0.0, 10.0, 10.0));
    assert!(
        result.crop.left_out.is_empty(),
        "{:?}",
        result.crop.left_out
    );
    assert!(
        result.svg.contains("stroke-dasharray=\"4,2\""),
        "{}",
        result.svg
    );
}

#[test]
fn an_empty_drawing_frames_nothing() {
    let result = to_svg(
        &db(Vec::new(), Vec::new()),
        ToSvgOptions {
            padding: 5.0,
            ..options(guarded())
        },
    );
    assert_eq!(result.crop.content, None);
    assert!(result.crop.left_out.is_empty());
    assert_eq!(result.view_box, Rect::new(-5.0, -5.0, 5.0, 5.0));
}

#[test]
fn a_png_reports_the_crop_its_svg_does() {
    let db = with_a_giant();
    let svg = to_svg(&db, options(guarded()));
    let png = to_png(
        &db,
        ToPngOptions {
            svg: options(guarded()),
            ..ToPngOptions::default()
        },
    )
    .expect("renders");
    assert_eq!(png.crop, svg.crop);
    assert_eq!((png.view.left, png.view.top), (0.0, 10.0));
}

#[test]
fn a_sheet_is_framed_on_its_paper() {
    let g14: CadDatabase = serde_json::from_str(include_str!("golden/g14.expected.json"))
        .expect("the golden model deserializes");
    for crop in [Crop::Cluster, guarded(), Crop::Everything] {
        let result = layout_to_svg(
            &g14,
            "Layout1",
            ToSvgOptions {
                crop,
                ..ToSvgOptions::default()
            },
        )
        .expect("a sheet");
        let paper = Rect::new(0.0, 0.0, 420.0, 297.0);
        assert_eq!(result.crop.content, Some(paper));
        assert_eq!(result.view_box, paper);
        assert!(
            result.crop.left_out.is_empty(),
            "{:?}",
            result.crop.left_out
        );
    }
}
