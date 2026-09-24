//! What an MTEXT counts towards the picture's extent is its block, not its
//! insertion point alone: as big as the file says it is -- the extents the
//! writing application measured (DXF 42/43), else the reference rectangle
//! it wraps to (DXF 41) -- or, when the file says nothing, as wide as its
//! longest line is estimated to be and as tall as its lines are drawn; hung
//! from the insertion point by its attachment and turned by its rotation.
//!
//! The expected numbers are worked out from the attachment rules and the
//! renderer's stated assumptions (0.6 em per character, capitals 0.7 of
//! the em, lines 5/3 of the height apart), not read off this crate's
//! output.

use std::f64::consts::FRAC_PI_2;

use iron_render_cad::{to_svg, Crop, Space, ToSvgOptions};
use uncad_model::model::{
    Confidence, Entity, EntityCommon, EntityId, MTextAttachment, MTextEntity, Origin, Point3D, Ref,
};
use uncad_model::tables::Tables;
use uncad_model::{CadDatabase, ReadDiagnostics};

struct MText {
    text: &'static str,
    attachment: Option<MTextAttachment>,
    rotation: f64,
    rect_width: f64,
    extents: (Option<f64>, Option<f64>),
}

impl Default for MText {
    fn default() -> Self {
        MText {
            text: "ABCD",
            attachment: Some(MTextAttachment::TopLeft),
            rotation: 0.0,
            rect_width: 0.0,
            extents: (None, None),
        }
    }
}

/// The extent an MTEXT of height 10 at (10, 20) is given, as
/// `[min_x, min_y, max_x, max_y]`.
fn extent(m: MText) -> [f64; 4] {
    let entity = Entity::MText(MTextEntity {
        common: EntityCommon {
            id: EntityId::new(0x11),
            origin: Origin::Vector,
            confidence: Confidence::High,
            source_handle: Ref::Resolved("11".to_string()),
            layer: Ref::Resolved("0".to_string()),
            color_index: 7,
            true_color: None,
            invisible: false,
            linetype: uncad_model::model::EntityLinetype::ByLayer,
            linetype_scale: 1.0,
            lineweight: Some(-1),
            transparency: Some(0),
        },
        insertion_point: Point3D {
            x: 10.0,
            y: 20.0,
            z: 0.0,
        },
        text: m.text.to_string(),
        text_height: 10.0,
        rotation: m.rotation,
        line_spacing_factor: 1.0,
        attachment: m.attachment,
        reference_width: m.rect_width,
        extents_width: m.extents.0,
        extents_height: m.extents.1,
        style_name: Ref::Absent,
    });
    let svg = to_svg(
        &CadDatabase {
            entities: vec![entity],
            tables: Tables::default(),
            read_diagnostics: ReadDiagnostics::default(),
        },
        ToSvgOptions {
            space: Space::All,
            padding: 0.0,
            crop: Crop::Everything,
            ..ToSvgOptions::default()
        },
    )
    .svg;
    let start = svg.find("viewBox=\"").unwrap() + "viewBox=\"".len();
    let end = svg[start..].find('"').unwrap() + start;
    let v: Vec<f64> = svg[start..end]
        .split_whitespace()
        .map(|n| n.parse().unwrap())
        .collect();
    [v[0], -v[1] - v[3], v[0] + v[2], -v[1]]
}

fn close(actual: [f64; 4], expected: [f64; 4]) {
    for (a, e) in actual.iter().zip(expected) {
        assert!((a - e).abs() < 1e-9, "{actual:?} vs {expected:?}");
    }
}

#[test]
fn a_block_the_file_measured_is_that_big() {
    // Top-left: the block hangs below and to the right of the point.
    close(
        extent(MText {
            extents: (Some(30.0), Some(8.0)),
            ..MText::default()
        }),
        [10.0, 12.0, 40.0, 20.0],
    );
    // Bottom-right: it stands above and to the left.
    close(
        extent(MText {
            attachment: Some(MTextAttachment::BottomRight),
            extents: (Some(30.0), Some(8.0)),
            ..MText::default()
        }),
        [-20.0, 20.0, 10.0, 28.0],
    );
}

#[test]
fn a_block_without_extents_is_as_wide_as_its_reference_rectangle() {
    // One line, so as tall as its capitals: 10.
    close(
        extent(MText {
            rect_width: 50.0,
            ..MText::default()
        }),
        [10.0, 10.0, 60.0, 20.0],
    );
}

#[test]
fn a_block_that_states_nothing_is_estimated_from_its_lines() {
    // Four characters at 0.6 em of an em 10 / 0.7 tall, centred both ways.
    let width = 4.0 * 0.6 * 10.0 / 0.7;
    close(
        extent(MText {
            attachment: Some(MTextAttachment::MiddleCenter),
            ..MText::default()
        }),
        [10.0 - width / 2.0, 15.0, 10.0 + width / 2.0, 25.0],
    );
    // No attachment stated: the first baseline is on the point, so the
    // block reaches one height above it and, two lines being 10 + 50/3
    // tall, 50/3 below it.
    let one = 0.6 * 10.0 / 0.7;
    close(
        extent(MText {
            text: r"A\PB",
            attachment: None,
            ..MText::default()
        }),
        [10.0, 20.0 - 50.0 / 3.0, 10.0 + one, 30.0],
    );
}

#[test]
fn a_turned_block_is_measured_turned() {
    // A quarter turn: the block's width runs up from the point, its height
    // to the left of it.
    close(
        extent(MText {
            attachment: Some(MTextAttachment::BottomLeft),
            rotation: FRAC_PI_2,
            extents: (Some(30.0), Some(8.0)),
            ..MText::default()
        }),
        [2.0, 20.0, 10.0, 50.0],
    );
}
