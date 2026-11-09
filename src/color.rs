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
/// Colors stay what the file says, with two exceptions, both made so that
/// nothing is drawn in a color the page would swallow ([`ink`](Self::ink)).
/// A resolved color equal to the page's own is drawn as the opposite one:
/// ACI index 7 (0xFFFFFF, "white/black") is AutoCAD's own
/// auto-invert-by-background special case, and a layer's color can report as
/// white unconditionally (see [`layer_color_hex`]), so on a light page a
/// resolved pure white is drawn black; on a dark page white stays white, as
/// it shows on AutoCAD's dark model space, and a resolved pure black is drawn
/// white instead. And on a light page a color whose contrast against white
/// is below 3:1 -- yellow, cyan, light gray -- is darkened until it reaches
/// 3:1, its hue kept.
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

/// The least contrast ratio (WCAG 2 relative luminance, against the page's
/// white) a color keeps on a light page: 3:1, the WCAG threshold for
/// graphical objects. A color below it is darkened to reach it.
#[cfg(test)]
const MIN_CONTRAST: f64 = 3.0;

/// What `1.0` is in [`LINEAR`]: 2^24.
const LINEAR_ONE: u64 = 1 << 24;

/// The linear-light value of each 8-bit sRGB level, scaled to
/// [`LINEAR_ONE`] and rounded. A table of integers, so that the contrast a
/// color is judged by -- and the color drawn for it -- does not depend on
/// the platform's floating-point `pow`.
#[rustfmt::skip]
const LINEAR: [u32; 256] = [
    0, 5092, 10185, 15277, 20369, 25462, 30554, 35646,
    40739, 45831, 50923, 56146, 61682, 67524, 73676, 80144,
    86931, 94043, 101483, 109255, 117364, 125813, 134607, 143749,
    153244, 163095, 173306, 183880, 194821, 206133, 217819, 229883,
    242327, 255157, 268373, 281981, 295983, 310382, 325182, 340386,
    355996, 372016, 388449, 405298, 422565, 440255, 458369, 476910,
    495881, 515286, 535127, 555406, 576126, 597291, 618902, 640963,
    663476, 686443, 709868, 733752, 758099, 782910, 808189, 833938,
    860159, 886854, 914027, 941680, 969814, 998433, 1027538, 1057133,
    1087218, 1117798, 1148873, 1180447, 1212520, 1245097, 1278179, 1311767,
    1345865, 1380475, 1415598, 1451237, 1487394, 1524071, 1561270, 1598994,
    1637244, 1676023, 1715332, 1755173, 1795550, 1836463, 1877915, 1919907,
    1962442, 2005522, 2049149, 2093324, 2138049, 2183328, 2229161, 2275550,
    2322497, 2370005, 2418074, 2466708, 2515908, 2565675, 2616012, 2666920,
    2718402, 2770458, 2823092, 2876304, 2930097, 2984472, 3039432, 3094977,
    3151110, 3207832, 3265145, 3323052, 3381553, 3440650, 3500346, 3560641,
    3621538, 3683038, 3745144, 3807855, 3871176, 3935106, 3999648, 4064803,
    4130573, 4196960, 4263965, 4331589, 4399836, 4468706, 4538200, 4608321,
    4679069, 4750448, 4822457, 4895099, 4968376, 5042288, 5116838, 5192027,
    5267856, 5344328, 5421443, 5499204, 5577611, 5656667, 5736372, 5816729,
    5897738, 5979402, 6061722, 6144699, 6228335, 6312631, 6397589, 6483210,
    6569496, 6656448, 6744068, 6832357, 6921317, 7010948, 7101253, 7192233,
    7283889, 7376223, 7469237, 7562930, 7657306, 7752366, 7848110, 7944540,
    8041658, 8139465, 8237963, 8337152, 8437035, 8537612, 8638885, 8740855,
    8843524, 8946893, 9050964, 9155737, 9261215, 9367397, 9474287, 9581885,
    9690192, 9799210, 9908940, 10019383, 10130542, 10242416, 10355008, 10468318,
    10582349, 10697100, 10812575, 10928773, 11045697, 11163346, 11281724, 11400831,
    11520668, 11641236, 11762538, 11884573, 12007344, 12130852, 12255098, 12380082,
    12505807, 12632274, 12759484, 12887438, 13016137, 13145583, 13275776, 13406719,
    13538412, 13670857, 13804054, 13938006, 14072712, 14208175, 14344396, 14481375,
    14619114, 14757615, 14896878, 15036905, 15177696, 15319253, 15461578, 15604671,
    15748533, 15893166, 16038571, 16184750, 16331702, 16479430, 16627934, 16777216,
];

