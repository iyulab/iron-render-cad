//! Every bound the renderer puts on a number that came out of the drawing,
//! in one place, and the report of what those bounds left out.
//!
//! A drawing is untrusted input: a count, a scale or a spacing in it is
//! whatever the file says, and a corrupt one turns straight into an
//! allocation size or a loop bound. Left alone that is not a wrong picture
//! but a dead process. A block that holds one LINE and eight INSERTs of
//! itself expanded to 1,000,000 block references and a 139,000,107-byte SVG
//! before the reference budget stopped it; a real corrupt file (one flipped
//! byte of LibreDWG's `example_2000.dwg`) puts eight such references beside
//! fifty drawable entities.
//!
//! So each such number is capped, the caps are named here, and every render
//! says through [`LimitReport`] what a cap took away. The rule: **a
//! malformed file may cost a missing entity and a note saying so; it may
//! never cost the process.** The caps sit far above anything a real drawing
//! reaches, so a well-formed drawing renders exactly as it would without
//! them and its report is empty.

use uncad_model::EntityId;

/// How deep block references may nest before rendering stops following
/// them: a reference inside more than this many others is not expanded.
pub(crate) const MAX_BLOCK_REF_DEPTH: u32 = 20;

/// How many block references one render may expand in total.
///
/// [`MAX_BLOCK_REF_DEPTH`] bounds nesting but not breadth: a block holding
/// ten references to itself fans out to 10^20 expansions before the depth
/// cap is reached, and this budget -- decremented once per expansion and
/// never restored -- is what stops that, by nesting level 5.
pub(crate) const MAX_BLOCK_REFS: u32 = 100_000;

/// How many bytes of drawing body one render may emit.
///
/// The backstop behind the other caps, and the one that bounds the
/// allocation: whatever a file asks for, the SVG string stops growing near
/// this size. Checked before every entity at every level of the block walk,
/// so reaching it unwinds the whole walk.
pub(crate) const MAX_SVG_BODY_BYTES: usize = 64 * 1024 * 1024;

/// How many bytes of drawing body one top-level entity may emit: a quarter
/// of the document's budget, over 200,000 `<line>` elements.
///
/// A drawing whose content is one block placed once -- a bound reference, an
/// imported survey -- is a single top-level INSERT, so this is a cap on a
/// part of the picture, not on the picture. Reaching it stops that entity's
/// block expansion at the next entity boundary and *keeps* what was drawn;
/// the entity is reported as truncated.
pub(crate) const MAX_ENTITY_SVG_BYTES: usize = MAX_SVG_BODY_BYTES / 4;

// A `<line>` is around 64 bytes: one entity may draw over 200,000 of them.
const _: () = assert!(MAX_ENTITY_SVG_BYTES / 64 > 200_000);

/// How many points one entity may be drawn with: a polyline's vertices, a
/// spline's points, a hatch boundary's points times the paths drawn through
/// them, a wireframe's edges. An entity past this is left out whole -- no
/// raster image at any sane size resolves that many points, and drawing
/// them costs both the emitted text and the rasterizer's work.
pub(crate) const MAX_ENTITY_POINTS: usize = 100_000;

/// How much larger than the shape it fills a HATCH pattern's tile may be,
/// as a multiple of the boundary's diagonal.
///
/// A pattern is emitted as an SVG `<pattern>` whose tile is the pattern's
/// line spacing in drawing units, and the rasterizer allocates a pixmap for
/// that tile at the device scale of the filled element: a corrupt spacing of
/// 1e12 over a ten-unit boundary asks for a pixmap 1e11 pixels on a side. A
/// tile that much larger than the shape cannot show more than one line
/// anyway, so the pattern is dropped and the hatch keeps its outline.
pub(crate) const MAX_HATCH_TILE_SPAN: f64 = 16.0;

/// How far from the origin, in drawing units, an entity's measured extent
/// may reach on either axis.
///
/// 1e150 is a finite number, so the screen for `NaN` does not see it, but
/// the viewBox is built from the extents and the stroke width, the padding
/// and every dash length are derived from the viewBox: in a fuzzed
/// `example_2000.dwg`, one entity reaching 1e150 made a viewBox 1.45e150
/// units wide, and rasterizing its dashed strokes asked for ~1e149 dashes. No real drawing comes near the bound --
/// the Earth's circumference in micrometres is 4e13 -- so an entity past it
/// is not drawn and does not count towards the extent.
pub(crate) const MAX_WORLD_COORDINATE: f64 = 1e15;

/// How many entities [`LimitReport::dropped`] names before it stops
/// collecting. The counts stay exact past it; only the naming stops.
const MAX_NAMED: usize = 100;

