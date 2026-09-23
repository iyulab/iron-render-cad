//! SVG -> PNG rasterization, layered on top of [`crate::svg::to_svg`]'s
//! output via resvg/usvg/tiny-skia (all pure Rust, no system library
//! dependency -- see [`crate::svg`]'s own doc comment for the CAD -> SVG
//! side; this module only handles the SVG -> PNG raster step).
//!
//! A single `resvg` dependency is used rather than separate `usvg` /
//! `tiny-skia` / `fontdb` crates: `resvg` re-exports both (`resvg::usvg`,
//! `resvg::tiny_skia`, and `usvg::fontdb`), which keeps the three in lock
//! step instead of risking a version mismatch across independently pinned
//! crates.

use crate::svg::{to_svg, ToSvgOptions};
use resvg::tiny_skia;
use resvg::usvg::{self, fontdb};
use std::sync::{Arc, OnceLock};
use uncad_model::CadDatabase;

/// The largest either side of an image may be unless a caller says
/// otherwise: [`ToPngOptions::max_edge`]'s default, and the bound
/// [`svg_to_png`] applies. A pixel is four bytes, so the largest image this
/// allows is 8192 x 8192 x 4 = 256 MiB of pixels.
pub const DEFAULT_MAX_EDGE: u32 = 8192;

#[derive(Debug, Clone, PartialEq)]
pub struct ToPngOptions {
    pub svg: ToSvgOptions,
    /// Multiplies the SVG's own viewBox-derived pixel size -- e.g. `2.0`
    /// renders at 2x resolution. Must be finite and > 0.
    pub scale: f32,
    /// Neither side of the image may be more pixels than this; a larger
    /// request fails with [`PngError::TooLarge`] before any pixel memory is
    /// allocated. The viewBox comes from the drawing's own coordinates, so
    /// without a bound a file decides how much memory a render asks for.
    /// Default [`DEFAULT_MAX_EDGE`].
    pub max_edge: u32,
    /// The fonts text is drawn with. Default [`Fonts::System`].
    pub fonts: Fonts,
}

impl Default for ToPngOptions {
    fn default() -> Self {
        ToPngOptions {
            svg: ToSvgOptions::default(),
            scale: 1.0,
            max_edge: DEFAULT_MAX_EDGE,
            fonts: Fonts::default(),
        }
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

pub struct ToPngResult {
    pub png: Vec<u8>,
    pub unsupported_types: Vec<String>,
    /// See [`ToSvgResult::empty_blocks`](crate::ToSvgResult::empty_blocks).
    pub empty_blocks: Vec<String>,
    /// See
    /// [`ToSvgResult::unresolved_block_refs`](crate::ToSvgResult::unresolved_block_refs).
    pub unresolved_block_refs: Vec<uncad_model::EntityId>,
    /// See [`ToSvgResult::limits`](crate::ToSvgResult::limits).
    pub limits: crate::limits::LimitReport,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum PngError {
    /// usvg couldn't parse the intermediate SVG string -- since that string
    /// comes from this crate's own `to_svg`, this would indicate a bug
    /// there rather than bad input from a caller.
    InvalidSvg(usvg::Error),
    /// The requested pixel size (viewBox size * `scale`) rounds to zero in
    /// at least one dimension.
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
}

impl std::fmt::Display for PngError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PngError::InvalidSvg(e) => write!(f, "SVG parsing failed: {e}"),
            PngError::EmptyCanvas => write!(f, "render size is zero (check the scale)"),
            PngError::TooLarge {
                width,
                height,
                max_edge,
            } => write!(
                f,
                "render size {width}x{height} px exceeds the {max_edge} px limit (use a smaller scale, or raise max_edge)"
            ),
            PngError::Encode(e) => write!(f, "PNG encoding failed: {e}"),
            PngError::RenderPanic(e) => write!(f, "the rasterizer panicked: {e}"),
        }
    }
}
impl std::error::Error for PngError {}

/// Renders `db` straight to PNG bytes, via [`to_svg`] internally -- the
/// intermediate SVG text never touches disk.
///
/// Fonts come from the host's installed system fonts, loaded fresh on every
/// call (`fontdb::Database::load_system_fonts`, matching this crate having
/// no bundled font of its own). A host with no matching font installed
/// renders `<text>` entities (dimension/MTEXT labels) as blank rather than
/// erroring -- usvg treats an unresolved glyph as empty, not a parse
/// failure.
pub fn to_png(db: &CadDatabase, options: ToPngOptions) -> Result<ToPngResult, PngError> {
    let svg_result = to_svg(db, options.svg);
    let png = rasterize(
        &svg_result.svg,
        options.scale,
        options.max_edge,
        &options.fonts,
    )?;
    Ok(ToPngResult {
        png,
        unsupported_types: svg_result.unsupported_types,
        empty_blocks: svg_result.empty_blocks,
        unresolved_block_refs: svg_result.unresolved_block_refs,
        limits: svg_result.limits,
    })
}

/// Rasterizes an already-built SVG string to PNG bytes at `scale`x the
/// SVG's own viewBox-derived size. Split out from [`to_png`] so a caller
/// that already has an SVG string (e.g. from a separately cached
/// [`to_svg`] call) doesn't have to re-render the CAD geometry to get a
/// PNG out of it.
///
/// Neither side of the image may exceed [`DEFAULT_MAX_EDGE`] pixels; a
/// larger request fails with [`PngError::TooLarge`] instead of allocating.
/// Text is drawn with the host's fonts ([`Fonts::System`]).
pub fn svg_to_png(svg: &str, scale: f32) -> Result<Vec<u8>, PngError> {
    rasterize(svg, scale, DEFAULT_MAX_EDGE, &Fonts::System)
}

/// [`svg_to_png`] with an explicit bound on the image's sides.
///
/// The bound is checked before the pixmap exists: the pixmap's allocation
/// cannot fail gracefully (tiny-skia allocates it with `vec!`, and a request
/// the allocator refuses aborts the process), so a size nobody should
/// allocate has to be refused before asking.
fn rasterize(svg: &str, scale: f32, max_edge: u32, fonts: &Fonts) -> Result<Vec<u8>, PngError> {
    let tree = usvg::Tree::from_str(svg, &usvg_options(fonts)).map_err(PngError::InvalidSvg)?;

    let size = tree.size();
    let width = (size.width() * scale).round() as u32;
    let height = (size.height() * scale).round() as u32;
    if width > max_edge || height > max_edge {
        return Err(PngError::TooLarge {
            width,
            height,
            max_edge,
        });
    }
    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or(PngError::EmptyCanvas)?;

    catch_panic(|| {
        resvg::render(
            &tree,
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
        assert!(rasterize(edge, 1.0, 30, &Fonts::System).is_ok());
        assert!(matches!(
            rasterize(edge, 1.0, 29, &Fonts::System),
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
            scale: 1.0e-4,
            ..options
        };
        assert!(to_png(&db, small).is_ok());
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
        assert!(rasterize(svg, 1.0, 100, &Fonts::Custom(Vec::new())).is_ok());
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
