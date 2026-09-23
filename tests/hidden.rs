//! What the drawing hides is not drawn, and is counted: an entity marked
//! invisible, an attribute whose own flag hides it, anything on the
//! DEFPOINTS layer, and anything on a layer that is off, frozen or stated
//! not to plot -- a layer whose file does not say whether it plots, plots.
//! Asked to, the renderer draws them at half opacity instead. Either way
//! they never stretch the picture.

use std::collections::BTreeMap;

use iron_render_cad::{to_png, to_svg, Crop, Space, ToPngOptions, ToSvgOptions, ToSvgResult};
use uncad_model::model::{
    Confidence, Entity, EntityCommon, EntityId, InsertEntity, LineEntity, Origin, Point3D, Ref,
};
use uncad_model::tables::{BlockRecord, LayerRecord, Tables};
use uncad_model::{CadDatabase, ReadDiagnostics};

fn options(space: Space, include_hidden: bool) -> ToSvgOptions {
    ToSvgOptions {
        space,
        padding: 0.0,
        crop: Crop::Everything,
        include_hidden,
        ..ToSvgOptions::default()
    }
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

fn g14() -> CadDatabase {
    serde_json::from_str(include_str!("golden/g14.expected.json"))
        .expect("the golden model deserializes")
}

#[test]
fn g14s_model_leaves_out_the_lines_on_the_off_frozen_and_non_plotting_layers() {
    // One line per layer state, at y = 0 (WALLS), 10 (off), 20 (frozen),
    // 30 (locked), 40 (not plotted), 50 (plotted) and 60 (a lineweight).
    let db = g14();
    let shown = to_svg(&db, options(Space::Model, false));
    let ys = |r: &ToSvgResult| -> Vec<String> {
        r.svg
            .split("<line x1=\"0\" y1=\"")
            .skip(1)
            .map(|l| l[..l.find('"').unwrap()].to_string())
            .collect()
    };
    assert_eq!(ys(&shown), ["0", "-30", "-50", "-60"], "{}", shown.svg);
    assert_eq!(shown.hidden, 3);
    assert!(!shown.svg.contains("opacity"), "{}", shown.svg);

    let faded = to_svg(&db, options(Space::Model, true));
    assert_eq!(ys(&faded).len(), 7, "{}", faded.svg);
    assert_eq!(faded.svg.matches("<g opacity=\"0.5\">").count(), 3);
    assert_eq!(faded.hidden, 3);
    for y in ["-10", "-20", "-40"] {
        assert!(
            faded
                .svg
                .contains(&format!("<g opacity=\"0.5\"><line x1=\"0\" y1=\"{y}\"")),
            "{y}: {}",
            faded.svg
        );
    }
}

#[test]
fn g13s_invisible_attribute_is_hidden_and_its_visible_one_is_not() {
    let db: CadDatabase = serde_json::from_str(include_str!("golden/g13.expected.json"))
        .expect("the golden model deserializes");
    let shown = to_svg(&db, options(Space::All, false));
    assert!(shown.svg.contains(">D-101</text>"), "{}", shown.svg);
    assert!(!shown.svg.contains("FIRE RATED"), "{}", shown.svg);
    assert_eq!(shown.hidden, 1);
    let faded = to_svg(&db, options(Space::All, true));
    assert!(
        faded.svg.contains(">FIRE RATED</text></g>"),
        "{}",
        faded.svg
    );
}

fn common(id: u64, layer: &str) -> EntityCommon {
    EntityCommon {
        id: EntityId::new(id),
        origin: Origin::Vector,
        confidence: Confidence::High,
        source_handle: Ref::Resolved(format!("{id:X}")),
        layer: Ref::Resolved(layer.to_string()),
        color_index: 256,
        true_color: None,
        invisible: false,
    }
}

fn line(id: u64, layer: &str, from: (f64, f64), to: (f64, f64)) -> Entity {
    Entity::Line(LineEntity {
        common: common(id, layer),
        start_point: Point3D {
            x: from.0,
            y: from.1,
            z: 0.0,
        },
        end_point: Point3D {
            x: to.0,
            y: to.1,
            z: 0.0,
        },
    })
}

fn insert(id: u64, layer: &str, block: &str) -> Entity {
    Entity::Insert(InsertEntity {
        common: common(id, layer),
        block_name: Ref::Resolved(block.to_string()),
        insertion_point: Point3D {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        },
        scale: Point3D {
            x: 1.0,
            y: 1.0,
            z: 1.0,
        },
        rotation: 0.0,
        attribs: Vec::new(),
        extrusion: Point3D {
            x: 0.0,
            y: 0.0,
            z: 1.0,
        },
    })
}

fn layer(name: &str, off: bool, frozen: bool) -> (String, LayerRecord) {
    (
        name.to_string(),
        LayerRecord {
            name: name.to_string(),
            color_index: 7,
            off,
            frozen,
            locked: false,
            plot: None,
            lineweight: None,
            linetype: Ref::Absent,
        },
    )
}

fn db(entities: Vec<Entity>, blocks: Vec<(&str, Vec<Entity>)>) -> CadDatabase {
    CadDatabase {
        entities,
        tables: Tables {
            layers: BTreeMap::from([
                layer("0", true, false),
                layer("SHOWN", false, false),
                layer("OFF", true, false),
                layer("FROZEN", false, true),
            ]),
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
                .collect(),
            ..Tables::default()
        },
        read_diagnostics: ReadDiagnostics::default(),
    }
}

#[test]
fn a_hidden_entity_does_not_stretch_the_picture_drawn_faded_or_not() {
    let mut invisible = line(0x12, "SHOWN", (0.0, 0.0), (500.0, 500.0));
    invisible.common_mut().invisible = true;
    let drawing = db(
        vec![
            line(0x10, "SHOWN", (0.0, 0.0), (10.0, 10.0)),
            line(0x11, "defpoints", (0.0, 0.0), (-500.0, -500.0)),
            invisible,
        ],
        Vec::new(),
    );
    for include_hidden in [false, true] {
        let result = to_svg(&drawing, options(Space::All, include_hidden));
        assert_eq!(result.hidden, 2);
        assert_eq!(extent(&result.svg), [0.0, 0.0, 10.0, 10.0]);
    }
}

#[test]
fn inside_a_block_each_entity_is_hidden_by_its_own_layer_and_layer_zero_by_the_references() {
    // Layer 0 is off, but the block's layer-0 line takes the reference's
    // layer (SHOWN) and is drawn; its line on the frozen layer is not.
    let drawing = db(
        vec![insert(0x20, "SHOWN", "B")],
        vec![(
            "B",
            vec![
                line(0x21, "0", (0.0, 0.0), (10.0, 0.0)),
                line(0x22, "FROZEN", (0.0, 5.0), (10.0, 5.0)),
            ],
        )],
    );
    let result = to_svg(&drawing, options(Space::All, false));
    assert_eq!(result.svg.matches("<line ").count(), 1, "{}", result.svg);
    assert!(result.svg.contains("y1=\"0\""), "{}", result.svg);
    assert_eq!(result.hidden, 1);
    // A reference on a layer that is off hides all it draws: its contents
    // are not visited, so they are not counted, and its block drew nothing.
    let drawing = db(
        vec![insert(0x20, "OFF", "B")],
        vec![("B", vec![line(0x21, "SHOWN", (0.0, 0.0), (10.0, 0.0))])],
    );
    let result = to_svg(&drawing, options(Space::All, false));
    assert!(!result.svg.contains("<line"), "{}", result.svg);
    assert_eq!(result.hidden, 1);
}

#[test]
fn a_png_reports_the_same_count() {
    let png = to_png(
        &g14(),
        ToPngOptions {
            svg: options(Space::Model, false),
            ..ToPngOptions::default()
        },
    )
    .expect("G14's model rasterizes");
    assert_eq!(png.hidden, 3);
}
