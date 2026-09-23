//! An entity's own coordinate system (OCS): the plane a planar entity --
//! CIRCLE, ARC, LWPOLYLINE and 2D POLYLINE, SOLID and TRACE, TEXT and
//! ATTRIB -- is drawn in, given by its extrusion direction.
//!
//! The format fixes the system's axes from the extrusion alone (the
//! "arbitrary axis algorithm"): the X axis is the world Y axis crossed with
//! the extrusion when the extrusion is within 1/64 of the world Z axis, and
//! the world Z axis crossed with it otherwise; the Y axis completes a
//! right-handed system. A mirror copy's extrusion (0, 0, -1) gives an X axis
//! of (-1, 0, 0) -- the world x reversed.
//!
//! An INSERT's placement is the model's own arithmetic
//! ([`Affine2::from_insert`]), which takes its block to the world the same
//! way; [`Ocs::map`] is that map for an entity drawn through an affine map
//! of its own, a text.

use uncad_model::model::{Point2D, Point3D};
use uncad_model::Affine2;

/// The axes of the coordinate system whose Z axis is `extrusion`, as world
/// vectors. `None` for a zero or non-finite extrusion, which names no plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Ocs {
    x: Point3D,
    y: Point3D,
    z: Point3D,
}

impl Ocs {
    pub fn of(extrusion: Point3D) -> Option<Self> {
        let z = normalized(extrusion)?;
        let limit = 1.0 / 64.0;
        let world = if z.x.abs() < limit && z.y.abs() < limit {
            Point3D {
                x: 0.0,
                y: 1.0,
                z: 0.0,
            }
        } else {
            Point3D {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            }
        };
        let x = normalized(cross(world, z))?;
        let y = cross(z, x);
        Some(Ocs { x, y, z })
    }

    /// `true` when the plane is the world's own -- the default extrusion,
    /// give or take the rounding files write it with.
    pub fn is_world(self) -> bool {
        self.flat() && self.z.z > 0.0
    }

    /// `true` when the plane is parallel to the world XY plane, facing
    /// either way.
    pub fn flat(self) -> bool {
        self.z.x.abs().max(self.z.y.abs()) <= 1e-9 * self.z.z.abs()
    }

    /// A point of this system in world coordinates, seen from above (its
    /// world z dropped).
    pub fn to_world_xy(self, p: Point3D) -> Point2D {
        Point2D {
            x: self.x.x * p.x + self.y.x * p.y + self.z.x * p.z,
            y: self.x.y * p.x + self.y.y * p.y + self.z.y * p.z,
        }
    }

    /// [`to_world_xy`](Self::to_world_xy) for the points at height `z` in
    /// this system, as an affine map of their `(x, y)`: what a text, whose
    /// glyphs are laid out along axes of their own, is drawn through.
    pub fn map(self, z: f64) -> Affine2 {
        Affine2 {
            a: self.x.x,
            b: self.x.y,
            c: self.y.x,
            d: self.y.y,
            e: self.z.x * z,
            f: self.z.y * z,
        }
    }
}

/// Whether an entity stated in the plane of `extrusion` at height `z` has
/// real numbers for everything its plane is drawn from: the extrusion
/// always, and the height wherever the plane is not the world's own. A
/// mirrored or tilted plane is taken to the world through
/// [`Ocs::to_world_xy`], whose arithmetic takes the height in -- even where
/// it moves nothing in plan, `0 * NaN` is `NaN`. In the world's own plane
/// the height is not used.
pub(super) fn numbers_are_real(extrusion: &Point3D, z: f64) -> bool {
    [extrusion.x, extrusion.y, extrusion.z]
        .iter()
        .all(|v| v.is_finite())
        && (z.is_finite() || Ocs::of(*extrusion).is_none_or(Ocs::is_world))
}

fn cross(a: Point3D, b: Point3D) -> Point3D {
    Point3D {
        x: a.y * b.z - a.z * b.y,
        y: a.z * b.x - a.x * b.z,
        z: a.x * b.y - a.y * b.x,
    }
}

