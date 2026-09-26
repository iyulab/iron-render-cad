//! What the tests that build drawings by hand share: the fields every entity
//! carries, in one place, so that a field the model adds to them is set here
//! once rather than in every test file.

use uncad_model::model::{Confidence, EntityCommon, EntityId, EntityLinetype, Origin, Ref};

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
