//! Line of sight through a map's static collision, as TypeScript
//! `bot/BotVision.ts` `canSee` asks it: a point ray with the player content
//! mask through `engine/physics/SweepCollisionBackend.ts`. The geometry comes
//! from `tools/bake-washoi-sight.mjs`. Brushes are clipped as in the backend's
//! `traceBrush`, and patch facets are their tessellated triangles. Doors and
//! lifts are not in the file and never block sight.

use std::{
    io::{self, Read},
    path::Path,
};

use serde::Deserialize;

const MAGIC: &[u8; 8] = b"SHAGSGT1";
const MAX_FILE: u64 = 64 * 1024 * 1024;
const MAX_HEADER: usize = 4096;
/// `traceBrush` backs every crossing off by this much.
const SURFACE_EPSILON: f64 = 0.03125;
/// Leaves of the bounding volume tree hold at most this many solids.
const LEAF_SIZE: usize = 4;

pub type Point = [f64; 3];

pub struct Sight {
    map: String,
    planes: Vec<[f64; 4]>,
    solids: Vec<Solid>,
    nodes: Vec<Node>,
}

#[derive(Deserialize)]
struct Header {
    map: String,
}

#[derive(Clone, Copy)]
enum Shape {
    /// A range of `planes`.
    Brush(u32, u32),
    Triangle([Point; 3]),
}

#[derive(Clone, Copy)]
struct Solid {
    shape: Shape,
    bounds: Bounds,
}

#[derive(Clone, Copy)]
struct Bounds {
    min: Point,
    max: Point,
}

struct Node {
    bounds: Bounds,
    /// A leaf holds `solids[first..first + count]`. An inner node's children
    /// are the next node and `first`.
    first: u32,
    count: u32,
}

