//! `Vec3` (prismarine `vec3`) — the extension methods node-vec3's `Vec3`
//! class carries that the engine's [`Vec3`] doesn't.
//!
//! The engine's `Vec3` covers `length`/`distance_to`/`dot`/`cross`/`min`/`max`,
//! `with_*`, `x_rot`/`y_rot`, and the arithmetic ops traits; [`Vec3Ext`]
//! fills the rest of the node-vec3 surface. Semantic traps, all inherited
//! from the engine rather than fixed here:
//!
//! - `v.xz()` is the engine's inherent method — it zeroes `y` and returns a
//!   `Vec3`; upstream's `xz()` returns the `[x, z]` pair. Use [`Vec3Ext::xy`]
//!   /[`Vec3Ext::yz`]/[`Vec3Ext::to_array`] for the upstream shapes.
//! - `v.scale(s)`/`v.normalize()`/`v.multiply(x,y,z)` are the engine's inherent
//!   *non-mutating* methods — they cover upstream's `scaled`/`unit`/
//!   `multiply` but under names that are mutating upstream. The mutating
//!   variants aren't provided (inherent names shadow any trait method);
//!   `+=`/`*=` and [`Vec3Ext::translate`] cover in-place updates.
//! - upstream `equals(other, error)` takes an error tolerance; Rust's
//!   `PartialEq` is exact. Compare components for tolerance checks.

use mineflayer_engine::Vec3;

/// `euclideanMod(numerator, denominator)` — node-vec3's helper, ported
/// literally (`n % d`, plus `d` when the remainder went negative; JS and
/// Rust `%` both keep the dividend's sign). For `d > 0` this is
/// `f64::rem_euclid`; for `d < 0` it returns the raw remainder, matching
/// upstream's quirk rather than `rem_euclid`'s divisor-signed result
/// (`euclideanMod(19, -16) = 3`, not -13). Upstream only ever divides by
/// positive constants (`Location`'s 16).
pub fn euclidean_mod(numerator: f64, denominator: f64) -> f64 {
    let result = numerator % denominator;
    if result < 0. {
        result + denominator
    } else {
        result
    }
}

/// node-vec3 `Vec3` methods on the engine's [`Vec3`]. Non-mutating methods take
/// `self` by value (`Vec3` is `Copy`), matching the engine's own `distance_to`
/// style; the mutating set takes `&mut self`.
pub trait Vec3Ext {
    /// `v.offset(dx, dy, dz)` — the most-used upstream method.
    fn offset(self, dx: f64, dy: f64, dz: f64) -> Vec3;
    /// `v.translate(dx, dy, dz)` — `offset` in place.
    fn translate(&mut self, dx: f64, dy: f64, dz: f64);
    /// `v.plus(other)`.
    fn plus(self, other: Vec3) -> Vec3;
    /// `v.minus(other)`.
    fn minus(self, other: Vec3) -> Vec3;
    /// `v.scaled(scalar)` — `v * scalar`. (the engine's inherent `scale` is the
    /// same method under the name upstream gives the mutating variant.)
    fn scaled(self, scalar: f64) -> Vec3;
    /// `v.floored()` — component-wise `floor`, still a `Vec3` (for the
    /// [`BlockPos`](mineflayer_engine::BlockPos) use `to_block_pos_floor`).
    fn floored(self) -> Vec3;
    /// `v.floor()` — `floored` in place.
    fn floor(&mut self);
    /// `v.rounded()` — component-wise JS `Math.round`: halves round toward
    /// +∞ (`Math.round(-1.5) === -1`), NOT Rust's `f64::round` (which gives
    /// -2 — halves away from zero).
    fn rounded(self) -> Vec3;
    /// `v.round()` — `rounded` in place.
    fn round(&mut self);
    /// `v.abs()` — component-wise.
    fn abs(self) -> Vec3;
    /// `v.modulus(other)` — component-wise [`euclidean_mod`], so negative
    /// components wrap into `0..other` like upstream.
    fn modulus(self, other: Vec3) -> Vec3;
    /// `v.volume()` — `x * y * z`.
    fn volume(self) -> f64;
    /// `v.norm()` — the Euclidean length (the engine calls it `length`).
    fn norm(self) -> f64;
    /// `v.unit()` — `scaled(1 / norm)` like upstream (multiply by the
    /// reciprocal — `v(0,3,4).unit().y` is `0.6000000000000001`, matching
    /// upstream bit-for-bit); the zero vector returns itself. Upstream's
    /// mutating `normalize` divides per component instead; the engine's
    /// inherent `normalize` does the same division but non-mutating (and
    /// returns `ZERO` under a 1e-5 length threshold upstream lacks).
    fn unit(self) -> Vec3;
    /// `v.angleTo(other)` — radians between the two vectors.
    fn angle_to(self, other: Vec3) -> f64;
    /// `v.manhattanDistanceTo(other)` — `|dx| + |dy| + |dz|`.
    fn manhattan_distance_to(self, other: Vec3) -> f64;
    /// `v.xyDistanceTo(other)` / `v.xzDistanceTo(other)` /
    /// `v.yzDistanceTo(other)`.
    fn xy_distance_to(self, other: Vec3) -> f64;
    /// See [`Vec3Ext::xy_distance_to`].
    fn xz_distance_to(self, other: Vec3) -> f64;
    /// See [`Vec3Ext::xy_distance_to`].
    fn yz_distance_to(self, other: Vec3) -> f64;
    /// `v.isZero()`.
    fn is_zero(&self) -> bool;
    /// `v.toArray()` — `[x, y, z]`.
    fn to_array(self) -> [f64; 3];
    /// `v.at(id)` — `to_array()[id]`.
    fn at(self, id: usize) -> f64;
    /// `v.xy()` — the `[x, y]` pair (upstream returns arrays, not Vec3s).
    fn xy(self) -> [f64; 2];
    /// `v.yz()`.
    fn yz(self) -> [f64; 2];
    /// `v.xzy()` — reorder components (upstream returns a Vec3 here).
    fn xzy(self) -> Vec3;
    /// `v.set(x, y, z)` — overwrite all components.
    fn set(&mut self, x: f64, y: f64, z: f64);
    /// `v.update(other)` — copy `other`'s components.
    fn update(&mut self, other: &Vec3);
}

