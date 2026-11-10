//! A block reference with a grid (a multiple insert) draws its block in every
//! cell of it -- the spacing turned with the reference and not scaled by it --
//! and a grid with no cell draws nothing and says so.

mod common;

use std::collections::BTreeMap;

use iron_render_cad::{to_svg, Space, ToSvgOptions};
use uncad_model::model::{
    Entity, EntityCommon, InsertArray, InsertEntity, LineEntity, Point3D, Ref,
};
use uncad_model::tables::Tables;
use uncad_model::{CadDatabase, ReadDiagnostics};

fn common(id: u64) -> EntityCommon {
    common::common_on(id, "0", 7)
}

fn xyz(x: f64, y: f64) -> Point3D {
    Point3D { x, y, z: 0.0 }
}

fn insert(id: u64, block_name: Ref<String>) -> Entity {
    Entity::Insert(InsertEntity {
        common: common(id),
        block_name,
        insertion_point: xyz(0.0, 0.0),
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
        array: None,
    })
}

fn drawing(entities: Vec<Entity>) -> CadDatabase {
    let mut block_records = BTreeMap::new();
    block_records.insert(
        "DOOR".to_string(),
        common::block_record(
            "DOOR",
            vec![Entity::Line(LineEntity {
                common: common(0x70),
                start_point: xyz(0.0, 0.0),
                end_point: xyz(1.0, 1.0),
            })],
        ),
    );
    block_records.insert(
        "TWICE".to_string(),
        common::block_record(
            "TWICE",
            vec![insert(0x71, Ref::Resolved("NOWHERE".to_string()))],
        ),
    );
    block_records.insert(
        "EMPTY".to_string(),
        common::block_record("EMPTY", Vec::new()),
    );
    CadDatabase {
        entities,
        tables: Tables {
            block_records,
            ..Tables::default()
        },
        header: Default::default(),
        read_diagnostics: ReadDiagnostics::default(),
    }
}

fn all() -> ToSvgOptions {
    ToSvgOptions {
        space: Space::All,
        ..ToSvgOptions::default()
    }
}

fn gridded(id: u64, columns: u32, rows: u32, column_spacing: f64, row_spacing: f64) -> Entity {
    let Entity::Insert(mut i) = insert(id, Ref::Resolved("DOOR".to_string())) else {
        unreachable!()
    };
    i.array = Some(InsertArray {
        columns,
        rows,
        column_spacing,
        row_spacing,
    });
    Entity::Insert(i)
}

/// The width of the picture, from its `viewBox`.
fn view_width(svg: &str) -> f64 {
    let at = svg.find("viewBox=\"").expect("a viewBox") + "viewBox=\"".len();
    let numbers: Vec<f64> = svg[at..]
        .split('"')
        .next()
        .unwrap()
        .split_whitespace()
        .map(|n| n.parse().unwrap())
        .collect();
    numbers[2]
}

#[test]
fn every_cell_of_a_grid_draws_the_block() {
    let one = to_svg(&drawing(vec![gridded(1, 1, 1, 0.0, 0.0)]), all());
    let grid = to_svg(&drawing(vec![gridded(1, 3, 2, 100.0, 50.0)]), all());
    assert_eq!(one.svg.matches("<line ").count(), 1);
    assert_eq!(grid.svg.matches("<line ").count(), 6);
    // The picture grows to hold the cells: two spacings of 100 more across.
    assert!(
        view_width(&grid.svg) > view_width(&one.svg) + 190.0,
        "{} vs {}",
        view_width(&grid.svg),
        view_width(&one.svg)
    );
}

#[test]
fn a_grid_with_no_cell_draws_nothing_and_is_named() {
    let result = to_svg(&drawing(vec![gridded(1, 0, 2, 5.0, 5.0)]), all());
    assert_eq!(result.svg.matches("<line ").count(), 0);
    assert!(
        result
            .unsupported_types
            .iter()
            .any(|u| u == "INSERT_EMPTY_GRID"),
        "{:?}",
        result.unsupported_types
    );
}

#[test]
fn a_grid_that_is_not_a_number_is_a_reference_that_is_not_drawn() {
    let result = to_svg(&drawing(vec![gridded(1, 2, 1, f64::NAN, 0.0)]), all());
    assert_eq!(result.svg.matches("<line ").count(), 0);
}