/// Which cap acted on an entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Cap {
    /// More than 100,000 points. The entity is not drawn.
    EntityPoints,
    /// The document's 64 MiB of drawing body was already spent when this
    /// entity's turn came. It is not drawn.
    DocumentBytes,
    /// The entity drew 16 MiB, a quarter of the document's budget, and its
    /// block expansion was stopped there. It *is* drawn, but part of it is
    /// missing.
    EntityBytes,
    /// A block reference this entity is, or expands to, nested more than 20
    /// deep or came after the render's 100,000th expansion, and was not
    /// followed.
    BlockRefs,
    /// A HATCH's pattern tile was more than 16 times the size of the shape
    /// it fills. The hatch keeps its outline but not its pattern.
    HatchTile,
    /// A coordinate, size or angle the entity is drawn from is not a real
    /// number (`NaN`, infinite), or an angle is beyond a million radians,
    /// where it names no direction. It is not drawn.
    NotANumber,
    /// The entity's extent reaches more than 1e15 drawing units from the
    /// origin, past what a viewBox can be built from. It is not drawn and
    /// does not count towards the extent.
    OutOfRange,
}

/// One entity a cap acted on, so a report can name what is missing instead
/// of only counting it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Dropped {
    /// The entity's reference ID.
    pub id: EntityId,
    /// Its DXF type name.
    pub type_name: String,
    /// Which cap acted, and so whether the entity is missing entirely or
    /// only incomplete.
    pub cap: Cap,
}

/// What the caps took away from one render. Empty for every well-formed
/// drawing; see [`engaged`](Self::engaged).
///
/// Every count is of events, not of entities as the file lists them: a
/// block referenced twice and dropped twice counts twice. [`dropped`]
/// names which entities, once per entity and cap, the first 100 of them.
///
/// [`dropped`]: LimitReport::dropped
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LimitReport {
    /// Entities left out because they would have been drawn with more than
    /// 100,000 points.
    pub oversized_entities: usize,
    /// Block references not expanded because they nest more than 20 deep or
    /// came after the render's 100,000th expansion.
    pub block_refs_dropped: usize,
    /// HATCH pattern fills reduced to an outline because the pattern's tile
    /// was more than 16 times the size of the shape.
    pub hatch_patterns_dropped: usize,
    /// Entities not drawn at all because the document's 64 MiB of drawing
    /// body was already spent when their turn came.
    pub entities_dropped: usize,
    /// Top-level entities drawn only as far as the 16 MiB one entity may
    /// emit: in the picture, but with their block expansion cut short.
    pub truncated_parts: usize,
    /// Entities not drawn because a coordinate, size or angle they are drawn
    /// from is not a real number, or an angle names no direction.
    pub unreadable_entities: usize,
    /// Top-level entities not drawn because their extent reaches more than
    /// 1e15 drawing units from the origin.
    pub out_of_range_entities: usize,
    /// Which entities the counts above are about, in the order they were
    /// met: one entry per (entity, cap) pair, at most 100 of them.
    pub dropped: Vec<Dropped>,
}

impl LimitReport {
    /// Whether any cap acted at all -- whether the picture is missing
    /// something the model holds.
    pub fn engaged(&self) -> bool {
        *self != LimitReport::default()
    }

    /// Names one entity a cap acted on, unless it is already named for that
    /// cap or the list is full.
    pub(crate) fn note(&mut self, cap: Cap, id: EntityId, type_name: &str) {
        if self.dropped.len() >= MAX_NAMED
            || self.dropped.iter().any(|d| d.cap == cap && d.id == id)
        {
            return;
        }
        self.dropped.push(Dropped {
            id,
            type_name: type_name.to_string(),
            cap,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_report_is_not_engaged() {
        assert!(!LimitReport::default().engaged());
        let counted = LimitReport {
            hatch_patterns_dropped: 1,
            ..LimitReport::default()
        };
        assert!(counted.engaged());
    }

    #[test]
    fn an_entity_is_named_once_per_cap_and_the_list_has_a_ceiling() {
        let mut r = LimitReport::default();
        let id = EntityId::new(0x7F);
        // The same entity met twice under one cap is one entry; under two
        // caps it is two.
        r.note(Cap::BlockRefs, id, "INSERT");
        r.note(Cap::BlockRefs, id, "INSERT");
        r.note(Cap::EntityBytes, id, "INSERT");
        assert_eq!(r.dropped.len(), 2, "{:?}", r.dropped);
        for i in 0..(MAX_NAMED as u64) * 2 {
            r.note(Cap::DocumentBytes, EntityId::new(i), "LINE");
        }
        assert_eq!(r.dropped.len(), MAX_NAMED);
    }
}