impl Vec3Ext for Vec3 {
    fn offset(self, dx: f64, dy: f64, dz: f64) -> Vec3 {
        Vec3 {
            x: self.x + dx,
            y: self.y + dy,
            z: self.z + dz,
        }
    }
    fn translate(&mut self, dx: f64, dy: f64, dz: f64) {
        *self = self.offset(dx, dy, dz);
    }
    fn plus(self, other: Vec3) -> Vec3 {
        self.offset(other.x, other.y, other.z)
    }
    fn minus(self, other: Vec3) -> Vec3 {
        self.offset(-other.x, -other.y, -other.z)
    }
    fn scaled(self, scalar: f64) -> Vec3 {
        Vec3 {
            x: self.x * scalar,
            y: self.y * scalar,
            z: self.z * scalar,
        }
    }
    fn floored(self) -> Vec3 {
        Vec3 {
            x: self.x.floor(),
            y: self.y.floor(),
            z: self.z.floor(),
        }
    }
    fn floor(&mut self) {
        *self = self.floored();
    }
    fn rounded(self) -> Vec3 {
        Vec3 {
            x: (self.x + 0.5).floor(),
            y: (self.y + 0.5).floor(),
            z: (self.z + 0.5).floor(),
        }
    }
    fn round(&mut self) {
        *self = self.rounded();
    }
    fn abs(self) -> Vec3 {
        Vec3 {
            x: self.x.abs(),
            y: self.y.abs(),
            z: self.z.abs(),
        }
    }
    fn modulus(self, other: Vec3) -> Vec3 {
        Vec3 {
            x: euclidean_mod(self.x, other.x),
            y: euclidean_mod(self.y, other.y),
            z: euclidean_mod(self.z, other.z),
        }
    }
    fn volume(self) -> f64 {
        self.x * self.y * self.z
    }
    fn norm(self) -> f64 {
        self.length()
    }
    fn unit(self) -> Vec3 {
        // upstream `unit()` is `scaled(1 / norm)` — multiply by the
        // reciprocal, not per-component division (`normalize()` divides,
        // which differs by up to 1 ulp).
        let norm = self.norm();
        if norm != 0. {
            self.scaled(1. / norm)
        } else {
            self
        }
    }
    fn angle_to(self, other: Vec3) -> f64 {
        (self.dot(other) / (self.norm() * other.norm())).acos()
    }
    fn manhattan_distance_to(self, other: Vec3) -> f64 {
        let d = self.minus(other).abs();
        d.x + d.y + d.z
    }
    fn xy_distance_to(self, other: Vec3) -> f64 {
        (self.x - other.x).hypot(self.y - other.y)
    }
    fn xz_distance_to(self, other: Vec3) -> f64 {
        (self.x - other.x).hypot(self.z - other.z)
    }
    fn yz_distance_to(self, other: Vec3) -> f64 {
        (self.y - other.y).hypot(self.z - other.z)
    }
    fn is_zero(&self) -> bool {
        *self == Vec3::ZERO
    }
    fn to_array(self) -> [f64; 3] {
        [self.x, self.y, self.z]
    }
    fn at(self, id: usize) -> f64 {
        self.to_array()[id]
    }
    fn xy(self) -> [f64; 2] {
        [self.x, self.y]
    }
    fn yz(self) -> [f64; 2] {
        [self.y, self.z]
    }
    fn xzy(self) -> Vec3 {
        Vec3 {
            x: self.x,
            y: self.z,
            z: self.y,
        }
    }
    fn set(&mut self, x: f64, y: f64, z: f64) {
        *self = Vec3 { x, y, z };
    }
    fn update(&mut self, other: &Vec3) {
        *self = *other;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f64, y: f64, z: f64) -> Vec3 {
        Vec3 { x, y, z }
    }