impl Sight {
    pub fn load(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(MAX_FILE + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_FILE {
            return Err(invalid("sight file exceeds size limit"));
        }
        Self::parse(&bytes)
    }

    pub fn map(&self) -> &str {
        &self.map
    }

    fn parse(bytes: &[u8]) -> io::Result<Self> {
        let mut reader = Reader(bytes);
        if reader.take(MAGIC.len())? != MAGIC {
            return Err(invalid("not a SHAGADELIC sight file"));
        }
        let header_length = reader.count()?;
        if header_length > MAX_HEADER {
            return Err(invalid("sight header exceeds size limit"));
        }
        let header: Header =
            serde_json::from_slice(reader.take(header_length)?).map_err(io::Error::from)?;
        if header.map.trim().is_empty() {
            return Err(invalid("sight file names no map"));
        }
        let plane_count = reader.count()?;
        let planes = reader.floats(plane_count, |values: [f64; 4]| {
            let plane = [values[0], values[1], values[2], values[3]];
            let length = (plane[0] * plane[0] + plane[1] * plane[1] + plane[2] * plane[2]).sqrt();
            ((length - 1.0).abs() < 1e-3).then_some(plane)
        })?;
        let brush_count = reader.count()?;
        let mut solids = Vec::with_capacity(brush_count);
        let mut first = 0usize;
        for _ in 0..brush_count {
            let count = reader.count()?;
            let end = first
                .checked_add(count)
                .filter(|&end| count > 0 && end <= planes.len())
                .ok_or_else(|| invalid("sight brush planes out of range"))?;
            solids.push(Solid {
                shape: Shape::Brush(first as u32, end as u32),
                bounds: brush_bounds(&planes[first..end]),
            });
            first = end;
        }
        if first != planes.len() {
            return Err(invalid("sight file has unused planes"));
        }
        let triangle_count = reader.count()?;
        let triangles = reader.floats(triangle_count, |values: [f64; 9]| {
            Some([
                [values[0], values[1], values[2]],
                [values[3], values[4], values[5]],
                [values[6], values[7], values[8]],
            ])
        })?;
        solids.extend(triangles.into_iter().map(|corners| Solid {
            shape: Shape::Triangle(corners),
            bounds: Bounds::around(&corners),
        }));
        if !reader.0.is_empty() {
            return Err(invalid("sight file has trailing bytes"));
        }
        let mut sight = Self {
            map: header.map,
            planes,
            solids,
            nodes: Vec::new(),
        };
        sight.build();
        Ok(sight)
    }

    /// Whether nothing static blocks the segment.
    pub fn clear(&self, from: Point, to: Point) -> bool {
        if !from.iter().chain(&to).all(|value| value.is_finite()) {
            return false;
        }
        let delta = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
        let inverse = delta.map(|d| 1.0 / d);
        let mut stack = Vec::with_capacity(64);
        if !self.nodes.is_empty() {
            stack.push(0usize);
        }
        while let Some(index) = stack.pop() {
            let node = &self.nodes[index];
            if !node.bounds.crossed(from, inverse) {
                continue;
            }
            if node.count == 0 {
                stack.push(node.first as usize);
                stack.push(index + 1);
                continue;
            }
            let leaf = &self.solids[node.first as usize..(node.first + node.count) as usize];
            if leaf.iter().any(|solid| {
                solid.bounds.crossed(from, inverse) && self.blocks(solid.shape, from, to, delta)
            }) {
                return false;
            }
        }
        true
    }

    fn blocks(&self, shape: Shape, from: Point, to: Point, delta: Point) -> bool {
        match shape {
            Shape::Brush(first, end) => {
                brush_blocks(&self.planes[first as usize..end as usize], from, to)
            }
            Shape::Triangle(corners) => triangle_blocks(corners, from, delta),
        }
    }

    fn build(&mut self) {
        if self.solids.is_empty() {
            return;
        }
        let count = self.solids.len();
        self.split(0, count);
    }

    /// Median split along the widest axis of the solids' centers.
    fn split(&mut self, first: usize, end: usize) -> usize {
        let index = self.nodes.len();
        let solids = &mut self.solids[first..end];
        let bounds = solids
            .iter()
            .skip(1)
            .fold(solids[0].bounds, |bounds, solid| bounds.union(solid.bounds));
        self.nodes.push(Node {
            bounds,
            first: first as u32,
            count: (end - first) as u32,
        });
        if solids.len() <= LEAF_SIZE {
            return index;
        }
        let centers = solids.iter().map(|solid| solid.bounds.center()).fold(
            Bounds::empty(),
            |bounds, center| {
                bounds.union(Bounds {
                    min: center,
                    max: center,
                })
            },
        );
        let axis = (0..3)
            .max_by(|&a, &b| {
                (centers.max[a] - centers.min[a]).total_cmp(&(centers.max[b] - centers.min[b]))
            })
            .unwrap_or(0);
        let middle = solids.len() / 2;
        solids.select_nth_unstable_by(middle, |a, b| {
            a.bounds.center()[axis].total_cmp(&b.bounds.center()[axis])
        });
        let middle = first + middle;
        self.split(first, middle);
        let right = self.split(middle, end);
        self.nodes[index].first = right as u32;
        self.nodes[index].count = 0;
        index
    }
}

/// `traceBrush` for a point: the segment is blocked if it enters the brush
/// before leaving it, or never leaves it.
fn brush_blocks(planes: &[[f64; 4]], from: Point, to: Point) -> bool {
    let mut enter = -1.0f64;
    let mut leave = 1.0f64;
    let mut starts_out = false;
    let mut ends_out = false;
    for plane in planes {
        let start = dot(plane, from) - plane[3];
        let end = dot(plane, to) - plane[3];
        starts_out |= start > 0.0;
        ends_out |= end > 0.0;
        if start > 0.0 && end > 0.0 {
            return false;
        }
        if start <= 0.0 && end <= 0.0 {
            continue;
        }
        if start > end {
            enter = enter.max((start - SURFACE_EPSILON) / (start - end));
        } else {
            leave = leave.min((start + SURFACE_EPSILON) / (start - end));
        }
    }
    if !starts_out {
        return !ends_out;
    }
    enter < leave && enter > -1.0 && enter < 1.0
}

/// Möller–Trumbore, from either side.
fn triangle_blocks([a, b, c]: [Point; 3], from: Point, delta: Point) -> bool {
    let ab = sub(b, a);
    let ac = sub(c, a);
    let p = cross(delta, ac);
    let determinant = dot3(ab, p);
    if determinant.abs() < 1e-9 {
        return false;
    }
    let inverse = 1.0 / determinant;
    let offset = sub(from, a);
    let u = dot3(offset, p) * inverse;
    if !(0.0..=1.0).contains(&u) {
        return false;
    }
    let q = cross(offset, ab);
    let v = dot3(delta, q) * inverse;
    if v < 0.0 || u + v > 1.0 {
        return false;
    }
    let t = dot3(ac, q) * inverse;
    (0.0..1.0).contains(&t)
}

/// Q3 brushes carry their six axial bevel planes. A missing one leaves that
/// side unbounded.
fn brush_bounds(planes: &[[f64; 4]]) -> Bounds {
    let mut bounds = Bounds {
        min: [f64::NEG_INFINITY; 3],
        max: [f64::INFINITY; 3],
    };
    for plane in planes {
        for axis in 0..3 {
            if plane[axis] == 1.0 {
                bounds.max[axis] = bounds.max[axis].min(plane[3]);
            } else if plane[axis] == -1.0 {
                bounds.min[axis] = bounds.min[axis].max(-plane[3]);
            }
        }
    }
    bounds
}

impl Bounds {
    fn empty() -> Self {
        Self {
            min: [f64::INFINITY; 3],
            max: [f64::NEG_INFINITY; 3],
        }
    }

