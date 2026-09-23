//! Render once, assemble many: a [`Scene`] keeps a render's walk as one
//! part per top-level entity -- which entity, what box of the world it
//! covers -- and writes a document for any window from any subset of them.
//! A tiled viewer, a printer splitting a plan over pages or a differ
//! showing one entity at a time walks the drawing once instead of once per
//! picture.

use std::collections::BTreeMap;

use iron_render_cad::{layout_to_svg, to_svg, Hidden, Part, Rect, Scene, Space, ToSvgOptions};
use uncad_model::model::{
    Confidence, Entity, EntityCommon, EntityId, LineEntity, Origin, Point3D, RayEntity, Ref,
};
use uncad_model::tables::{LayerRecord, Tables};
use uncad_model::{CadDatabase, ReadDiagnostics};

fn common(id: u64, layer: &str) -> EntityCommon {
    EntityCommon {
        id: EntityId::new(id),
        origin: Origin::Vector,
        confidence: Confidence::High,
        source_handle: Ref::Resolved(format!("{id:X}")),
        layer: Ref::Resolved(layer.to_string()),
        color_index: 7,
        true_color: None,
        invisible: false,
    }
}

fn xyz(x: f64, y: f64) -> Point3D {
    Point3D { x, y, z: 0.0 }
}

fn line(id: u64, from: (f64, f64), to: (f64, f64)) -> Entity {
    line_on(id, "0", from, to)
}

fn line_on(id: u64, layer: &str, from: (f64, f64), to: (f64, f64)) -> Entity {
    Entity::Line(LineEntity {
        common: common(id, layer),
        start_point: xyz(from.0, from.1),
        end_point: xyz(to.0, to.1),
    })
}

fn xline(id: u64, at: (f64, f64), dir: (f64, f64)) -> Entity {
    Entity::XLine(RayEntity {
        common: common(id, "0"),
        point: xyz(at.0, at.1),
        vector: xyz(dir.0, dir.1),
    })
}

fn db(entities: Vec<Entity>) -> CadDatabase {
    let frozen = LayerRecord {
        name: "FROZEN".to_string(),
        color_index: 7,
        off: false,
        frozen: true,
        locked: false,
        plot: None,
        lineweight: None,
        linetype: Ref::Absent,
    };
    CadDatabase {
        entities,
        tables: Tables {
            layers: BTreeMap::from([("FROZEN".to_string(), frozen)]),
            ..Tables::default()
        },
        read_diagnostics: ReadDiagnostics::default(),
    }
}

fn all() -> ToSvgOptions {
    ToSvgOptions {
        space: Space::All,
        padding: 0.0,
        outlier_trim: false,
        ..ToSvgOptions::default()
    }
}

/// A 10 x 10 square.
fn square() -> Vec<Entity> {
    vec![
        line(0x1, (0.0, 0.0), (10.0, 0.0)),
        line(0x2, (10.0, 0.0), (10.0, 10.0)),
        line(0x3, (10.0, 10.0), (0.0, 10.0)),
        line(0x4, (0.0, 10.0), (0.0, 0.0)),
    ]
}

/// The four numbers of the document's `viewBox`.
fn view_box(svg: &str) -> Vec<f64> {
    let at = svg.find("viewBox=\"").unwrap() + "viewBox=\"".len();
    svg[at..at + svg[at..].find('"').unwrap()]
        .split(' ')
        .map(|v| v.parse().unwrap())
        .collect()
}

