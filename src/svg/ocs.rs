//! An entity's own coordinate system (OCS): the plane a CIRCLE or ARC is
//! drawn in, given by its extrusion direction.
//!
//! The format fixes the system's axes from the extrusion alone (the
//! "arbitrary axis algorithm"): the X axis is the world Y axis crossed with
//! the extrusion when the extrusion is within 1/64 of the world Z axis, and
//! the world Z axis crossed with it otherwise; the Y axis completes a
//! right-handed system. A mirror copy's extrusion (0, 0, -1) gives an X axis
//! of (-1, 0, 0) -- the world x reversed.

use uncad_model::model::{Point2D, Point3D};

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
}
