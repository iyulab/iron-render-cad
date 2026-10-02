//! AutoCAD color resolution for rendering: BYLAYER/BYBLOCK precedence, the
//! true-color override, and the one color the page would swallow, on top of
//! the ACI palette the model carries. Pure functions, kept separate from the
//! renderer's string building.
//!
//! Two of the quirks here (a layer's color reporting as white, and
//! white-on-white invisibility) were real bugs caught only by comparing
//! rendered output against AutoCAD itself -- see `docs/CAVEATS.md`.

use uncad_model::color::aci_to_rgb;
use uncad_model::tables::Tables;

/// The color an entity falls back to when nothing states one (BYBLOCK at
/// the top level, a BYLAYER color on a layer the tables do not hold), as
/// resolved -- before [`Paper::ink`] draws it on a page.
pub const DEFAULT_COLOR: &str = "#000000";

/// The page a render is drawn on.
///
/// Colors stay what the file says, with one exception: a resolved color
/// equal to the page's own would vanish into it, so it is drawn as the
/// opposite one ([`ink`](Self::ink)). ACI index 7 (0xFFFFFF, "white/black")
/// is AutoCAD's own auto-invert-by-background special case, and a layer's
/// color can report as white unconditionally (see [`layer_color_hex`]), so
/// on a light page a resolved pure white is drawn black; on a dark page
/// white stays white, as it shows on AutoCAD's dark model space, and a
/// resolved pure black is drawn white instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Paper {
    /// A white page. The document has no background of its own: it is
    /// meant to be shown on white.
    #[default]
    Light,
    /// A black page. The document's first drawn element is a black
    /// rectangle covering its whole view, so it reads the same on any
    /// background.
    Dark,
}

impl Paper {
    /// The page's own color, `#rrggbb`.
    pub const fn hex(self) -> &'static str {
        match self {
            Paper::Light => "#ffffff",
            Paper::Dark => "#000000",
        }
    }

    /// The color a resolved `#rrggbb` is drawn in on this page: the page's
    /// own color becomes the opposite one, every other color is unchanged.
    /// Drawing an already drawn color again changes nothing.
    pub fn ink(self, hex: String) -> String {
        match (self, hex.as_str()) {
            (Paper::Light, "#ffffff") => "#000000".to_string(),
            (Paper::Dark, "#000000") => "#ffffff".to_string(),
            _ => hex,
        }
    }
}

fn hex(packed: u32, paper: Paper) -> String {
    paper.ink(format!("#{:06x}", packed & 0xff_ffff))
}

/// ACI palette entry `index` as drawn on `paper`.
pub fn aci_to_hex(index: u16, paper: Paper) -> Option<String> {
    aci_to_rgb(index).map(|packed| hex(packed, paper))
}

/// A packed 24-bit color as drawn on `paper`.
pub fn true_color_to_hex(color: Option<u32>, paper: Paper) -> Option<String> {
    color.map(|packed| hex(packed, paper))
}

/// Deliberately ignores a layer's own truecolor field. On an older LibreDWG it
/// was a constant 0xFFFFFF placeholder on every real LAYER entry, so trusting
/// it rendered every BYLAYER entity black; a newer LibreDWG reports something
/// different (see `resolve_layer_color_index` in `table_convert.rs`), but the
/// conclusion holds either way -- `color_index` is the only trustworthy field,
/// and it is corrected before it ever reaches the model's `LayerRecord`.
pub fn layer_color_hex(tables: &Tables, layer_name: &str, paper: Paper) -> Option<String> {
    let layer = tables.layers.get(layer_name)?;
    aci_to_hex(layer.color_index.unsigned_abs(), paper)
}

/// AutoCAD's layer 0, the one layer name with a meaning inside a block:
/// geometry on layer 0 in a block definition is drawn on the layer of the
/// block reference that places it.
const LAYER_ZERO: &str = "0";

