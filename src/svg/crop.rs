//! What a picture shows: the rectangle of the world its viewBox frames,
//! chosen from the extents the render measured ([`Crop`]), and what that
//! choice leaves out of the picture ([`CropReport`]).
//!
//! The guard ([`Crop::Guarded`]) is the one rule here that decides by
//! itself what is not drawn. A drawing is framed on everything it holds
//! except a few outliers -- at most `max(3, 1 %)` entities. Each pass sets
//! aside the largest entities and the ones farthest from the median centre
//! (at most a quarter of the drawing) and measures them against the rest:
//! a candidate whose diagonal is over 20 times the rest's is a scale
//! outlier (a block reference scaled 3256 times beside a drawing of a few
//! hundred units); one more than 20 of the rest's diagonals away is a far
//! outlier (a stray point a million units out) -- but never more than a
//! fifth of the drawing, so a notes block a drawing-width away stays.
//! Passes repeat until nothing changes. Nothing else is ever set aside.
//!
//! A caller may know the drawing's extent from elsewhere -- a file's
//! header states one -- and hand it over as `stated`: it is taken instead
//! of the guarded extent when it is a sane rectangle that contains at least
//! 90 % of the entities the guard kept, is at most four times their area,
//! and contains more entities than their bounds do. The outliers it
//! reaches are then shown after all.

use super::bounds::{bbox_of, diag, dominant_cluster_box, intersects, rect_gap, Box2D};
use super::scene::Rect;
use uncad_model::model::EntityId;

/// How a render chooses the world rectangle its picture shows, from the
/// extents its top-level entities measured (a hidden entity measures
/// nothing). The chosen rectangle is then padded by
/// [`ToSvgOptions::padding`](crate::ToSvgOptions::padding).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[non_exhaustive]
pub enum Crop {
    /// The dominant spatially connected cluster of entities: groups whose
    /// corners touch, the largest by count and size absorbing its near
    /// neighbours. The default -- 0.1's `outlier_trim: true`.
    #[default]
    Cluster,
    /// Every entity that measured an extent -- 0.1's `outlier_trim: false`.
    Everything,
    /// Every entity but the outliers the guard finds (see the module
    /// docs), which are left out of the picture -- not drawn at all, since
    /// a block reference thousands of times the drawing's size would cross
    /// it whatever the viewBox -- and reported. `stated` is an extent the
    /// caller holds from elsewhere, taken instead when it passes the test
    /// in the module docs.
    Guarded { stated: Option<Rect> },
    /// Exactly this world rectangle. One that is not a finite rectangle
    /// (`min <= max` on both axes) frames nothing, like an empty drawing.
    Window(Rect),
}

/// Why an entity is not in the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum LeftOutReason {
    /// [`Crop::Guarded`] set it aside: its diagonal is over 20 times the
    /// rest of the drawing's. Not drawn.
    ScaleOutlier,
    /// [`Crop::Guarded`] set it aside: it lies more than 20 of the rest's
    /// diagonals away from the rest. Not drawn.
    FarOutlier,
    /// Its extent does not reach the picture's viewBox. It is in the
    /// document all the same, outside what the viewBox shows.
    OutsideView,
}

/// A top-level entity the picture does not show.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct LeftOut {
    pub id: EntityId,
    /// Its DXF type name.
    pub type_name: String,
    /// The world box it measured.
    pub extent: Rect,
    pub reason: LeftOutReason,
}

/// How the picture's rectangle was chosen, and what it leaves out.
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct CropReport {
    /// The world rectangle the picture is framed on, before padding: the
    /// bounds of the entities it was chosen from (the cluster, every
    /// entity, the ones the guard kept), the stated extent when that was
    /// taken, the window of [`Crop::Window`], or -- for a layout that
    /// states its sheet -- the sheet. `None` when nothing was measured.
    pub content: Option<Rect>,
    /// Whether [`Crop::Guarded`]'s stated extent was taken.
    pub stated_taken: bool,
    /// The top-level entities the picture does not show, in drawing order.
    /// A construction line (RAY, XLINE) is never among them: it reaches
    /// every picture it crosses.
    pub left_out: Vec<LeftOut>,
}

