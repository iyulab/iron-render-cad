//! SVG -> PNG rasterization, layered on top of the SVG renderer
//! ([`crate::to_svg`]) via resvg/usvg/tiny-skia (all pure Rust, no system
//! library dependency). This module chooses the pixel size, the stroke
//! width in pixels, the fonts and the background, and runs the raster step.
//!
//! A single `resvg` dependency is used rather than separate `usvg` /
//! `tiny-skia` / `fontdb` crates: `resvg` re-exports both (`resvg::usvg`,
//! `resvg::tiny_skia`, and `usvg::fontdb`), which keeps the three in lock
//! step instead of risking a version mismatch across independently pinned
//! crates.

use crate::svg::{self, Part, Rect, Scene, ToSvgOptions};
use resvg::tiny_skia;
use resvg::usvg::{self, fontdb};
use std::sync::{Arc, OnceLock};
use uncad_model::model::Point2D;
use uncad_model::CadDatabase;

/// The largest either side of an image may be unless a caller says
/// otherwise: [`ToPngOptions::max_edge`]'s default, and the bound
/// [`svg_to_png`] applies. A pixel is four bytes, so the largest image this
/// allows is 8192 x 8192 x 4 = 256 MiB of pixels.
pub const DEFAULT_MAX_EDGE: u32 = 8192;

#[derive(Debug, Clone, PartialEq)]
pub struct ToPngOptions {
    pub svg: ToSvgOptions,
    /// How many pixels the image is. Default [`PngSize::Scale`]`(1.0)`, one
    /// pixel per drawing unit.
    pub size: PngSize,
    /// Every stroke's width in output pixels, turned into drawing units once
    /// the pixel scale is known. `None` (the default) keeps
    /// [`ToSvgOptions::stroke_width`]'s rule -- about 1/6000th of the
    /// viewBox diagonal, which is below a pixel at most sizes. An explicit
    /// `svg.stroke_width` takes precedence over this.
    pub stroke_px: Option<f64>,
    /// Neither side of the image may be more pixels than this; a larger
    /// request fails with [`PngError::TooLarge`] before any pixel memory is
    /// allocated. The viewBox comes from the drawing's own coordinates, so
    /// without a bound a file decides how much memory a render asks for.
    /// Default [`DEFAULT_MAX_EDGE`].
    pub max_edge: u32,
    /// The fonts text is drawn with. Default [`Fonts::System`].
    pub fonts: Fonts,
    /// What the pixels the drawing does not touch are. Default
    /// [`Background::White`].
    pub background: Background,
}

/// What the pixels a PNG's drawing does not touch are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Background {
    /// Opaque white. The default: the renderer's colors are chosen for a
    /// white page (pure white is drawn black), so a transparent image shown
    /// on a dark viewer or flattened onto black loses the black lines.
    #[default]
    White,
    /// Fully transparent, as [`svg_to_png`] draws.
    Transparent,
}

impl Default for ToPngOptions {
    fn default() -> Self {
        ToPngOptions {
            svg: ToSvgOptions::default(),
            size: PngSize::default(),
            stroke_px: None,
            max_edge: DEFAULT_MAX_EDGE,
            fonts: Fonts::default(),
            background: Background::default(),
        }
    }
}

/// How the image's pixel size follows from the drawing's viewBox.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum PngSize {
    /// This many pixels per drawing unit: the viewBox's size times this --
    /// e.g. `2.0` renders at twice one pixel a unit. Must be finite and
    /// greater than 0.
    Scale(f32),
    /// The longer side of the image is this many pixels, the other follows
    /// the viewBox's aspect ratio: an image of a known size whatever the
    /// drawing's units.
    FitLongEdge(u32),
}

impl Default for PngSize {
    fn default() -> Self {
        PngSize::Scale(1.0)
    }
}

