//! An attribute with no text draws nothing, but its place still counts
//! towards the picture's extent -- the place it has in the world, not in
//! its own plane. A mirror copy (an attribute written in a plane facing
//! down, extrusion (0, 0, -1)) has its own x axis pointing the other way.

use iron_render_cad::{to_svg, Crop, Space, ToSvgOptions};
use uncad_model::model::{
    AttribEntity, CircleEntity, Confidence, Entity, EntityCommon, EntityId, Origin, Point2D,
    Point3D, Ref,
};
use uncad_model::tables::Tables;
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

fn up(z: f64) -> Point3D {
    Point3D { x: 0.0, y: 0.0, z }
}

fn empty_attrib(at: Point2D, z: f64) -> Entity {
    Entity::Attrib(AttribEntity {
        common: common(0x60),
        start_point: at,
        text_height: 2.5,
        tag: "TAG".to_string(),
        text: String::new(),
        rotation: 0.0,
        flags: Default::default(),
        horizontal_justification: Default::default(),
        vertical_justification: Default::default(),
        alignment_point: None,
        width_factor: 1.0,
        oblique_angle: 0.0,
        style_name: Ref::Absent,
        elevation: 0.0,
        extrusion: up(z),
    })
}

/// The viewBox's `(min_x, min_y, width, height)`.
fn view_box(db: &CadDatabase) -> [f64; 4] {
    let svg = to_svg(
        db,
        ToSvgOptions {
            space: Space::All,
            crop: Crop::Everything,
            padding: 0.0,
            ..ToSvgOptions::default()
        },
    )
    .svg;
    let at = svg.find("viewBox=\"").expect("a viewBox") + "viewBox=\"".len();
    let end = at + svg[at..].find('"').unwrap();
    let v: Vec<f64> = svg[at..end]
        .split_whitespace()
        .map(|n| n.parse().unwrap())
        .collect();
    [v[0], v[1], v[2], v[3]]
}

fn drawing(attrib: Entity) -> CadDatabase {
    CadDatabase {
        entities: vec![
            Entity::Circle(CircleEntity {
                common: common(0x10),
                center: Point3D {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                radius: 1.0,
                extrusion: up(1.0),
            }),
            attrib,
        ],
        tables: Tables::default(),
        read_diagnostics: ReadDiagnostics::default(),
    }
}

#[test]
fn an_empty_attribute_in_a_mirrored_plane_counts_where_it_is_drawn() {
    // In its own plane at x = 10; that plane's x axis points to the world's
    // -x, so in the world it is at x = -10.
    let mirrored = view_box(&drawing(empty_attrib(Point2D { x: 10.0, y: 5.0 }, -1.0)));
    let upright = view_box(&drawing(empty_attrib(Point2D { x: 10.0, y: 5.0 }, 1.0)));
    let (lo, hi) = (mirrored[0], mirrored[0] + mirrored[2]);
    assert!(
        lo <= -10.0 + 1e-9,
        "the mirrored anchor is at x = -10: {mirrored:?}"
    );
    assert!(hi < 5.0, "and nothing reaches x = +10: {mirrored:?}");
    let (lo, hi) = (upright[0], upright[0] + upright[2]);
    assert!(
        hi >= 10.0 - 1e-9 && lo > -5.0,
        "upright: at x = +10: {upright:?}"
    );
}