/// Relative luminance of `rgb` (WCAG 2: weights 0.2126, 0.7152, 0.0722 on the
/// linear channels), as an integer scaled by `10_000 * LINEAR_ONE`.
fn luminance_scaled(rgb: [u8; 3]) -> u64 {
    2126 * u64::from(LINEAR[usize::from(rgb[0])])
        + 7152 * u64::from(LINEAR[usize::from(rgb[1])])
        + 722 * u64::from(LINEAR[usize::from(rgb[2])])
}

/// Whether `rgb` has at least 3:1 against white. Contrast is
/// `1.05 / (L + 0.05)`, so 3:1 holds exactly when `L <= 0.30`.
fn legible_on_white(rgb: [u8; 3]) -> bool {
    luminance_scaled(rgb) <= 3_000 * LINEAR_ONE
}

/// Resolution of the darkening: a color is scaled by `k / DARKEN_STEPS`.
const DARKEN_STEPS: u32 = 4096;

/// `rgb` scaled toward black by the same ratio on every channel -- the hue
/// and the saturation ratios stay -- by the largest ratio that leaves it
/// [legible on white](legible_on_white); `rgb` itself when it already is.
/// Integer arithmetic throughout.
fn darkened_for_white(rgb: [u8; 3]) -> [u8; 3] {
    if legible_on_white(rgb) {
        return rgb;
    }
    let scaled = |k: u32| rgb.map(|c| ((u32::from(c) * k + DARKEN_STEPS / 2) / DARKEN_STEPS) as u8);
    // The ratio 0 (black) is legible and `DARKEN_STEPS` (`rgb`) is not; the
    // luminance only falls as the ratio does, so the boundary is bisected.
    let (mut legible, mut illegible) = (0, DARKEN_STEPS);
    while illegible - legible > 1 {
        let mid = (legible + illegible) / 2;
        if legible_on_white(scaled(mid)) {
            legible = mid;
        } else {
            illegible = mid;
        }
    }
    scaled(legible)
}