/// The stroke width, in drawing units, a PNG drawn at `px_per_unit` uses:
/// an explicit SVG width first, then `stroke_px` turned into units, then the
/// SVG's own rule (`auto`). A `stroke_px` that is not a positive number is
/// ignored.
fn stroke_width(
    svg_stroke_width: Option<f64>,
    stroke_px: Option<f64>,
    px_per_unit: f64,
    auto: f64,
) -> f64 {
    match (svg_stroke_width, stroke_px) {
        (Some(units), _) => units,
        (None, Some(px)) if px.is_finite() && px > 0.0 => px / px_per_unit,
        _ => auto,
    }
}

/// Which fonts a PNG's `<text>` is drawn with. The SVG itself names no
/// font; this is where the face is chosen.
#[derive(Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Fonts {
    /// The host's installed fonts, found on the first call in a process and
    /// shared by every later one (scanning them is the slow part of a small
    /// render). Text is drawn in usvg's default family, or whatever the host
    /// substitutes for it; a host with no matching font draws `<text>` blank
    /// rather than failing.
    #[default]
    System,
    /// These fonts and no others -- nothing from the host, so the same
    /// drawing gives the same picture everywhere. Each is the bytes of an
    /// OpenType or TrueType file (a collection contributes every face in
    /// it); text is drawn in the family of the first face that loads. A
    /// character none of them has is drawn as the face's missing-glyph
    /// shape. This crate bundles no font of its own: a caller that ships
    /// one hands it over here, and states its capital height in
    /// [`ToSvgOptions::cap_height`].
    Custom(Vec<Arc<[u8]>>),
}

impl std::fmt::Debug for Fonts {
    /// The sizes of custom fonts, not their bytes.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fonts::System => write!(f, "System"),
            Fonts::Custom(files) => f
                .debug_tuple("Custom")
                .field(&files.iter().map(|b| b.len()).collect::<Vec<_>>())
                .finish(),
        }
    }
}

/// The usvg options that draw text with `fonts`.
///
/// The host's fonts are scanned once per process and the database is
/// shared from then on; custom fonts are parsed per call, which for a file
/// or two is cheap.
fn usvg_options(fonts: &Fonts) -> usvg::Options<'static> {
    static SYSTEM: OnceLock<Arc<fontdb::Database>> = OnceLock::new();
    match fonts {
        Fonts::System => usvg::Options {
            fontdb: SYSTEM
                .get_or_init(|| {
                    let mut db = fontdb::Database::new();
                    db.load_system_fonts();
                    Arc::new(db)
                })
                .clone(),
            ..Default::default()
        },
        Fonts::Custom(files) => {
            let mut db = fontdb::Database::new();
            let mut family = None;
            for bytes in files {
                let source = fontdb::Source::Binary(Arc::new(bytes.clone()));
                for id in db.load_font_source(source) {
                    if family.is_none() {
                        family = db
                            .face(id)
                            .and_then(|face| face.families.first())
                            .map(|(name, _)| name.clone());
                    }
                }
            }
            let mut options = usvg::Options::default();
            if let Some(family) = family {
                // Text names no family, so it asks for the default one; the
                // generic families are what usvg falls back to.
                db.set_serif_family(family.clone());
                db.set_sans_serif_family(family.clone());
                options.font_family = family;
            }
            options.fontdb = Arc::new(db);
            options
        }
    }
}

/// A picture's pixels laid over the world: `width` x `height` pixels,
/// `px_per_unit` of them to a drawing unit, the top-left corner of the
/// top-left pixel at the world point (`left`, `top`). Pixel rows run down
/// the picture, the world's y up it, so a world point `(x, y)` is at pixel
/// `((x - left) * px_per_unit, (top - y) * px_per_unit)` -- fractional, a
/// pixel's centre at `.5`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub left: f64,
    pub top: f64,
    pub px_per_unit: f64,
    pub width: u32,
    pub height: u32,
}