/// What [`choose`] decided over a list of extents.
pub(super) struct Choice {
    /// See [`CropReport::content`].
    pub(super) content: Option<Box2D>,
    pub(super) stated_taken: bool,
    /// The extents set aside, by index into the list, with the reason.
    pub(super) set_aside: Vec<(usize, LeftOutReason)>,
}

/// Decides the unpadded rectangle `crop` frames over `extents` (the
/// measured top-level extents, in drawing order).
pub(super) fn choose(extents: &[Box2D], crop: Crop) -> Choice {
    let framed = |content: Option<Box2D>| Choice {
        content,
        stated_taken: false,
        set_aside: Vec::new(),
    };
    if let Crop::Window(r) = crop {
        let finite = [r.min_x, r.min_y, r.max_x, r.max_y]
            .iter()
            .all(|v| v.is_finite());
        let ordered = r.min_x <= r.max_x && r.min_y <= r.max_y;
        return framed((finite && ordered).then(|| Box2D::from(r)));
    }
    if extents.is_empty() {
        return framed(None);
    }
    let raw = bbox_of(extents);
    match crop {
        Crop::Everything | Crop::Window(_) => framed(Some(raw)),
        Crop::Cluster => framed(Some(if extents.len() > 2 {
            dominant_cluster_box(extents).unwrap_or(raw)
        } else {
            raw
        })),
        Crop::Guarded { stated } => guarded(extents, stated),
    }
}

/// [`Crop::Guarded`] over a non-empty list of extents.
fn guarded(extents: &[Box2D], stated: Option<Rect>) -> Choice {
    let out = outliers(extents);
    let kept: Vec<&Box2D> = (0..extents.len())
        .filter(|i| !out.iter().any(|(j, _)| j == i))
        .map(|i| &extents[i])
        .collect();
    // The guard keeps at least three quarters of the drawing, so `kept` is
    // never empty here.
    let content = bbox_of(&kept.iter().copied().copied().collect::<Vec<_>>());
    let covers = |r: &Box2D| extents.iter().filter(|e| contains(r, e)).count();
    match stated.and_then(|s| stated_candidate(s, &content, &kept)) {
        Some(s) if covers(&s) > covers(&content) => {
            // The stated extent reaches entities the guard set aside: they
            // are in the picture after all.
            let set_aside = out
                .into_iter()
                .filter(|(i, _)| !intersects(&s, &extents[*i]))
                .collect();
            Choice {
                content: Some(s),
                stated_taken: true,
                set_aside,
            }
        }
        _ => Choice {
            content: Some(content),
            stated_taken: false,
            set_aside: out,
        },
    }
}

/// Whether `inner` lies entirely inside `outer`, edges included.
fn contains(outer: &Box2D, inner: &Box2D) -> bool {
    inner.min_x >= outer.min_x
        && inner.max_x <= outer.max_x
        && inner.min_y >= outer.min_y
        && inner.max_y <= outer.max_y
}

fn area(b: &Box2D) -> f64 {
    (b.max_x - b.min_x) * (b.max_y - b.min_y)
}

/// The stated extent, when it may be taken: a finite rectangle of positive
/// area within 1e15 units of the origin (headers carry runaway values),
/// containing at least 90 % of `kept`, and at most four times the area of
/// `content`, their bounds.
fn stated_candidate(stated: Rect, content: &Box2D, kept: &[&Box2D]) -> Option<Box2D> {
    let s = Box2D::from(stated);
    let sane = [s.min_x, s.min_y, s.max_x, s.max_y]
        .iter()
        .all(|v| v.is_finite() && v.abs() < 1e15)
        && s.max_x > s.min_x
        && s.max_y > s.min_y;
    if !sane {
        return None;
    }
    if area(content) > 0.0 && area(&s) > 4.0 * area(content) {
        return None;
    }
    let covered = kept.iter().filter(|r| contains(&s, r)).count();
    (covered * 10 >= kept.len() * 9).then_some(s)
}