fn parse_hex(hex: &str) -> Option<[u8; 3]> {
    let digits = hex.strip_prefix('#')?;
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let packed = u32::from_str_radix(digits, 16).ok()?;
    Some([(packed >> 16) as u8, (packed >> 8) as u8, packed as u8])
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
    /// own color becomes the opposite one, and on the light page a color
    /// whose contrast against white is below 3:1 is darkened until it
    /// reaches 3:1, its hue kept. Every other color is unchanged.
    /// Drawing an already drawn color again changes nothing.
    pub fn ink(self, hex: String) -> String {
        self.ink_noted(hex).0
    }

    /// [`ink`](Self::ink), and whether the color was darkened for contrast
    /// (the flip of the page's own color is not).
    fn ink_noted(self, hex: String) -> (String, bool) {
        match (self, hex.as_str()) {
            (Paper::Light, "#ffffff") => ("#000000".to_string(), false),
            (Paper::Dark, "#000000") => ("#ffffff".to_string(), false),
            (Paper::Light, _) => match parse_hex(&hex) {
                Some(rgb) if !legible_on_white(rgb) => {
                    let [r, g, b] = darkened_for_white(rgb);
                    (format!("#{r:02x}{g:02x}{b:02x}"), true)
                }
                _ => (hex, false),
            },
            _ => (hex, false),
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
    resolve_color_noted(
        color_index,
        true_color,
        layer,
        tables,
        inherited_color,
        paper,
    )
    .0
}

/// [`resolve_color`], and whether the entity's own color -- not an inherited
/// one, which was drawn already where it was stated -- was darkened for
/// contrast on `paper`.
pub(crate) fn resolve_color_noted(
    color_index: i16,
    true_color: Option<u32>,
    layer: &str,
    tables: &Tables,
    inherited_color: &str,
    paper: Paper,
) -> (String, bool) {
    let stated = |packed: u32| paper.ink_noted(format!("#{:06x}", packed & 0xff_ffff));
    let fallback = || paper.ink_noted(DEFAULT_COLOR.to_string());
    if let Some(packed) = true_color {
        return stated(packed);
    }
    match color_index {
        256 => tables
            .layers
            .get(layer)
            .and_then(|l| aci_to_rgb(l.color_index.unsigned_abs()))
            .map_or_else(fallback, stated),
        0 => paper.ink_noted(inherited_color.to_string()),
        idx => aci_to_rgb(idx.unsigned_abs()).map_or_else(fallback, stated),
    }
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
    fn aci_index_2_is_yellow_on_a_dark_page_and_a_darker_yellow_on_a_light_one() {
        assert_eq!(aci_to_hex(2, Paper::Dark).as_deref(), Some("#ffff00"));
        assert_eq!(aci_to_hex(2, Paper::Light).as_deref(), Some("#9a9a00"));
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
            for hex in ["#000000", "#ffffff", "#ffff00", "#00ffff", "#c0c0c0"] {
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
            Some(0x0000ff),
            "L",
            &tables,
            DEFAULT_COLOR,
            Paper::Light,
        );
        assert_eq!(resolved, "#0000ff");
    }

    #[test]
    fn bylayer_resolves_through_layer_colorindex_not_layer_rgb() {
        // Regression test for the bug in docs/CAVEATS.md: a layer with color
        // index 2 (yellow) must resolve to yellow, never black, whatever
        // Dwg_Color.rgb the LAYER entry reports (LayerRecord does not even
        // carry it, by design).
        let tables = tables_with("Tavolo 1", 2);
        let resolved = resolve_color(256, None, "Tavolo 1", &tables, DEFAULT_COLOR, Paper::Dark);
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
        let positive = resolve_color(2, None, "0", &tables, DEFAULT_COLOR, Paper::Dark);
        let negative = resolve_color(-2, None, "0", &tables, DEFAULT_COLOR, Paper::Dark);
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

    /// The WCAG 2 contrast ratio of `rgb` against white, in floating point:
    /// the reference the integer table is checked against.
    fn contrast_on_white(rgb: [u8; 3]) -> f64 {
        let linear = |c: u8| {
            let v = f64::from(c) / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let l = 0.2126 * linear(rgb[0]) + 0.7152 * linear(rgb[1]) + 0.0722 * linear(rgb[2]);
        1.05 / (l + 0.05)
    }

    fn rgb_of(hex: &str) -> [u8; 3] {
        parse_hex(hex).expect("a #rrggbb color")
    }

    fn drawn_light(hex: &str) -> String {
        Paper::Light.ink(hex.to_string())
    }

    #[test]
    fn the_linear_table_is_the_srgb_curve() {
        assert_eq!(LINEAR[0], 0);
        assert_eq!(u64::from(LINEAR[255]), LINEAR_ONE);
        for level in 0..=255u8 {
            let v = f64::from(level) / 255.0;
            let exact = if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            };
            let entry = f64::from(LINEAR[usize::from(level)]) / LINEAR_ONE as f64;
            assert!((entry - exact).abs() < 1e-7, "level {level}");
        }
        assert!(LINEAR.windows(2).all(|w| w[0] < w[1]), "strictly rising");
    }

    #[test]
    fn the_integer_judgement_agrees_with_the_floating_point_contrast() {
        // Every gray and a grid of the color cube, away from the boundary
        // where the two could differ by a rounding.
        let mut colors: Vec<[u8; 3]> = (0..=255u8).map(|g| [g, g, g]).collect();
        for r in (0..=255u8).step_by(15) {
            for g in (0..=255u8).step_by(15) {
                for b in (0..=255u8).step_by(15) {
                    colors.push([r, g, b]);
                }
            }
        }
        for rgb in colors {
            let ratio = contrast_on_white(rgb);
            if (ratio - MIN_CONTRAST).abs() > 1e-4 {
                assert_eq!(legible_on_white(rgb), ratio >= MIN_CONTRAST, "{rgb:?}");
            }
        }
    }

    #[test]
    fn light_and_saturated_pens_come_out_at_three_to_one_or_better() {
        for (name, file, drawn) in [
            ("ACI 2 yellow", "#ffff00", "#9a9a00"),
            ("ACI 3 green", "#00ff00", "#00ad00"),
            ("ACI 4 cyan", "#00ffff", "#00a5a5"),
            ("ACI 9 light gray", "#c0c0c0", "#949494"),
        ] {
            assert!(contrast_on_white(rgb_of(file)) < MIN_CONTRAST, "{name}");
            let out = drawn_light(file);
            assert_eq!(out, drawn, "{name}");
            assert!(contrast_on_white(rgb_of(&out)) >= MIN_CONTRAST, "{name}");
        }
    }

    #[test]
    fn every_aci_color_is_legible_on_a_light_page_and_the_legible_ones_are_unchanged() {
        let mut darkened = 0;
        for index in 1..=255u16 {
            let packed = aci_to_rgb(index).unwrap();
            let file = format!("#{packed:06x}");
            let out = aci_to_hex(index, Paper::Light).unwrap();
            if index == 7 {
                assert_eq!(out, "#000000");
                continue;
            }
            assert!(
                contrast_on_white(rgb_of(&out)) >= MIN_CONTRAST,
                "ACI {index}"
            );
            if contrast_on_white(rgb_of(&file)) >= MIN_CONTRAST + 1e-4 {
                assert_eq!(out, file, "ACI {index} passes and stays");
            }
            if out != file {
                darkened += 1;
            }
        }
        assert_eq!(darkened, 63, "the ACI colors below 3:1, as measured");
    }

    #[test]
    fn colors_that_already_pass_are_byte_identical() {
        for hex in [
            "#000000", "#ff0000", "#0000ff", "#ff00ff", "#808080", "#123456", "#7f7f7f",
        ] {
            assert_eq!(drawn_light(hex), hex);
        }
        assert_eq!(
            drawn_light("#ffffff"),
            "#000000",
            "white is flipped, as before"
        );
    }

    #[test]
    fn darkening_keeps_the_hue() {
        for hex in [
            "#ffff00", "#00ffff", "#00ff00", "#ffff7f", "#ffbf7f", "#7fffbf",
        ] {
            let [r, g, b] = rgb_of(hex).map(f64::from);
            let [dr, dg, db] = rgb_of(&drawn_light(hex)).map(f64::from);
            let scale = dr.max(dg).max(db) / r.max(g).max(b);
            assert!(scale < 1.0, "{hex} is darkened");
            // Every channel keeps its ratio to the others, up to the
            // rounding of 8-bit channels.
            for (from, to) in [(r, dr), (g, dg), (b, db)] {
                assert!((to - from * scale).abs() <= 1.0, "{hex}");
            }
        }
    }

    #[test]
    fn darkening_is_the_largest_step_that_reaches_the_floor() {
        // One level brighter on every channel is below 3:1, so the
        // correction made is the least that is.
        for hex in ["#ffff00", "#00ffff", "#ffff7f", "#c0c0c0"] {
            let out = rgb_of(&drawn_light(hex));
            let brighter = out.map(|c| c.saturating_add(1));
            assert!(!legible_on_white(brighter), "{hex}");
        }
    }

    #[test]
    fn a_dark_page_is_not_touched_by_the_contrast_floor() {
        for index in 1..=255u16 {
            let packed = aci_to_rgb(index).unwrap();
            let expected = if index == 7 {
                "#ffffff".to_string()
            } else {
                format!("#{packed:06x}")
            };
            assert_eq!(
                aci_to_hex(index, Paper::Dark).unwrap(),
                expected,
                "ACI {index}"
            );
        }
        assert_eq!(Paper::Dark.ink("#c0c0c0".to_string()), "#c0c0c0");
    }

    #[test]
    fn a_color_that_is_not_hex_is_left_alone() {
        assert_eq!(Paper::Light.ink("black".to_string()), "black");
        assert_eq!(Paper::Light.ink("#fff".to_string()), "#fff");
    }

    #[test]
    fn only_a_color_the_entity_states_is_counted_as_darkened() {
        let tables = tables_with("L", 4);
        let noted = |index: i16, inherited: &str| {
            resolve_color_noted(index, None, "L", &tables, inherited, Paper::Light)
        };
        assert_eq!(noted(2, DEFAULT_COLOR), ("#9a9a00".to_string(), true));
        assert_eq!(noted(256, DEFAULT_COLOR), ("#00a5a5".to_string(), true));
        assert_eq!(noted(1, DEFAULT_COLOR), ("#ff0000".to_string(), false));
        assert_eq!(noted(7, DEFAULT_COLOR), ("#000000".to_string(), false));
        // BYBLOCK takes the reference's drawn color, which was counted there.
        assert_eq!(noted(0, "#9a9a00"), ("#9a9a00".to_string(), false));
        let (_, dark) = resolve_color_noted(2, None, "L", &tables, DEFAULT_COLOR, Paper::Dark);
        assert!(!dark);
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