#[test]
fn every_top_level_entity_is_a_part_in_drawing_order() {
    let mut entities = square();
    entities.push(line_on(0x5, "FROZEN", (20.0, 0.0), (30.0, 0.0)));
    entities.push(Entity::Unknown {
        common: common(0x6, "0"),
        type_name: "ACAD_PROXY_ENTITY".to_string(),
    });
    let scene = Scene::new(&db(entities), all());
    let parts = scene.parts();
    let ids: Vec<u64> = parts.iter().map(|p| p.id.value()).collect();
    assert_eq!(ids, [1, 2, 3, 4, 5, 6]);
    assert_eq!(parts[0].type_name, "LINE");
    assert_eq!(parts[0].extent, Some(Rect::new(0.0, 0.0, 10.0, 0.0)));
    assert!(parts[0].drawn && !parts[0].unbounded && parts[0].hidden.is_none());
    // The frozen line: hidden, not drawn, no extent.
    assert_eq!(parts[4].hidden, Some(Hidden::LayerFrozen));
    assert!(!parts[4].drawn);
    assert_eq!(parts[4].extent, None);
    // An entity with nothing to draw is still a part, and says so.
    assert_eq!(parts[5].type_name, "ACAD_PROXY_ENTITY");
    assert!(!parts[5].drawn && parts[5].extent.is_none());
    assert!(parts.iter().all(|p| !p.through_viewport));
    assert_eq!(scene.hidden, 1);
    assert_eq!(scene.unsupported_types, ["ACAD_PROXY_ENTITY"]);
    // The view box is the square, unpadded, in world units.
    assert_eq!(scene.view_box, Rect::new(0.0, 0.0, 10.0, 10.0));
}

#[test]
fn the_whole_scene_is_to_svgs_document_byte_for_byte() {
    let g1: CadDatabase = serde_json::from_str(include_str!("golden/g1.expected.json"))
        .expect("the golden model deserializes");
    // Far from the world origin too, where the document is written about a
    // point of the drawing's own.
    let far = db(square()
        .into_iter()
        .map(|e| match e {
            Entity::Line(mut l) => {
                l.start_point.x += 2.5e8;
                l.end_point.x += 2.5e8;
                Entity::Line(l)
            }
            other => other,
        })
        .collect());
    for (db, options) in [(&g1, ToSvgOptions::default()), (&far, all())] {
        let scene = Scene::new(db, options);
        let result = to_svg(db, options);
        assert_eq!(
            scene.svg(scene.view_box, scene.auto_stroke_width, |_| true),
            result.svg
        );
        assert_eq!(scene.view_box, result.view_box);
        assert_eq!(scene.origin, result.origin);
        assert_eq!(scene.hidden, result.hidden);
        assert_eq!(scene.unsupported_types, result.unsupported_types);
    }
}

#[test]
fn the_view_box_is_the_documents_viewbox_in_world_units() {
    let result = to_svg(
        &db(square()),
        ToSvgOptions {
            padding: 1.0,
            ..all()
        },
    );
    assert_eq!(view_box(&result.svg), [-1.0, -11.0, 12.0, 12.0]);
    assert_eq!(result.view_box, Rect::new(-1.0, -1.0, 11.0, 11.0));
}

#[test]
fn a_window_holds_only_the_parts_kept_and_shows_only_itself() {
    let mut entities = square();
    entities.push(line(0x10, (100.0, 0.0), (110.0, 5.0)));
    let scene = Scene::new(&db(entities), all());
    let window = Rect::new(99.0, -1.0, 111.0, 6.0);
    let svg = scene.svg(window, 0.5, |p: &Part| {
        p.extent.is_some_and(|e| e.intersects(&window))
    });
    assert_eq!(view_box(&svg), [99.0, -6.0, 12.0, 7.0]);
    assert_eq!(svg.matches("<line ").count(), 1, "{svg}");
    assert!(
        svg.contains("x1=\"100\" y1=\"0\" x2=\"110\" y2=\"-5\""),
        "{svg}"
    );
    assert!(svg.contains("stroke-width=\"0.5\""), "{svg}");
    // Nothing kept is an empty picture of the window, not an error.
    let empty = scene.svg(window, 0.5, |_| false);
    assert!(!empty.contains("<line"), "{empty}");
    assert_eq!(view_box(&empty), [99.0, -6.0, 12.0, 7.0]);
}