/// The guard: the indices of the extents the picture should not stretch
/// to, each with its reason, in index order. Empty when everything belongs
/// together. See the module docs for the rule.
fn outliers(extents: &[Box2D]) -> Vec<(usize, LeftOutReason)> {
    let n = extents.len();
    if n < 3 {
        return Vec::new();
    }
    let limit = 3.max((n as f64 * 0.01).ceil() as usize);
    let mut out: Vec<(usize, LeftOutReason)> = Vec::new();
    // One pass can only judge what it set aside; a second huge entity hides
    // behind the first until that one is out, so passes repeat until
    // nothing changes (at most `limit` entities can go in total).
    loop {
        let remaining: Vec<usize> = (0..n)
            .filter(|i| !out.iter().any(|(j, _)| j == i))
            .collect();
        let found = outlier_pass(extents, &remaining, limit - out.len().min(limit));
        if found.is_empty() {
            break;
        }
        out.extend(found);
        if out.len() >= limit {
            break;
        }
    }
    out.sort_by_key(|(i, _)| *i);
    out
}

/// One pass of the guard over `remaining` (indices into `extents`).
///
/// The candidates are the `k` largest entities and the `k` farthest from
/// the median centre, `k = min(budget, remaining / 4)`, so that at least
/// three quarters of the drawing stay as the *rest*. With `R` the rest's
/// bounding box and `D` its diagonal (or the median entity diagonal when
/// larger): a candidate whose own diagonal exceeds 20 D is a scale
/// outlier; one whose gap to `R` exceeds 20 D is a far outlier. Far
/// outliers are set aside only when there are at most `budget` of them and
/// they are no more than a fifth of the drawing.
fn outlier_pass(
    extents: &[Box2D],
    remaining: &[usize],
    budget: usize,
) -> Vec<(usize, LeftOutReason)> {
    let m = remaining.len();
    let k = budget.min(m / 4);
    if m < 4 || k == 0 {
        return Vec::new();
    }
    let centre = |i: usize| {
        let r = &extents[i];
        ((r.min_x + r.max_x) / 2.0, (r.min_y + r.max_y) / 2.0)
    };
    let mut xs: Vec<f64> = remaining.iter().map(|i| centre(*i).0).collect();
    let mut ys: Vec<f64> = remaining.iter().map(|i| centre(*i).1).collect();
    xs.sort_by(f64::total_cmp);
    ys.sort_by(f64::total_cmp);
    let (mx, my) = (xs[m / 2], ys[m / 2]);
    let distance = |i: usize| {
        let (x, y) = centre(i);
        (x - mx).hypot(y - my)
    };
    let mut by_diag: Vec<usize> = remaining.to_vec();
    by_diag.sort_by(|a, b| {
        diag(&extents[*b])
            .total_cmp(&diag(&extents[*a]))
            .then(a.cmp(b))
    });
    let mut by_dist: Vec<usize> = remaining.to_vec();
    by_dist.sort_by(|a, b| distance(*b).total_cmp(&distance(*a)).then(a.cmp(b)));
    let mut candidates: Vec<usize> = by_diag[..k].to_vec();
    for i in &by_dist[..k] {
        if !candidates.contains(i) {
            candidates.push(*i);
        }
    }
    let rest: Vec<Box2D> = remaining
        .iter()
        .filter(|i| !candidates.contains(i))
        .map(|i| extents[*i])
        .collect();
    if rest.len() < 3 {
        return Vec::new();
    }
    let rest_box = bbox_of(&rest);
    let mut diagonals: Vec<f64> = rest.iter().map(diag).collect();
    diagonals.sort_by(f64::total_cmp);
    let d = diag(&rest_box)
        .max(diagonals[diagonals.len() / 2])
        .max(1e-9);

    let mut out: Vec<(usize, LeftOutReason)> = Vec::new();
    let mut scale: Vec<usize> = candidates
        .iter()
        .copied()
        .filter(|i| diag(&extents[*i]) > 20.0 * d)
        .collect();
    scale.sort_by(|a, b| diag(&extents[*b]).total_cmp(&diag(&extents[*a])));
    scale.truncate(budget);
    out.extend(scale.iter().map(|i| (*i, LeftOutReason::ScaleOutlier)));
    let far: Vec<usize> = candidates
        .iter()
        .copied()
        .filter(|i| !scale.contains(i) && rect_gap(&extents[*i], &rest_box) > 20.0 * d)
        .collect();
    if !far.is_empty() && far.len() + scale.len() <= budget && far.len() * 5 <= m {
        out.extend(far.into_iter().map(|i| (i, LeftOutReason::FarOutlier)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Box2D {
        Box2D {
            min_x,
            max_x,
            min_y,
            max_y,
        }
    }

    fn corners(b: Box2D) -> [f64; 4] {
        [b.min_x, b.min_y, b.max_x, b.max_y]
    }

    /// Four lines around a 10 x 10 square at the origin.
    fn square() -> Vec<Box2D> {
        vec![
            bx(0.0, 0.0, 10.0, 0.0),
            bx(10.0, 0.0, 10.0, 10.0),
            bx(0.0, 10.0, 10.0, 10.0),
            bx(0.0, 0.0, 0.0, 10.0),
        ]
    }

    fn scaled(boxes: Vec<Box2D>, k: f64) -> Vec<Box2D> {
        boxes
            .into_iter()
            .map(|b| bx(b.min_x * k, b.min_y * k, b.max_x * k, b.max_y * k))
            .collect()
    }

    fn guard(stated: Option<Rect>) -> Crop {
        Crop::Guarded { stated }
    }

    #[test]
    fn a_huge_far_line_is_a_scale_outlier_and_a_small_far_dot_a_far_outlier() {
        let mut extents = square();
        extents.push(bx(1e6, 1e6, 2e6, 2e6));
        assert_eq!(outliers(&extents), [(4, LeftOutReason::ScaleOutlier)]);

        let mut extents = square();
        extents.push(bx(1e6, 1e6, 1e6 + 1.0, 1e6 + 1.0));
        assert_eq!(outliers(&extents), [(4, LeftOutReason::FarOutlier)]);

        // A frame around the drawing stays: 50x the content is not 20x
        // everything else once the frame's own lines count.
        let mut extents = square();
        extents.extend(scaled(square(), 50.0));
        assert!(outliers(&extents).is_empty());
        // One entity 50x the rest is out; so are two of them.
        let mut extents = square();
        extents.push(bx(0.0, 0.0, 500.0, 500.0));
        assert_eq!(outliers(&extents), [(4, LeftOutReason::ScaleOutlier)]);
        extents.push(bx(-500.0, -500.0, 0.0, 0.0));
        assert_eq!(
            outliers(&extents),
            [
                (4, LeftOutReason::ScaleOutlier),
                (5, LeftOutReason::ScaleOutlier)
            ]
        );
        // Two squares 30 units apart are one drawing.
        let mut extents = square();
        extents.extend(
            square()
                .into_iter()
                .map(|b| bx(b.min_x + 30.0, b.min_y, b.max_x + 30.0, b.max_y)),
        );
        assert!(outliers(&extents).is_empty());
        assert!(outliers(&square()[..2]).is_empty(), "fewer than 3: nothing");
    }

    /// Three 10 x 10 squares side by side: twelve lines.
    fn squares() -> Vec<Box2D> {
        [0.0, 20.0, 40.0]
            .into_iter()
            .flat_map(|dx| {
                square()
                    .into_iter()
                    .map(move |b| bx(b.min_x + dx, b.min_y, b.max_x + dx, b.max_y))
            })
            .collect()
    }

    #[test]
    fn a_fifth_of_the_drawing_is_never_far() {
        // Twelve lines and one dot a million units away: the dot is out.
        let mut extents = squares();
        extents.push(bx(1e6, 1e6, 1e6 + 1.0, 1e6 + 1.0));
        assert_eq!(outliers(&extents), [(12, LeftOutReason::FarOutlier)]);
        // Four dots out of sixteen are a quarter of the drawing: it is just
        // that big, and nothing is set aside.
        extents.push(bx(1e6, 0.0, 1e6 + 1.0, 1.0));
        extents.push(bx(0.0, 1e6, 1.0, 1e6 + 1.0));
        extents.push(bx(-1e6, -1e6, -1e6 + 1.0, -1e6 + 1.0));
        assert!(outliers(&extents).is_empty());
        // A sparse drawing: a few small lines and labels 30 diagonals out
        // are one drawing (the labels are a third of it).
        let mut sparse = square();
        sparse.push(bx(0.0, 20.0, 0.0, 20.0));
        sparse.push(bx(50.0, 50.0, 50.0, 50.0));
        sparse.push(bx(100.0, 100.0, 100.0, 100.0));
        assert!(outliers(&sparse).is_empty());
    }

    #[test]
    fn a_stated_extent_is_a_candidate_only_when_sane_covering_and_not_too_large() {
        let extents = square();
        let kept: Vec<&Box2D> = extents.iter().collect();
        let content = bx(0.0, 0.0, 10.0, 10.0);
        let ok = Rect::new(-1.0, -1.0, 11.0, 11.0);
        assert_eq!(
            stated_candidate(ok, &content, &kept).map(corners),
            Some([-1.0, -1.0, 11.0, 11.0])
        );
        // Too large: 30 x 30 is 9x the content area.
        let large = Rect::new(-10.0, -10.0, 20.0, 20.0);
        assert!(stated_candidate(large, &content, &kept).is_none());
        // Contains only 2 of 4 lines.
        let partial = Rect::new(0.0, 0.0, 10.0, 5.0);
        assert!(stated_candidate(partial, &content, &kept).is_none());
        // Runaway, not a number, and empty.
        for bad in [
            Rect::new(1e20, 0.0, 1e21, 1.0),
            Rect::new(f64::NAN, 0.0, 1.0, 1.0),
            Rect::new(5.0, 5.0, 5.0, 5.0),
        ] {
            assert!(stated_candidate(bad, &content, &kept).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn every_crop_frames_what_it_says() {
        let mut extents = square();
        extents.push(bx(1e6, 1e6, 2e6, 2e6));
        let stated = Rect::new(-1.0, -1.0, 11.0, 11.0);

        // The stated extent holds the same 4 entities as the content: the
        // content is framed.
        let g = choose(&extents, guard(Some(stated)));
        assert_eq!(g.content.map(corners), Some([0.0, 0.0, 10.0, 10.0]));
        assert!(!g.stated_taken);
        assert_eq!(g.set_aside, [(4, LeftOutReason::ScaleOutlier)]);

        let e = choose(&extents, Crop::Everything);
        assert_eq!(e.content.map(corners), Some([0.0, 0.0, 2e6, 2e6]));
        assert!(e.set_aside.is_empty());

        // The cluster trim cannot tell: the outlier alone outscores the
        // square (count times diagonal), and no cluster holds a majority, so
        // it keeps everything -- the case the guard is for. It never sets
        // anything aside.
        let c = choose(&extents, Crop::Cluster);
        assert_eq!(c.content.map(corners), Some([0.0, 0.0, 2e6, 2e6]));
        assert!(c.set_aside.is_empty());
        // With a majority, it frames the square.
        let mut spread = square();
        spread.push(bx(1e6, 1e6, 1e6 + 1.0, 1e6 + 1.0));
        let c = choose(&spread, Crop::Cluster);
        assert_eq!(c.content.map(corners), Some([0.0, 0.0, 10.0, 10.0]));

        let window = Rect::new(0.0, 0.0, 5.0, 5.0);
        let w = choose(&extents, Crop::Window(window));
        assert_eq!(w.content.map(corners), Some([0.0, 0.0, 5.0, 5.0]));
        assert!(w.set_aside.is_empty());
        // A window that is no rectangle frames nothing.
        for bad in [
            Rect::new(1.0, 0.0, 0.0, 1.0),
            Rect::new(0.0, 0.0, f64::INFINITY, 1.0),
        ] {
            assert!(choose(&extents, Crop::Window(bad)).content.is_none());
        }

        for crop in [Crop::Cluster, Crop::Everything, guard(None)] {
            assert!(choose(&[], crop).content.is_none());
        }
    }

    #[test]
    fn the_guard_takes_the_stated_extent_when_it_covers_more() {
        // Twelve collinear lines on y = 0 (x 0..118) and a dot a million
        // units away. The guard sets the dot aside as far, so the content
        // is the zero-area (0,0)-(118,0); with no content area the 4x limit
        // does not apply, the stated extent contains all 12 kept lines and
        // 13 entities to the content's 12: it is taken, and the dot, inside
        // it after all, is no longer set aside.
        let mut extents: Vec<Box2D> = (0..12)
            .map(|i| {
                let x = 10.0 * f64::from(i);
                bx(x, 0.0, x + 8.0, 0.0)
            })
            .collect();
        extents.push(bx(1e6, 1e6, 1e6 + 1.0, 1e6 + 1.0));
        assert_eq!(outliers(&extents), [(12, LeftOutReason::FarOutlier)]);
        let stated = Rect::new(-1.0, -1.0, 1e6 + 2.0, 1e6 + 2.0);
        let g = choose(&extents, guard(Some(stated)));
        assert!(g.stated_taken);
        assert_eq!(
            g.content.map(corners),
            Some([-1.0, -1.0, 1e6 + 2.0, 1e6 + 2.0])
        );
        assert!(g.set_aside.is_empty());

        // Not only for collinear drawings: a 10 x 10 square of twelve lines
        // (the square plus eight one-unit diagonals inside it), a 150 x 150
        // entity and a 210 x 210 one. With 14 entities the guard may set 3
        // aside; the rest is the 10 x 10 square (D = 14.14), so the 210 x
        // 210 entity (diagonal 297 > 20 D = 283) is a scale outlier while
        // the 150 x 150 one (diagonal 212) is kept. The content is then
        // (0,0)-(150,150), area 22500; the stated (0,0)-(210,210) is 44100
        // <= 4 x 22500, holds all 13 kept entities and contains 14 > 13.
        let mut extents = square();
        for i in 0..8 {
            let o = f64::from(i);
            extents.push(bx(o, o, o + 1.0, o + 1.0));
        }
        extents.push(bx(0.0, 0.0, 150.0, 150.0));
        extents.push(bx(0.0, 0.0, 210.0, 210.0));
        assert_eq!(outliers(&extents), [(13, LeftOutReason::ScaleOutlier)]);
        let g = choose(&extents, guard(Some(Rect::new(0.0, 0.0, 210.0, 210.0))));
        assert!(g.stated_taken);
        assert!(g.set_aside.is_empty());
    }

    #[test]
    fn the_guard_refuses_a_stated_extent_over_four_times_the_content() {
        // The square and a far dot: the guard sets the dot aside, the
        // content is the 10 x 10 square, and a stated extent reaching the
        // dot is a million units square -- far more than 4x the content's
        // area, so it is not taken though it would contain one entity more.
        let mut extents = square();
        extents.push(bx(1e6, 1e6, 1e6 + 1.0, 1e6 + 1.0));
        let stated = Rect::new(0.0, 0.0, 1e6 + 1.0, 1e6 + 1.0);
        let g = choose(&extents, guard(Some(stated)));
        assert!(!g.stated_taken);
        assert_eq!(g.content.map(corners), Some([0.0, 0.0, 10.0, 10.0]));
        assert_eq!(g.set_aside, [(4, LeftOutReason::FarOutlier)]);
    }
}