impl View {
    /// The pixels showing `window` at `px_per_unit`: from its top-left
    /// corner, as many whole pixels as it is wide and tall at that scale,
    /// each rounded to the nearest. The pixels then end where the last one
    /// does, within half a pixel of the window's far edges; the scale is
    /// exact. `None` when the scale is not a positive number, or the size
    /// is not one a picture can have (not a number, or past `u32::MAX`).
    pub fn of(window: Rect, px_per_unit: f64) -> Option<View> {
        if !(px_per_unit.is_finite() && px_per_unit > 0.0) {
            return None;
        }
        let px = |units: f64| {
            let n = (units * px_per_unit).round();
            (n.is_finite() && n >= 0.0 && n <= f64::from(u32::MAX)).then_some(n as u32)
        };
        Some(View {
            left: window.min_x,
            top: window.max_y,
            px_per_unit,
            width: px(window.width())?,
            height: px(window.height())?,
        })
    }

    /// The world rectangle the pixels cover.
    pub fn window(&self) -> Rect {
        Rect::new(
            self.left,
            self.top - f64::from(self.height) / self.px_per_unit,
            self.left + f64::from(self.width) / self.px_per_unit,
            self.top,
        )
    }

    /// Where the world point `p` is in the picture, in pixels from its
    /// top-left corner.
    pub fn world_to_px(&self, p: Point2D) -> [f64; 2] {
        [
            (p.x - self.left) * self.px_per_unit,
            (self.top - p.y) * self.px_per_unit,
        ]
    }

    /// The world point at pixel position `[px, py]`: the inverse of
    /// [`world_to_px`](Self::world_to_px).
    pub fn px_to_world(&self, [px, py]: [f64; 2]) -> Point2D {
        Point2D {
            x: self.left + px / self.px_per_unit,
            y: self.top - py / self.px_per_unit,
        }
    }
}

pub struct ToPngResult {
    pub png: Vec<u8>,
    /// Where the image's pixels lie in the world: its size, its scale and
    /// the world point of its top-left corner. For a layout's sheet, the
    /// paper in the layout's paper units.
    pub view: View,
    pub unsupported_types: Vec<String>,
    /// See [`ToSvgResult::empty_blocks`](crate::ToSvgResult::empty_blocks).
    pub empty_blocks: Vec<String>,
    /// See
    /// [`ToSvgResult::unresolved_block_refs`](crate::ToSvgResult::unresolved_block_refs).
    pub unresolved_block_refs: Vec<uncad_model::EntityId>,
    /// See [`ToSvgResult::limits`](crate::ToSvgResult::limits).
    pub limits: crate::limits::LimitReport,
    /// See [`ToSvgResult::hidden`](crate::ToSvgResult::hidden).
    pub hidden: usize,
    /// See
    /// [`ToSvgResult::undrawn_viewports`](crate::ToSvgResult::undrawn_viewports).
    pub undrawn_viewports: Vec<uncad_model::EntityId>,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum PngError {
    /// usvg couldn't parse the intermediate SVG string -- since that string
    /// comes from this crate's own `to_svg`, this would indicate a bug
    /// there rather than bad input from a caller.
    InvalidSvg(usvg::Error),
    /// The requested pixel size rounds to zero in at least one dimension, or
    /// the size asked for is not a positive number.
    EmptyCanvas,
    /// The requested pixel size exceeds the largest side allowed
    /// ([`ToPngOptions::max_edge`]). Nothing was allocated.
    TooLarge {
        width: u32,
        height: u32,
        max_edge: u32,
    },
    /// tiny-skia's PNG encoder failed. Stored as its `Display` text rather
    /// than the underlying `png::EncodingError` type itself, so this crate
    /// doesn't need its own direct dependency on the `png` crate (tiny-skia
    /// doesn't re-export it) just to name the error type.
    Encode(String),
    /// The rasterizer panicked. tiny-skia asserts, rather than returning an
    /// error, when a path's coordinates overflow its fixed-point scan
    /// converter -- a line a million units long in a drawing a few
    /// thousandths of a unit across is enough. The panic is caught and
    /// returned here with its own message, so a caller gets an error
    /// instead of a dead process.
    RenderPanic(String),
    /// [`layout_to_png`] has no sheet to draw.
    Layout(crate::LayoutError),
}

impl std::fmt::Display for PngError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PngError::InvalidSvg(e) => write!(f, "SVG parsing failed: {e}"),
            PngError::EmptyCanvas => write!(f, "render size is zero (check the size option)"),
            PngError::TooLarge {
                width,
                height,
                max_edge,
            } => write!(
                f,
                "render size {width}x{height} px exceeds the {max_edge} px limit (ask for a smaller size, or raise max_edge)"
            ),
            PngError::Encode(e) => write!(f, "PNG encoding failed: {e}"),
            PngError::RenderPanic(e) => write!(f, "the rasterizer panicked: {e}"),
            PngError::Layout(e) => write!(f, "no sheet to draw: {e}"),
        }
    }
}
impl std::error::Error for PngError {}

