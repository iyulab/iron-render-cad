//! What the drawing hides. A file carries entities nobody sees on a plot:
//! ones marked invisible, attribute values whose own flag hides them,
//! AutoCAD's `DEFPOINTS` layer (where a dimension keeps its definition
//! points), and anything on a layer that is switched off, frozen or marked
//! not to plot. The renderer leaves them out of the picture -- or draws
//! them faded, when asked -- and counts them, and never lets them into the
//! extent.

use crate::color::effective_layer;
use std::collections::BTreeSet;
use uncad_model::model::Entity;
use uncad_model::tables::Tables;

/// Why the drawing hides an entity (see
/// [`ToSvgOptions::include_hidden`](crate::ToSvgOptions::include_hidden)).
/// Checked in this order, so an invisible entity on a frozen layer is
/// hidden because it is invisible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Hidden {
    /// The entity's own invisible flag (DXF 60), or an ATTRIB's (DXF 70,
    /// bit 1).
    Invisible,
    /// On the `DEFPOINTS` layer, which AutoCAD never plots whatever its
    /// state says.
    Defpoints,
    /// The layer is switched off.
    LayerOff,
    /// The layer is frozen.
    LayerFrozen,
    /// The layer is frozen in the viewport the entity is drawn through
    /// (a paper layout's, see `svg::sheet`).
    FrozenInViewport,
    /// The layer is stated not to plot. A layer whose file does not say
    /// (`plot: None`) plots.
    LayerNoPlot,
}

/// The name of the layer AutoCAD never plots.
const DEFPOINTS: &str = "DEFPOINTS";

/// Why the drawing hides `e`, or `None` when it shows it. `reference_layer`
/// is the effective layer of the block reference `e` is drawn inside --
/// `None` at the top level -- which an entity on layer 0 takes as its own
/// (see [`effective_layer`]); `frozen_here` the layers frozen in the
/// viewport it is drawn through, empty outside one. A layer the model does
/// not hold, or one the entity does not resolve to, is shown unless the
/// viewport freezes it: nothing says otherwise.
pub(super) fn hidden_reason(
    e: &Entity,
    tables: &Tables,
    reference_layer: Option<&str>,
    frozen_here: &BTreeSet<String>,
) -> Option<Hidden> {
    let common = e.common();
    if common.invisible || matches!(e, Entity::Attrib(a) if a.flags.invisible) {
        return Some(Hidden::Invisible);
    }
    let layer = effective_layer(common.layer.name(), reference_layer);
    if layer.eq_ignore_ascii_case(DEFPOINTS) {
        return Some(Hidden::Defpoints);
    }
    let record = tables.layers.get(layer);
    if record.is_some_and(|r| r.off) {
        Some(Hidden::LayerOff)
    } else if record.is_some_and(|r| r.frozen) {
        Some(Hidden::LayerFrozen)
    } else if frozen_here.contains(layer) {
        Some(Hidden::FrozenInViewport)
    } else if record.is_some_and(|r| r.plot == Some(false)) {
        Some(Hidden::LayerNoPlot)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use uncad_model::model::{Confidence, EntityCommon, EntityId, LineEntity, Origin, Point3D};
    use uncad_model::tables::LayerRecord;
    use uncad_model::Ref;

    fn tables() -> Tables {
        let mut layers = BTreeMap::new();
        for (name, off, frozen, plot) in [
            ("SHOWN", false, false, None),
            ("PLOTTED", false, false, Some(true)),
            ("OFF", true, false, None),
            ("FROZEN", false, true, None),
            ("NOPLOT", false, false, Some(false)),
            ("OFF-AND-FROZEN", true, true, Some(false)),
            ("Defpoints", false, false, None),
            ("0", true, false, None),
        ] {
            layers.insert(
                name.to_string(),
                LayerRecord {
                    name: name.to_string(),
                    color_index: 7,
                    off,
                    frozen,
                    locked: false,
                    plot,
                    lineweight: None,
                    linetype: Ref::Absent,
                },
            );
        }
        Tables {
            layers,
            ..Tables::default()
        }
    }

    fn on(layer: Ref<String>) -> Entity {
        let p = Point3D {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        };
        Entity::Line(LineEntity {
            common: EntityCommon {
                id: EntityId::new(1),
                origin: Origin::Vector,
                confidence: Confidence::High,
                source_handle: Ref::Absent,
                layer,
                color_index: 256,
                true_color: None,
                invisible: false,
            },
            start_point: p,
            end_point: p,
        })
    }

    fn named(layer: &str) -> Entity {
        on(Ref::Resolved(layer.to_string()))
    }

    #[test]
    fn each_reason_in_its_order() {
        let t = tables();
        let none = BTreeSet::new();
        let reason = |e: &Entity| hidden_reason(e, &t, None, &none);
        assert_eq!(reason(&named("SHOWN")), None);
        assert_eq!(reason(&named("PLOTTED")), None);
        assert_eq!(reason(&named("OFF")), Some(Hidden::LayerOff));
        assert_eq!(reason(&named("FROZEN")), Some(Hidden::LayerFrozen));
        assert_eq!(reason(&named("NOPLOT")), Some(Hidden::LayerNoPlot));
        assert_eq!(reason(&named("OFF-AND-FROZEN")), Some(Hidden::LayerOff));
        // DEFPOINTS by its name, in any case, whatever its record says --
        // or with no record at all.
        assert_eq!(reason(&named("Defpoints")), Some(Hidden::Defpoints));
        assert_eq!(reason(&named("DEFPOINTS")), Some(Hidden::Defpoints));
        // The entity's own flag first.
        let mut invisible = named("FROZEN");
        invisible.common_mut().invisible = true;
        assert_eq!(reason(&invisible), Some(Hidden::Invisible));
        // Nothing says a layer the model does not hold, or a reference that
        // resolved to nothing, is hidden.
        assert_eq!(reason(&named("NOT-A-LAYER")), None);
        assert_eq!(reason(&on(Ref::Absent)), None);
        assert_eq!(reason(&on(Ref::Unresolved("2A".to_string()))), None);
    }

    #[test]
    fn inside_a_block_layer_zero_is_the_references_layer() {
        let t = tables();
        let none = BTreeSet::new();
        // Layer 0 is off here, but a child on it takes the reference's
        // layer; a child on its own layer keeps it.
        assert_eq!(hidden_reason(&named("0"), &t, Some("SHOWN"), &none), None);
        assert_eq!(
            hidden_reason(&named("0"), &t, Some("FROZEN"), &none),
            Some(Hidden::LayerFrozen)
        );
        assert_eq!(
            hidden_reason(&named("OFF"), &t, Some("SHOWN"), &none),
            Some(Hidden::LayerOff)
        );
        // At the top level, layer 0 is layer 0.
        assert_eq!(
            hidden_reason(&named("0"), &t, None, &none),
            Some(Hidden::LayerOff)
        );
    }

    #[test]
    fn a_viewport_freezes_its_own_layers_whether_or_not_the_model_holds_them() {
        let t = tables();
        let frozen: BTreeSet<String> = ["SHOWN", "UNLISTED"].map(String::from).into();
        let reason = |e: &Entity| hidden_reason(e, &t, None, &frozen);
        assert_eq!(reason(&named("SHOWN")), Some(Hidden::FrozenInViewport));
        assert_eq!(reason(&named("UNLISTED")), Some(Hidden::FrozenInViewport));
        assert_eq!(reason(&named("PLOTTED")), None);
        // Frozen everywhere is still said as that.
        assert_eq!(reason(&named("FROZEN")), Some(Hidden::LayerFrozen));
    }
}
