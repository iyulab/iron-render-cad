//! A REGION, 3DSOLID or POLYLINE_PFACE that is flat in a plane parallel to
//! XY is drawn where the file puts it; only a body with depth gets the
//! isometric view.

use iron_render_cad::{to_svg, Space, ToSvgOptions};
use uncad_model::model::{
    Confidence, Entity, EntityCommon, EntityId, Origin, Point3D, Ref, Solid3DEntity,
};
use uncad_model::tables::Tables;
use uncad_model::{CadDatabase, ReadDiagnostics};

fn p3(x: f64, y: f64, z: f64) -> Point3D {
    Point3D { x, y, z }
}

fn region(edges: Vec<[Point3D; 2]>) -> CadDatabase {
    CadDatabase {
        entities: vec![Entity::Region(Solid3DEntity {
            common: EntityCommon {
                id: EntityId::new(0x10),
                origin: Origin::Vector,
                confidence: Confidence::High,
                source_handle: Ref::Resolved("10".to_string()),
                layer: Ref::Resolved("0".to_string()),
                color_index: 7,
                true_color: None,
                invisible: false,
                linetype: uncad_model::model::EntityLinetype::ByLayer,
                linetype_scale: 1.0,
                lineweight: Some(-1),
                transparency: Some(0),
            },
            wireframe_edges: edges,
            skipped_edges: 0,
        })],
        tables: Tables::default(),
        read_diagnostics: ReadDiagnostics::default(),
    }
}

fn svg(db: &CadDatabase) -> String {
    to_svg(
        db,
        ToSvgOptions {
            space: Space::All,
            padding: 0.0,
            ..ToSvgOptions::default()
        },
    )
    .svg
}

#[test]
fn a_flat_region_is_drawn_where_the_file_puts_it() {
    // The world square (10,10)-(20,20) at z = 0. Through the isometric map
    // (x, y, 0) -> (x cos30, y + x sin30) its corners would land at
    // (8.66, 15) .. (17.32, 30): the wrong size, ten units off. The expected
    // numbers are the file's own, y flipped.
    let db = region(vec![
        [p3(10.0, 10.0, 0.0), p3(20.0, 10.0, 0.0)],
        [p3(20.0, 10.0, 0.0), p3(20.0, 20.0, 0.0)],
        [p3(20.0, 20.0, 0.0), p3(10.0, 20.0, 0.0)],
        [p3(10.0, 20.0, 0.0), p3(10.0, 10.0, 0.0)],
    ]);
    let out = svg(&db);
    assert!(
        out.contains("x1=\"10\" y1=\"-10\" x2=\"20\" y2=\"-10\""),
        "{out}"
    );
    // And the extent is the square's: viewBox 10, -20, 10 x 10.
    assert!(out.contains("viewBox=\"10 -20 10 10\""), "{out}");
}

#[test]
fn a_body_with_depth_still_gets_the_isometric_view() {
    // One edge out of the plane: there is no plan view that shows the body,
    // so the approximation stays. (10, 10, 0) -> (10 cos30, 10 + 10 sin30).
    let db = region(vec![
        [p3(10.0, 10.0, 0.0), p3(20.0, 10.0, 0.0)],
        [p3(10.0, 10.0, 0.0), p3(10.0, 10.0, 5.0)],
    ]);
    let out = svg(&db);
    let x = 10.0 * (std::f64::consts::PI / 6.0).cos();
    assert!(out.contains(&format!("x1=\"{x}\" y1=\"-15\"")), "{out}");
}