/// Renders `db` straight to PNG bytes: the drawing [`crate::to_svg`] would
/// write, drawn at the size, stroke width, fonts and background `options`
/// ask for. The intermediate SVG text never touches disk.
///
/// Text is drawn with `options.fonts` -- by default the host's installed
/// fonts, scanned once per process; this crate bundles none. A host with no
/// matching font renders `<text>` entities (dimension/MTEXT labels) as blank
/// rather than erroring -- usvg treats an unresolved glyph as empty, not a
/// parse failure.
pub fn to_png(db: &CadDatabase, options: ToPngOptions) -> Result<ToPngResult, PngError> {
    png_result(svg::render(db, options.svg), &options)
}

/// Renders the paper layout named `layout` straight to PNG bytes: the sheet
/// [`crate::layout_to_svg`] would write, drawn at the size, stroke width,
/// fonts and background `options` ask for. [`PngError::Layout`] when there
/// is no such sheet to draw.
pub fn layout_to_png(
    db: &CadDatabase,
    layout: &str,
    options: ToPngOptions,
) -> Result<ToPngResult, PngError> {
    let rendered = svg::render_layout(db, layout, options.svg).map_err(PngError::Layout)?;
    png_result(rendered, &options)
}

/// The PNG of a render, at `options`' size, stroke, fonts and background.
fn png_result(scene: svg::Scene, options: &ToPngOptions) -> Result<ToPngResult, PngError> {
    let [_, _, width, height] = scene.doc_view_box();
    let px_per_unit = match options.size {
        PngSize::Scale(s) => f64::from(s),
        PngSize::FitLongEdge(px) => f64::from(px) / width.max(height),
    };
    if !(px_per_unit.is_finite() && px_per_unit > 0.0) {
        return Err(PngError::EmptyCanvas);
    }
    let stroke = stroke_width(
        options.svg.stroke_width,
        options.stroke_px,
        px_per_unit,
        scene.auto_stroke_width,
    );
    let scale = px_per_unit as f32;
    let tree = parse(&scene.document(stroke), &options.fonts)?;
    let (width, height) = pixel_size(&tree, scale);
    let png = draw(
        &tree,
        scale,
        width,
        height,
        options.max_edge,
        options.background,
    )?;
    Ok(ToPngResult {
        png,
        view: View {
            left: scene.view_box.min_x,
            top: scene.view_box.max_y,
            // What the picture was drawn at: the rasterizer's scale is an
            // `f32`.
            px_per_unit: f64::from(scale),
            width,
            height,
        },
        unsupported_types: scene.unsupported_types,
        empty_blocks: scene.empty_blocks,
        unresolved_block_refs: scene.unresolved_block_refs,
        limits: scene.limits,
        hidden: scene.hidden,
        undrawn_viewports: scene.undrawn_viewports,
    })
}

/// Rasterizes an already-built SVG string to PNG bytes at `scale`x the
/// SVG's own viewBox-derived size. Split out from [`to_png`] so a caller
/// that already has an SVG string (e.g. from a separately cached
/// [`crate::to_svg`] call) doesn't have to re-render the CAD geometry to get
/// a PNG out of it.
///
/// Neither side of the image may exceed [`DEFAULT_MAX_EDGE`] pixels; a
/// larger request fails with [`PngError::TooLarge`] instead of allocating.
/// Text is drawn with the host's fonts ([`Fonts::System`]), on a
/// transparent background.
pub fn svg_to_png(svg: &str, scale: f32) -> Result<Vec<u8>, PngError> {
    rasterize(
        svg,
        scale,
        DEFAULT_MAX_EDGE,
        &Fonts::System,
        Background::Transparent,
    )
}