/// The layer an entity is effectively on. `reference_layer` is the layer of
/// the enclosing block reference -- `None` at the top level, where there is
/// none -- and it applies only to an entity on layer 0, which takes the
/// reference's layer; every other layer is used as the model states it.
///
/// Resolved here, at render time, not in the model: one block definition is
/// placed by many references on many layers, so "the layer of this entity"
/// only has an answer per reference.
pub(crate) fn effective_layer<'a>(layer: &'a str, reference_layer: Option<&'a str>) -> &'a str {
    match reference_layer {
        Some(reference) if layer == LAYER_ZERO => reference,
        _ => layer,
    }
}

/// Resolves an entity's rendered color following AutoCAD's own precedence:
/// explicit 24-bit truecolor overrides everything; otherwise `color_index`
/// is either BYLAYER (256, resolved through the entity's own layer),
/// BYBLOCK (0, inherited from the enclosing INSERT/DIMENSION via
/// `inherited_color` -- pass [`DEFAULT_COLOR`] at the top level, matching
/// AutoCAD's documented BYBLOCK-with-no-enclosing-block fallback), or a
/// direct ACI palette index. The sign of `color_index` (negative = "layer
/// off") is deliberately ignored -- this doesn't track visibility, only color.
/// The result is drawn on `paper`, fallbacks and inherited colors included.
pub fn resolve_color(
    color_index: i16,
    true_color: Option<u32>,
    layer: &str,
    tables: &Tables,
    inherited_color: &str,
    paper: Paper,
) -> String {
    if let Some(hex) = true_color_to_hex(true_color, paper) {
        return hex;
    }
    let resolved = match color_index {
        256 => layer_color_hex(tables, layer, paper).unwrap_or_else(|| DEFAULT_COLOR.to_string()),
        0 => inherited_color.to_string(),
        idx => aci_to_hex(idx.unsigned_abs(), paper).unwrap_or_else(|| DEFAULT_COLOR.to_string()),
    };
    paper.ink(resolved)
}