    #[test]
    fn vec3_ext_matches_node_vec3() {
        let a = v(1., 2., 3.);
        assert_eq!(a.offset(4., 5., 6.), v(5., 7., 9.));
        assert_eq!(a.plus(v(1., 1., 1.)), v(2., 3., 4.));
        assert_eq!(a.minus(v(1., 1., 1.)), v(0., 1., 2.));
        assert_eq!(a.scaled(2.), v(2., 4., 6.));
        assert_eq!(v(1.7, -1.7, 0.5).floored(), v(1., -2., 0.));
        // Math.round rounds halves toward +∞: -1.5 -> -1 (not Rust's -2)
        assert_eq!(v(1.5, -1.5, 0.4).rounded(), v(2., -1., 0.));
        assert_eq!(v(-1., 2., -3.).abs(), v(1., 2., 3.));
        assert_eq!(v(2., 3., 4.).volume(), 24.);
        assert_eq!(v(3., 4., 0.).norm(), 5.);
        // upstream unit() = scaled(1/norm): 3 * (1/5) = 0.6000000000000001
        assert_eq!(v(0., 3., 4.).unit(), v(0., 0.6000000000000001, 0.8));
        assert_eq!(v(0., 0., 0.).unit(), v(0., 0., 0.));
        assert_eq!(v(1., 2., 3.).manhattan_distance_to(v(4., 0., 3.)), 5.);
        assert_eq!(a.to_array(), [1., 2., 3.]);
        assert_eq!(a.at(1), 2.);
        assert_eq!(a.xy(), [1., 2.]);
        assert_eq!(a.yz(), [2., 3.]);
        assert_eq!(a.xzy(), v(1., 3., 2.));
        assert!(!a.is_zero() && v(0., 0., 0.).is_zero());
        assert_eq!(
            v(1., 0., 0.).angle_to(v(0., 1., 0.)),
            std::f64::consts::FRAC_PI_2
        );
    }

    #[test]
    fn modulus_and_euclidean_mod_wrap_negatives() {
        // node-vec3 euclideanMod: -19 % 16 -> 13 (result + denominator)
        assert_eq!(euclidean_mod(-19., 16.), 13.);
        // negative denominator keeps the dividend's sign (upstream quirk;
        // f64::rem_euclid would give -13/-3 here)
        assert_eq!(euclidean_mod(19., -16.), 3.);
        assert_eq!(euclidean_mod(-19., -16.), -19.);
        assert_eq!(v(-19., 33., -1.).modulus(v(16., 16., 16.)), v(13., 1., 15.));
        // used by world::Location on negative chunk coords
        assert_eq!(v(-18.2, 70., 20.5).floored(), v(-19., 70., 20.));
    }

    #[test]
    fn mutating_variants_match() {
        let mut a = v(1., 2., 3.);
        a.translate(1., 1., 1.);
        assert_eq!(a, v(2., 3., 4.));
        a = a.scaled(2.);
        assert_eq!(a, v(4., 6., 8.));
        // The engine's inherent scale/normalize are non-mutating — upstream's
        // names for these are scaled/unit
        assert_eq!(a.scale(2.), v(8., 12., 16.));
        a.floor();
        assert_eq!(a, v(4., 6., 8.));
        a.set(0., 0., 0.);
        assert!(a.is_zero());
        a.update(&v(7., 8., 9.));
        assert_eq!(a, v(7., 8., 9.));
        a.round();
        assert_eq!(a, v(7., 8., 9.));
        a = a.unit();
        assert!((a.norm() - 1.).abs() < 1e-12);
    }
}