impl Scene {
    /// The picture `view` describes, as PNG bytes: the document of the
    /// view's [`window`](View::window) from the parts `keep` accepts
    /// ([`Scene::svg`]), drawn at exactly `view.width` x `view.height`
    /// pixels, every stroke `stroke_px` pixels wide (a width that is not a
    /// positive number keeps the scene's automatic one), text in `fonts`,
    /// on `background`.
    ///
    /// The view is the caller's, so the picture is exactly that grid of
    /// pixels: pixel `(0, 0)`'s corner at (`view.left`, `view.top`), one
    /// pixel `1 / view.px_per_unit` units wide -- the same map
    /// [`View::world_to_px`] computes. A side of 0 pixels is
    /// [`PngError::EmptyCanvas`], one past [`DEFAULT_MAX_EDGE`]
    /// [`PngError::TooLarge`].
    pub fn png(
        &self,
        view: &View,
        stroke_px: f64,
        fonts: &Fonts,
        background: Background,
        keep: impl Fn(&Part) -> bool,
    ) -> Result<Vec<u8>, PngError> {
        if !(view.px_per_unit.is_finite() && view.px_per_unit > 0.0) {
            return Err(PngError::EmptyCanvas);
        }
        // Refused before anything is written or parsed, let alone
        // allocated: see [`draw`].
        if view.width > DEFAULT_MAX_EDGE || view.height > DEFAULT_MAX_EDGE {
            return Err(PngError::TooLarge {
                width: view.width,
                height: view.height,
                max_edge: DEFAULT_MAX_EDGE,
            });
        }
        let stroke = stroke_width(
            None,
            Some(stroke_px),
            view.px_per_unit,
            self.auto_stroke_width,
        );
        let tree = parse(&self.svg(view.window(), stroke, keep), fonts)?;
        draw(
            &tree,
            view.px_per_unit as f32,
            view.width,
            view.height,
            DEFAULT_MAX_EDGE,
            background,
        )
    }
}

/// [`svg_to_png`] with an explicit bound on the image's sides.
fn rasterize(
    svg: &str,
    scale: f32,
    max_edge: u32,
    fonts: &Fonts,
    background: Background,
) -> Result<Vec<u8>, PngError> {
    let tree = parse(svg, fonts)?;
    let (width, height) = pixel_size(&tree, scale);
    draw(&tree, scale, width, height, max_edge, background)
}

/// `svg` parsed for drawing with `fonts`.
fn parse(svg: &str, fonts: &Fonts) -> Result<usvg::Tree, PngError> {
    usvg::Tree::from_str(svg, &usvg_options(fonts)).map_err(PngError::InvalidSvg)
}

/// The pixel size of `tree`'s viewBox at `scale`, each side rounded to the
/// nearest pixel.
fn pixel_size(tree: &usvg::Tree, scale: f32) -> (u32, u32) {
    let size = tree.size();
    (
        (size.width() * scale).round() as u32,
        (size.height() * scale).round() as u32,
    )
}

/// `tree` drawn at `scale` into a `width` x `height` pixmap from its
/// viewBox's top-left corner, as PNG bytes.
///
/// The bound is checked before the pixmap exists: the pixmap's allocation
/// cannot fail gracefully (tiny-skia allocates it with `vec!`, and a request
/// the allocator refuses aborts the process), so a size nobody should
/// allocate has to be refused before asking.
fn draw(
    tree: &usvg::Tree,
    scale: f32,
    width: u32,
    height: u32,
    max_edge: u32,
    background: Background,
) -> Result<Vec<u8>, PngError> {
    if width > max_edge || height > max_edge {
        return Err(PngError::TooLarge {
            width,
            height,
            max_edge,
        });
    }
    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or(PngError::EmptyCanvas)?;
    if background == Background::White {
        pixmap.fill(tiny_skia::Color::WHITE);
    }

    catch_panic(|| {
        resvg::render(
            tree,
            tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        )
    })?;

    pixmap
        .encode_png()
        .map_err(|e| PngError::Encode(e.to_string()))
}

