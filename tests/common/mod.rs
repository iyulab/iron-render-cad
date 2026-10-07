//! What the tests that build drawings by hand share: the fields every entity
//! carries, in one place, so that a field the model adds to them is set here
//! once rather than in every test file.

#![allow(dead_code)]

use uncad_model::model::{Confidence, Entity, EntityCommon, EntityId, EntityLinetype, Origin, Ref};
use uncad_model::tables::BlockRecord;

/// A block definition named `name` holding `entities`, with its base point
/// at the origin -- the common case, in which placing it moves it by the
/// insertion point alone.
pub fn block_record(name: &str, entities: Vec<Entity>) -> BlockRecord {
    BlockRecord {
        base_point: Default::default(),
        name: name.to_string(),
        entities,
        external_reference: None,
    }
}

/// An entity read from a vector file with high confidence: its handle is
/// its ID in hex, it is on `layer` in ACI `color_index` (7 is white/black,
/// 256 BYLAYER), visible, in its layer's line type at scale 1, with the
/// default line weight and no transparency.
pub fn common_on(id: u64, layer: &str, color_index: i16) -> EntityCommon {
    EntityCommon {
        id: EntityId::new(id),
        origin: Origin::Vector,
        confidence: Confidence::High,
        source_handle: Ref::Resolved(format!("{id:X}")),
        layer: Ref::Resolved(layer.to_string()),
        color_index,
        true_color: None,
        invisible: false,
        linetype: EntityLinetype::ByLayer,
        linetype_scale: 1.0,
        lineweight: Some(-1),
        transparency: Some(0),
    }
}