/// Blends a hex color toward white by `tint` (0.0 = unchanged, 1.0 = white),
/// clamped to `[0, 1]`. Approximates a single-color HATCH gradient's second
/// stop -- unverified, like the rest of `HatchGradient`.
pub fn tint_toward_white(hex: &str, tint: f64) -> String {
    let t = tint.clamp(0.0, 1.0);
    let packed = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0);
    let blend = |shift: u32| -> u32 {
        let c = ((packed >> shift) & 0xff) as f64;
        (c + (255.0 - c) * t).round() as u32
    };
    format!("#{:02x}{:02x}{:02x}", blend(16), blend(8), blend(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use uncad_model::tables::LayerRecord;

    fn tables_with(name: &str, color_index: i16) -> Tables {
        let mut layers = BTreeMap::new();
        layers.insert(
            name.to_string(),
            LayerRecord {
                name: name.to_string(),
                color_index,
                off: false,
                frozen: false,
                locked: false,
                plot: None,
                lineweight: None,
                linetype: uncad_model::Ref::Absent,
            },
        );
        Tables {
            layers,
            ..Default::default()
        }
    }

    #[test]
    fn aci_index_2_is_yellow() {
        assert_eq!(aci_to_hex(2, Paper::Light).as_deref(), Some("#ffff00"));
    }

    #[test]
    fn aci_index_7_white_normalizes_to_black() {
        assert_eq!(aci_to_hex(7, Paper::Light).as_deref(), Some("#000000"));
    }

    #[test]
    fn on_a_dark_page_white_stays_white_and_black_turns_white() {
        assert_eq!(aci_to_hex(7, Paper::Dark).as_deref(), Some("#ffffff"));
        assert_eq!(
            true_color_to_hex(Some(0x000000), Paper::Dark).as_deref(),
            Some("#ffffff")
        );
        assert_eq!(aci_to_hex(2, Paper::Dark).as_deref(), Some("#ffff00"));
        assert_eq!(aci_to_hex(250, Paper::Dark), aci_to_hex(250, Paper::Light));
    }

    #[test]
    fn the_fallback_and_an_inherited_black_are_drawn_on_the_page() {
        let tables = Tables::default();
        let unknown_layer = resolve_color(
            256,
            None,
            "nonexistent",
            &tables,
            DEFAULT_COLOR,
            Paper::Dark,
        );
        assert_eq!(unknown_layer, "#ffffff");
        let byblock_at_top = resolve_color(0, None, "0", &tables, DEFAULT_COLOR, Paper::Dark);
        assert_eq!(byblock_at_top, "#ffffff");
    }

    #[test]
    fn drawing_a_drawn_color_again_changes_nothing() {
        for paper in [Paper::Light, Paper::Dark] {
            for hex in ["#000000", "#ffffff", "#ffff00"] {
                let once = paper.ink(hex.to_string());
                assert_eq!(paper.ink(once.clone()), once);
                assert_ne!(
                    once,
                    paper.hex(),
                    "nothing is drawn in the page's own color"
                );
            }
        }
    }

    #[test]
    fn truecolor_overrides_colorindex() {
        let tables = tables_with("L", 2);
        let resolved = resolve_color(
            256,
            Some(0x00ff00),
            "L",
            &tables,
            DEFAULT_COLOR,
            Paper::Light,
        );
        assert_eq!(resolved, "#00ff00");
    }

    #[test]
    fn bylayer_resolves_through_layer_colorindex_not_layer_rgb() {
        // Regression test for the bug in docs/CAVEATS.md: a layer with color
        // index 2 (yellow) must resolve to yellow, never black, whatever
        // Dwg_Color.rgb the LAYER entry reports (LayerRecord does not even
        // carry it, by design).
        let tables = tables_with("Tavolo 1", 2);
        let resolved = resolve_color(256, None, "Tavolo 1", &tables, DEFAULT_COLOR, Paper::Light);
        assert_eq!(resolved, "#ffff00");
    }

    #[test]
    fn bylayer_with_unknown_layer_falls_back_to_default() {
        let tables = Tables::default();
        let resolved = resolve_color(
            256,
            None,
            "nonexistent",
            &tables,
            DEFAULT_COLOR,
            Paper::Light,
        );
        assert_eq!(resolved, DEFAULT_COLOR);
    }

    #[test]
    fn byblock_inherits_from_context() {
        let tables = Tables::default();
        let resolved = resolve_color(0, None, "0", &tables, "#123456", Paper::Light);
        assert_eq!(resolved, "#123456");
    }

    #[test]
    fn direct_aci_index_ignores_sign() {
        let tables = Tables::default();
        let positive = resolve_color(2, None, "0", &tables, DEFAULT_COLOR, Paper::Light);
        let negative = resolve_color(-2, None, "0", &tables, DEFAULT_COLOR, Paper::Light);
        assert_eq!(positive, "#ffff00");
        assert_eq!(
            negative, "#ffff00",
            "sign marks 'layer off', not a different color"
        );
    }

    #[test]
    fn only_layer_zero_takes_the_references_layer() {
        assert_eq!(effective_layer("0", Some("WALLS")), "WALLS");
        assert_eq!(effective_layer("DOORS", Some("WALLS")), "DOORS");
        // At the top level there is no reference to take a layer from.
        assert_eq!(effective_layer("0", None), "0");
    }

    #[test]
    fn tint_zero_leaves_color_unchanged() {
        assert_eq!(tint_toward_white("#123456", 0.0), "#123456");
    }

    #[test]
    fn tint_one_is_pure_white() {
        assert_eq!(tint_toward_white("#123456", 1.0), "#ffffff");
    }

    #[test]
    fn tint_half_blends_black_toward_mid_gray() {
        assert_eq!(tint_toward_white("#000000", 0.5), "#808080");
    }

    #[test]
    fn tint_out_of_range_is_clamped() {
        assert_eq!(tint_toward_white("#123456", -1.0), "#123456");
        assert_eq!(tint_toward_white("#123456", 2.0), "#ffffff");
    }
}