#[test]
fn a_construction_line_is_cut_to_every_window_it_is_written_for() {
    let mut entities = square();
    // Horizontal through (5, 5), both ways. Its extent is its base point.
    entities.push(xline(0x20, (5.0, 5.0), (1.0, 0.0)));
    let scene = Scene::new(&db(entities), all());
    let xl = scene.parts().iter().find(|p| p.id.value() == 0x20).unwrap();
    assert!(xl.unbounded);
    assert_eq!(xl.extent, Some(Rect::new(5.0, 5.0, 5.0, 5.0)));
    // A window far to the right, which the base point is not in: kept
    // because it is unbounded, and cut to this window, not the scene's.
    let window = Rect::new(100.0, 0.0, 120.0, 10.0);
    let svg = scene.svg(window, 0.0, |p: &Part| {
        p.unbounded || p.extent.is_some_and(|e| e.intersects(&window))
    });
    assert!(!svg.contains("@@"), "no placeholder is left: {svg}");
    let at = svg.find("<line ").expect("the xline is drawn");
    let el = &svg[at..];
    let attr = |name: &str| -> f64 {
        let at = el.find(&format!("{name}=\"")).unwrap() + name.len() + 2;
        el[at..at + el[at..].find('"').unwrap()].parse().unwrap()
    };
    assert_eq!((attr("y1"), attr("y2")), (-5.0, -5.0));
    assert!(attr("x1") < 100.0 && attr("x1") > 99.0, "{}", attr("x1"));
    assert!(attr("x2") > 120.0 && attr("x2") < 121.0, "{}", attr("x2"));
}

#[test]
fn a_far_away_drawing_writes_every_window_about_its_own_origin() {
    let far: Vec<Entity> = square()
        .into_iter()
        .map(|e| match e {
            Entity::Line(mut l) => {
                l.start_point.x += 1.0e6;
                l.end_point.x += 1.0e6;
                l.start_point.y += 2.0e6;
                l.end_point.y += 2.0e6;
                Entity::Line(l)
            }
            other => other,
        })
        .collect();
    let scene = Scene::new(&db(far), all());
    let (ox, oy) = (scene.origin.x, scene.origin.y);
    assert!(ox > 32768.0 && oy > 32768.0, "{:?}", scene.origin);
    // Extents stay in world units.
    assert_eq!(
        scene.parts()[0].extent,
        Some(Rect::new(1.0e6, 2.0e6, 1.0e6 + 10.0, 2.0e6))
    );
    let window = Rect::new(1.0e6 + 2.0, 2.0e6 + 3.0, 1.0e6 + 6.0, 2.0e6 + 9.0);
    let svg = scene.svg(window, 0.1, |_| true);
    assert_eq!(
        view_box(&svg),
        [1.0e6 + 2.0 - ox, oy - (2.0e6 + 9.0), 4.0, 6.0]
    );
}

#[test]
fn a_sheet_is_its_own_entities_then_the_model_through_each_viewport() {
    let g14: CadDatabase = serde_json::from_str(include_str!("golden/g14.expected.json"))
        .expect("the golden model deserializes");
    let scene = Scene::layout(&g14, "Layout1", ToSvgOptions::default()).expect("a sheet");
    let parts = scene.parts();
    let kinds: Vec<(u64, &str, bool)> = parts
        .iter()
        .map(|p| (p.id.value(), p.type_name.as_str(), p.through_viewport))
        .collect();
    // The border, the title and the three viewport frames, then the model
    // through the detail viewport -- the overall one is the sheet itself
    // and the third is off.
    assert_eq!(
        kinds,
        [
            (269, "LWPOLYLINE", false),
            (270, "TEXT", false),
            (271, "VIEWPORT", false),
            (272, "VIEWPORT", false),
            (273, "VIEWPORT", false),
            (272, "VIEWPORT", true),
        ]
    );
    // Its extent is the viewport's frame on the paper.
    assert_eq!(parts[5].extent, Some(Rect::new(50.0, 75.0, 250.0, 225.0)));
    // The sheet is the layout's limits, and the whole scene is what
    // layout_to_svg writes.
    assert_eq!(scene.view_box, Rect::new(0.0, 0.0, 420.0, 297.0));
    let result = layout_to_svg(&g14, "Layout1", ToSvgOptions::default()).expect("a sheet");
    assert_eq!(
        scene.svg(scene.view_box, scene.auto_stroke_width, |_| true),
        result.svg
    );
    assert_eq!(result.view_box, scene.view_box);
    // The paper alone: the sheet without the model.
    let paper = scene.svg(scene.view_box, scene.auto_stroke_width, |p| {
        !p.through_viewport
    });
    assert!(!paper.contains("clip-path=\"url("), "{paper}");
    assert!(paper.contains(">SHEET 1</text>"), "{paper}");
}