/// Runs `render` and turns a panic inside it into [`PngError::RenderPanic`]
/// carrying the panic's own message.
///
/// Asserting unwind safety is sound because nothing `render` touched is
/// used after a panic: the pixmap it drew into is dropped unread and the
/// error is returned instead.
fn catch_panic(render: impl FnOnce()) -> Result<(), PngError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(render)).map_err(|payload| {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "no message".to_string());
        PngError::RenderPanic(message)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decodes just the IHDR chunk's width/height (bytes 16..24 of any
    /// PNG) rather than pulling in an image-decoding dependency purely for
    /// test assertions.
    fn png_dimensions(png: &[u8]) -> (u32, u32) {
        let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
        (width, height)
    }

    #[test]
    fn svg_to_png_produces_a_valid_png_sized_to_the_viewbox() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 20"><rect width="10" height="20" fill="red"/></svg>"#;
        let png = svg_to_png(svg, 1.0).expect("svg_to_png should succeed");
        assert!(
            png.starts_with(b"\x89PNG\r\n\x1a\n"),
            "output should start with the PNG signature"
        );
        assert_eq!(png_dimensions(&png), (10, 20));
    }

    #[test]
    fn svg_to_png_scale_multiplies_the_viewbox_pixel_size() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 20"></svg>"#;
        let png = svg_to_png(svg, 2.5).expect("svg_to_png should succeed");
        assert_eq!(png_dimensions(&png), (25, 50));
    }

    #[test]
    fn a_size_past_the_bound_is_refused_before_anything_is_allocated() {
        // 1e7 units at one pixel a unit is a 1e7 x 1e7 pixmap: 400 TB, which
        // the allocator refuses by aborting the process. It must come back
        // as an error instead, naming the size that was asked for.
        let svg =
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10000000 10000000"></svg>"#;
        match svg_to_png(svg, 1.0) {
            Err(PngError::TooLarge {
                width,
                height,
                max_edge,
            }) => {
                assert_eq!((width, height), (10_000_000, 10_000_000));
                assert_eq!(max_edge, DEFAULT_MAX_EDGE);
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
        // Exactly at the bound is allowed, one pixel over is not.
        let edge = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 30 10"></svg>"#;
        assert!(rasterize(edge, 1.0, 30, &Fonts::System, Background::White).is_ok());
        assert!(matches!(
            rasterize(edge, 1.0, 29, &Fonts::System, Background::White),
            Err(PngError::TooLarge {
                width: 30,
                height: 10,
                max_edge: 29
            })
        ));
    }

    #[test]
    fn to_png_refuses_a_drawing_whose_extent_is_too_large_for_its_scale() {
        // A drawing 1e7 units across rendered at the default one pixel a
        // unit: the render must return, and say why there is no image.
        let mut db: CadDatabase =
            serde_json::from_str(include_str!("../tests/golden/g2.expected.json"))
                .expect("the golden model deserializes");
        db.entities
            .push(uncad_model::Entity::Line(uncad_model::model::LineEntity {
                common: db.entities[0].common().clone(),
                start_point: uncad_model::Point3D {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                end_point: uncad_model::Point3D {
                    x: 1.0e7,
                    y: 1.0e7,
                    z: 0.0,
                },
            }));
        let options = ToPngOptions {
            svg: ToSvgOptions {
                space: crate::Space::All,
                ..ToSvgOptions::default()
            },
            ..ToPngOptions::default()
        };
        let err = to_png(&db, options.clone())
            .err()
            .expect("too large to rasterize");
        assert!(matches!(err, PngError::TooLarge { .. }), "{err}");
        // A caller who asks for fewer pixels a unit gets the image.
        let small = ToPngOptions {
            size: PngSize::Scale(1.0e-4),
            ..options.clone()
        };
        assert!(to_png(&db, small).is_ok());
        // So does one who asks for an image of a given size.
        let fit = ToPngOptions {
            size: PngSize::FitLongEdge(1000),
            ..options
        };
        let png = to_png(&db, fit).expect("fits").png;
        assert_eq!(png_dimensions(&png).0.max(png_dimensions(&png).1), 1000);
    }

    #[test]
    fn fit_long_edge_sizes_the_longer_side_and_keeps_the_aspect_ratio() {
        // G1's viewBox is wider than tall; its longer side becomes 1000 px
        // and the shorter one follows.
        let db: CadDatabase =
            serde_json::from_str(include_str!("../tests/golden/g1.expected.json"))
                .expect("the golden model deserializes");
        let options = ToPngOptions {
            size: PngSize::FitLongEdge(1000),
            ..ToPngOptions::default()
        };
        let scene = svg::render(&db, options.svg);
        let [_, _, w, h] = scene.doc_view_box();
        let (width, height) = png_dimensions(&to_png(&db, options).expect("renders").png);
        assert_eq!(width.max(height), 1000);
        let expected_short = (w.min(h) * 1000.0 / w.max(h)).round() as u32;
        assert_eq!(width.min(height), expected_short);
        // Asking for nothing is an empty canvas, not a panic.
        let none = ToPngOptions {
            size: PngSize::FitLongEdge(0),
            ..ToPngOptions::default()
        };
        assert!(matches!(to_png(&db, none), Err(PngError::EmptyCanvas)));
    }

    #[test]
    fn a_stroke_in_pixels_is_turned_into_drawing_units_at_the_pixel_scale() {
        // 1.5 px at 4 px a unit is 0.375 units.
        assert_eq!(stroke_width(None, Some(1.5), 4.0, 0.01), 0.375);
        // An explicit SVG stroke width wins.
        assert_eq!(stroke_width(Some(0.2), Some(1.5), 4.0, 0.01), 0.2);
        // Neither: the SVG's own rule; nor is a nonsense pixel width used.
        assert_eq!(stroke_width(None, None, 4.0, 0.01), 0.01);
        assert_eq!(stroke_width(None, Some(-1.0), 4.0, 0.01), 0.01);
        assert_eq!(stroke_width(None, Some(f64::NAN), 4.0, 0.01), 0.01);
    }

    #[test]
    fn a_panic_while_drawing_comes_back_as_an_error_with_its_message() {
        let err = catch_panic(|| panic!("edges out of order")).unwrap_err();
        assert!(
            matches!(&err, PngError::RenderPanic(m) if m == "edges out of order"),
            "{err:?}"
        );
        let err = catch_panic(|| panic!("{} edges", 3)).unwrap_err();
        assert!(matches!(&err, PngError::RenderPanic(m) if m == "3 edges"));
        assert!(catch_panic(|| {}).is_ok());
    }

    #[test]
    fn a_document_the_rasterizer_cannot_scan_convert_still_returns() {
        // A line a million units long in a picture 0.002 units across,
        // drawn at 1568 px: 8e11 px of line. tiny-skia 0.12 panics inside
        // its scan converter on this. Whether a later version does is its
        // business -- what is asserted is only that the call *returns*,
        // with an image or with the error, and never takes the process.
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="-0.001 -0.001 0.002 0.002" stroke="black" stroke-width="0.00001"><line x1="-1000000" y1="0" x2="1000000" y2="0.0005" stroke-dasharray="4,2"/><line x1="0" y1="-1000000" x2="0.0003" y2="1000000"/></svg>"#;
        let result = svg_to_png(svg, 784_000.0);
        assert!(
            matches!(result, Ok(_) | Err(PngError::RenderPanic(_))),
            "{:?}",
            result.map(|png| png.len())
        );
    }

    #[test]
    fn the_hosts_fonts_are_scanned_once_per_process() {
        let first = usvg_options(&Fonts::System).fontdb;
        let second = usvg_options(&Fonts::System).fontdb;
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn custom_fonts_are_the_only_fonts() {
        // Nothing from the host: no bytes, no faces.
        let none = usvg_options(&Fonts::Custom(Vec::new()));
        assert_eq!(none.fontdb.len(), 0);
        // Bytes that are not a font load nothing and fail nothing.
        let junk: Arc<[u8]> = Arc::from(&b"not a font"[..]);
        let junk = usvg_options(&Fonts::Custom(vec![junk]));
        assert_eq!(junk.fontdb.len(), 0);
        // A text drawn with no font at all is a blank, not an error.
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 40 10"><text x="0" y="8" font-size="8">ABC</text></svg>"#;
        assert!(rasterize(svg, 1.0, 100, &Fonts::Custom(Vec::new()), Background::White).is_ok());
        // The bytes are not in the options' debug text.
        assert_eq!(
            format!("{:?}", Fonts::Custom(vec![Arc::from(&[0u8; 5][..])])),
            "Custom([5])"
        );
    }

    #[test]
    fn text_is_drawn_in_the_family_of_the_first_custom_face() {
        // Needs a real font file, which this crate does not ship; the
        // well-known locations of one on each CI host are tried in turn.
        let Some(bytes) = [
            "C:/Windows/Fonts/arial.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
        ]
        .iter()
        .find_map(|path| std::fs::read(path).ok()) else {
            eprintln!("no font file found on this host; nothing to check");
            return;
        };
        let options = usvg_options(&Fonts::Custom(vec![Arc::from(bytes)]));
        assert_eq!(options.fontdb.len(), 1);
        let face = options.fontdb.faces().next().expect("one face");
        assert_eq!(options.font_family, face.families[0].0);
        // A `<text>` naming no family resolves to that face: usvg turns the
        // text into glyph outlines, which it can only do with a face.
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 40 10"><text x="0" y="8" font-size="8">ABC</text></svg>"#;
        let tree = usvg::Tree::from_str(svg, &options).expect("parses");
        let usvg::Node::Text(text) = &tree.root().children()[0] else {
            panic!("a text node")
        };
        assert!(
            !text.flattened().children().is_empty(),
            "the text was shaped with the custom face"
        );
    }

    #[test]
    fn the_background_is_white_unless_asked_for_transparent() {
        // Read back one corner pixel, which no drawing touches: the
        // property asked for, not a comparison with a reference image.
        let db: CadDatabase =
            serde_json::from_str(include_str!("../tests/golden/g1.expected.json"))
                .expect("the golden model deserializes");
        let corner = |background| {
            let png = to_png(
                &db,
                ToPngOptions {
                    background,
                    ..ToPngOptions::default()
                },
            )
            .expect("renders")
            .png;
            let pixmap = tiny_skia::Pixmap::decode_png(&png).expect("a PNG");
            pixmap.pixel(0, 0).expect("a pixel")
        };
        let white = corner(Background::White);
        assert_eq!(
            (white.red(), white.green(), white.blue(), white.alpha()),
            (255, 255, 255, 255)
        );
        assert_eq!(corner(Background::Transparent).alpha(), 0);
    }

    #[test]
    fn svg_to_png_rejects_unparseable_svg() {
        let err = svg_to_png("not an svg document", 1.0).unwrap_err();
        assert!(matches!(err, PngError::InvalidSvg(_)));
    }

    /// Exercises the full `to_png` -> `to_svg` -> `svg_to_png` pipeline
    /// against a whole drawing, not just the SVG -> PNG half tested above.
    /// The drawing is the first golden case's expected model (see
    /// `tests/golden/README.md`): a real drawing's worth of entities, with no
    /// parser involved.
    #[test]
    fn to_png_renders_a_drawing_to_a_valid_png() {
        let db: CadDatabase =
            serde_json::from_str(include_str!("../tests/golden/g1.expected.json"))
                .expect("the golden model deserializes");
        let result = to_png(&db, ToPngOptions::default()).expect("to_png should succeed");
        assert!(result.png.starts_with(b"\x89PNG\r\n\x1a\n"));
        let (width, height) = png_dimensions(&result.png);
        assert!(width > 0 && height > 0);
    }
}