    fn around(points: &[Point]) -> Self {
        points.iter().fold(Self::empty(), |bounds, &point| {
            bounds.union(Self {
                min: point,
                max: point,
            })
        })
    }

    fn union(self, other: Self) -> Self {
        Self {
            min: [0, 1, 2].map(|axis| self.min[axis].min(other.min[axis])),
            max: [0, 1, 2].map(|axis| self.max[axis].max(other.max[axis])),
        }
    }

    fn center(&self) -> Point {
        [0, 1, 2].map(|axis| {
            let (min, max) = (self.min[axis], self.max[axis]);
            if min.is_finite() && max.is_finite() {
                (min + max) / 2.0
            } else if min.is_finite() {
                min
            } else if max.is_finite() {
                max
            } else {
                0.0
            }
        })
    }

    /// Slab test for the segment `from + t * delta`, `t` in [0, 1], padded by
    /// the brush epsilon.
    fn crossed(&self, from: Point, inverse: Point) -> bool {
        let (mut near, mut far) = (0.0f64, 1.0f64);
        for axis in 0..3 {
            let (min, max) = (
                self.min[axis] - SURFACE_EPSILON,
                self.max[axis] + SURFACE_EPSILON,
            );
            if inverse[axis].is_infinite() {
                if from[axis] < min || from[axis] > max {
                    return false;
                }
                continue;
            }
            let a = (min - from[axis]) * inverse[axis];
            let b = (max - from[axis]) * inverse[axis];
            let (a, b) = if a <= b { (a, b) } else { (b, a) };
            near = if a > near { a } else { near };
            far = if b < far { b } else { far };
            if near > far {
                return false;
            }
        }
        true
    }
}

fn dot(plane: &[f64; 4], point: Point) -> f64 {
    plane[0] * point[0] + plane[1] * point[1] + plane[2] * point[2]
}

fn dot3(a: Point, b: Point) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sub(a: Point, b: Point) -> Point {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: Point, b: Point) -> Point {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> io::Result<&'a [u8]> {
        if self.0.len() < length {
            return Err(invalid("sight file is truncated"));
        }
        let (head, tail) = self.0.split_at(length);
        self.0 = tail;
        Ok(head)
    }