fn normalized(v: Point3D) -> Option<Point3D> {
    let len = (v.x * v.x + v.y * v.y + v.z * v.z).sqrt();
    (len > 0.0 && len.is_finite()).then(|| Point3D {
        x: v.x / len,
        y: v.y / len,
        z: v.z / len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p3(x: f64, y: f64, z: f64) -> Point3D {
        Point3D { x, y, z }
    }

    #[test]
    fn the_default_extrusion_is_the_world() {
        let o = Ocs::of(p3(0.0, 0.0, 1.0)).unwrap();
        assert!(o.is_world());
        assert_eq!(o.to_world_xy(p3(3.0, 4.0, 5.0)), Point2D { x: 3.0, y: 4.0 });
    }

    #[test]
    fn a_mirror_copy_reverses_the_world_x() {
        let o = Ocs::of(p3(0.0, 0.0, -1.0)).unwrap();
        assert!(o.flat() && !o.is_world());
        assert_eq!(
            o.to_world_xy(p3(3.0, 4.0, 0.0)),
            Point2D { x: -3.0, y: 4.0 }
        );
    }

    #[test]
    fn a_tilted_plane_follows_the_arbitrary_axis_algorithm() {
        // Extrusion along world X: far from Z, so X axis = Z x N = (0, 1, 0)
        // and Y axis = N x X = (0, 0, 1). An OCS point (2, 3, 0) is at world
        // (0, 2, 3); seen from above, (0, 2).
        let o = Ocs::of(p3(1.0, 0.0, 0.0)).unwrap();
        assert!(!o.flat());
        assert_eq!(o.to_world_xy(p3(2.0, 3.0, 0.0)), Point2D { x: 0.0, y: 2.0 });
    }

    #[test]
    fn no_plane_without_a_direction() {
        assert!(Ocs::of(p3(0.0, 0.0, 0.0)).is_none());
        assert!(Ocs::of(p3(f64::NAN, 0.0, 1.0)).is_none());
    }

    #[test]
    fn a_normal_of_any_length_or_with_rounding_noise_is_still_its_plane() {
        assert!(Ocs::of(p3(0.0, 0.0, 3.0)).unwrap().is_world());
        assert!(Ocs::of(p3(1e-17, -2e-17, 1.0)).unwrap().is_world());
        let mirror = Ocs::of(p3(1e-17, 0.0, -1.0)).unwrap();
        assert!(mirror.flat() && !mirror.is_world());
    }

    #[test]
    fn the_map_at_a_height_is_the_plane_seen_from_above() {
        // Normal (1, 0, 0): a point (x, y) at height z is the world point
        // (z, x, y) -- seen from above, (z, x).
        let m = Ocs::of(p3(1.0, 0.0, 0.0)).unwrap().map(7.0);
        let p = m.apply(Point2D { x: 2.0, y: 3.0 });
        assert!(
            (p.x - 7.0).abs() < 1e-12 && (p.y - 2.0).abs() < 1e-12,
            "{p:?}"
        );
    }

    #[test]
    fn the_map_is_the_one_the_model_places_a_block_reference_through() {
        use uncad_model::model::{Confidence, EntityCommon, EntityId, InsertEntity, Origin, Ref};
        // An INSERT at the plane's origin, unscaled and unturned, is placed
        // by exactly the plane's map: the two cannot disagree.
        for normal in [
            p3(0.0, 0.0, -1.0),
            p3(1.0, 0.0, 1.0),
            p3(0.3, -0.2, 0.9),
            p3(0.01, 0.005, 1.0),
            p3(0.0, 1.0, 0.0),
        ] {
            let z = 2.5;
            let insert = InsertEntity {
                common: EntityCommon {
                    id: EntityId::new(1),
                    origin: Origin::Vector,
                    confidence: Confidence::High,
                    source_handle: Ref::Absent,
                    layer: Ref::Absent,
                    color_index: 7,
                    true_color: None,
                    invisible: false,
                },
                block_name: Ref::Absent,
                insertion_point: p3(0.0, 0.0, z),
                scale: p3(1.0, 1.0, 1.0),
                rotation: 0.0,
                attribs: Vec::new(),
                extrusion: normal,
            };
            let model = Affine2::from_insert(&insert);
            let ours = Ocs::of(normal).unwrap().map(z);
            for (a, b) in [
                (model.a, ours.a),
                (model.b, ours.b),
                (model.c, ours.c),
                (model.d, ours.d),
                (model.e, ours.e),
                (model.f, ours.f),
            ] {
                assert!((a - b).abs() < 1e-12, "{normal:?}: {model:?} vs {ours:?}");
            }
        }
    }

    #[test]
    fn a_planes_height_has_to_be_a_number_unless_the_plane_is_the_worlds() {
        assert!(numbers_are_real(&p3(0.0, 0.0, 1.0), f64::NAN));
        // Mirrored, the height adds nothing in plan -- but it is multiplied
        // in, and a NaN reaches the point.
        assert!(!numbers_are_real(&p3(0.0, 0.0, -1.0), f64::NAN));
        let mirror = Ocs::of(p3(0.0, 0.0, -1.0)).unwrap();
        assert!(mirror.to_world_xy(p3(1.0, 1.0, f64::NAN)).x.is_nan());
        assert!(!numbers_are_real(&p3(1.0, 0.0, 1.0), f64::NAN));
        assert!(!numbers_are_real(&p3(0.0, f64::INFINITY, 1.0), 0.0));
    }
}
