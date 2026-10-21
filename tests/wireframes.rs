//! A REGION, 3DSOLID or POLYLINE_PFACE that is flat in a plane parallel to
//! XY is drawn where the file puts it; only a body with depth gets the
//! isometric view.

mod common;

use iron_render_cad::{to_svg, Space, ToSvgOptions};
use uncad_model::model::{Entity, Point3D, Solid3DEntity};
use uncad_model::tables::Tables;
use uncad_model::{CadDatabase, ReadDiagnostics};

fn p3(x: f64, y: f64, z: f64) -> Point3D {
    Point3D { x, y, z }
}

fn region(edges: Vec<[Point3D; 2]>) -> CadDatabase {
    CadDatabase {
        entities: vec![Entity::Region(Solid3DEntity {
            common: common::common_on(0x10, "0", 7),
            wireframe_edges: edges,
            skipped_edges: 0,
        })],
        tables: Tables::default(),
        header: Default::default(),
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

#[test]
fn a_solid_with_no_edge_is_reported_by_id_not_as_an_unsupported_type() {
    // A whole torus is one face that closes on itself: a body read in full
    // with no edge to draw. Beside it, a solid that has edges is drawn.
    let mut db = region(vec![[p3(0.0, 0.0, 0.0), p3(10.0, 0.0, 0.0)]]);
    db.entities.push(Entity::Solid3D(Solid3DEntity {
        common: common::common_on(0x20, "0", 7),
        wireframe_edges: vec![],
        skipped_edges: 0,
    }));
    db.entities.push(Entity::Solid3D(Solid3DEntity {
        common: common::common_on(0x30, "0", 7),
        wireframe_edges: vec![[p3(0.0, 5.0, 0.0), p3(10.0, 5.0, 0.0)]],
        skipped_edges: 0,
    }));
    let r = to_svg(
        &db,
        ToSvgOptions {
            space: Space::All,
            ..ToSvgOptions::default()
        },
    );
    assert!(r.unsupported_types.is_empty(), "{:?}", r.unsupported_types);
    let ids: Vec<u64> = r.edgeless.iter().map(|id| id.value()).collect();
    assert_eq!(ids, vec![0x20]);
    assert_eq!(r.svg.matches("<line").count(), 2, "{}", r.svg);
}