    fn count(&mut self) -> io::Result<usize> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize)
    }

    /// `count` records of `N` finite f32 values, each checked by `make`.
    fn floats<T, const N: usize>(
        &mut self,
        count: usize,
        make: impl Fn([f64; N]) -> Option<T>,
    ) -> io::Result<Vec<T>> {
        let length = count
            .checked_mul(4 * N)
            .ok_or_else(|| invalid("sight file is truncated"))?;
        let bytes = self.take(length)?;
        bytes
            .chunks_exact(4 * N)
            .map(|record| {
                let mut values = [0.0; N];
                for (value, bytes) in values.iter_mut().zip(record.as_chunks::<4>().0) {
                    *value = f64::from(f32::from_le_bytes(*bytes));
                }
                values
                    .iter()
                    .all(|value| value.is_finite())
                    .then(|| make(values))
                    .flatten()
                    .ok_or_else(|| invalid("sight file has invalid geometry"))
            })
            .collect()
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A sight file in the baker's format.
    pub(in super::super) fn encode(
        map: &str,
        brushes: &[Vec<[f32; 4]>],
        triangles: &[[f32; 9]],
    ) -> Vec<u8> {
        let header = format!(r#"{{"map":"{map}","bsp_sha256":"0"}}"#);
        let mut bytes = MAGIC.to_vec();
        bytes.extend((header.len() as u32).to_le_bytes());
        bytes.extend(header.as_bytes());
        let planes: Vec<_> = brushes.iter().flatten().collect();
        bytes.extend((planes.len() as u32).to_le_bytes());
        bytes.extend(
            planes
                .iter()
                .flat_map(|plane| plane.iter().flat_map(|v| v.to_le_bytes())),
        );
        bytes.extend((brushes.len() as u32).to_le_bytes());
        bytes.extend(
            brushes
                .iter()
                .flat_map(|brush| (brush.len() as u32).to_le_bytes()),
        );
        bytes.extend((triangles.len() as u32).to_le_bytes());
        bytes.extend(triangles.iter().flatten().flat_map(|v| v.to_le_bytes()));
        bytes
    }

    /// An axis-aligned box brush.
    pub(in super::super) fn cube(min: [f32; 3], max: [f32; 3]) -> Vec<[f32; 4]> {
        vec![
            [1.0, 0.0, 0.0, max[0]],
            [-1.0, 0.0, 0.0, -min[0]],
            [0.0, 1.0, 0.0, max[1]],
            [0.0, -1.0, 0.0, -min[1]],
            [0.0, 0.0, 1.0, max[2]],
            [0.0, 0.0, -1.0, -min[2]],
        ]
    }

    #[test]
    fn walls_and_patches_block_sight_and_open_air_does_not() {
        let mut brushes: Vec<_> = (0..20)
            .map(|i| {
                let x = 1000.0 + 100.0 * i as f32;
                cube([x, 0.0, 0.0], [x + 10.0, 10.0, 10.0])
            })
            .collect();
        brushes.push(cube([-8.0, -100.0, -100.0], [8.0, 100.0, 100.0]));
        let sight = Sight::parse(&encode(
            "test",
            &brushes,
            &[[0.0, 0.0, 200.0, 0.0, 100.0, 300.0, 100.0, 0.0, 300.0]],
        ))
        .unwrap();
        assert_eq!(sight.map(), "test");
        // Through the wall, and through the slanted patch.
        assert!(!sight.clear([-50.0, 0.0, 0.0], [50.0, 0.0, 0.0]));
        assert!(!sight.clear([20.0, 20.0, 200.0], [20.0, 20.0, 300.0]));
        assert!(!sight.clear([20.0, 20.0, 300.0], [20.0, 20.0, 200.0]));
        // Past it, short of it, and between the small boxes.
        assert!(sight.clear([-50.0, 150.0, 0.0], [50.0, 150.0, 0.0]));
        assert!(sight.clear([-50.0, 0.0, 0.0], [-20.0, 0.0, 0.0]));
        assert!(sight.clear([1015.0, 5.0, -50.0], [1015.0, 5.0, 50.0]));
        assert!(!sight.clear([1005.0, 5.0, -50.0], [1005.0, 5.0, 50.0]));
        // Leaving a brush is not blocked; staying inside one is.
        assert!(sight.clear([0.0, 0.0, 0.0], [50.0, 0.0, 0.0]));
        assert!(!sight.clear([0.0, 0.0, 0.0], [1.0, 0.0, 0.0]));
        assert!(!sight.clear([f64::NAN, 0.0, 0.0], [1.0, 0.0, 0.0]));
    }

    #[test]
    fn rejects_damaged_files() {
        let good = encode("test", &[cube([0.0; 3], [1.0; 3])], &[]);
        assert!(Sight::parse(&good).is_ok());
        assert!(Sight::parse(&good[..good.len() - 1]).is_err());
        let mut trailing = good.clone();
        trailing.push(0);
        assert!(Sight::parse(&trailing).is_err());
        let mut tilted = encode("test", &[vec![[2.0, 0.0, 0.0, 1.0]]], &[]);
        assert!(Sight::parse(&tilted).is_err());
        tilted[0] = b'X';
        assert!(Sight::parse(&tilted).is_err());
        assert!(Sight::parse(&encode(" ", &[], &[])).is_err());
    }

    /// Compares with SHAG's own trace on sight lines the baker answered.
    #[test]
    #[ignore = "needs SHAGADELIC_TEST_WASHOI_SIGHT and SHAGADELIC_TEST_WASHOI_SIGHT_LINES"]
    fn agrees_with_shag_on_baked_sight_lines() {
        let sight = Sight::load(std::env::var("SHAGADELIC_TEST_WASHOI_SIGHT").unwrap()).unwrap();
        #[derive(Deserialize)]
        struct Lines {
            map: String,
            lines: Vec<(Point, Point, bool)>,
        }
        let path = std::env::var("SHAGADELIC_TEST_WASHOI_SIGHT_LINES").unwrap();
        let lines: Lines = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(lines.map, sight.map());
        let wrong: Vec<_> = lines
            .lines
            .iter()
            .filter(|(from, to, clear)| sight.clear(*from, *to) != *clear)
            .collect();
        assert!(
            wrong.is_empty(),
            "{} of {} differ: {wrong:?}",
            wrong.len(),
            lines.lines.len()
        );
    }
}
