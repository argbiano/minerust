// Nether portals: their frame, where a traveller arrives, and building a portal on the
// other side when none is close enough, the way vanilla's PortalForcer does.

use crate::{
    server::Server,
    world::{Chunk, Dimension, blocks},
};
use anyhow::{Context, Result};
use std::{collections::HashMap, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// The frame runs along x; travellers walk through it along z.
    X,
    Z,
}

pub type Edit = ([i32; 3], u16);

#[derive(Clone, Debug)]
pub struct Portal {
    pub dimension: Dimension,
    /// The lowest inner (portal) block with the smaller coordinate along the axis.
    pub origin: [i32; 3],
    pub axis: Axis,
}

impl Portal {
    /// `a` along the frame, `b` up, relative to `origin`.
    fn at(&self, a: i32, b: i32) -> [i32; 3] {
        let [x, y, z] = self.origin;
        match self.axis {
            Axis::X => [x + a, y + b, z],
            Axis::Z => [x, y + b, z + a],
        }
    }

    /// Feet position in the middle of the portal.
    pub fn arrival(&self) -> [f64; 3] {
        let [x, y, z] = self.origin;
        match self.axis {
            Axis::X => [x as f64 + 1.0, y as f64, z as f64 + 0.5],
            Axis::Z => [x as f64 + 0.5, y as f64, z as f64 + 1.0],
        }
    }

    /// The 4×5 obsidian frame with its 2×3 portal blocks.
    pub fn blocks(&self) -> Vec<Edit> {
        let inner = match self.axis {
            Axis::X => blocks::NETHER_PORTAL_X,
            Axis::Z => blocks::NETHER_PORTAL_Z,
        };
        let mut out = Vec::with_capacity(20);
        for a in -1..=2 {
            for b in -1..=3 {
                let is_inner = (0..=1).contains(&a) && (0..=2).contains(&b);
                out.push((self.at(a, b), if is_inner { inner } else { blocks::OBSIDIAN }));
            }
        }
        out
    }

    pub fn horizontal_distance_sq(&self, x: f64, z: f64) -> f64 {
        let [px, _, pz] = self.arrival();
        (px - x).powi(2) + (pz - z).powi(2)
    }

    pub fn key(&self) -> PortalKey {
        (self.dimension, self.origin)
    }

    /// Whether `block` is one of the portal blocks inside the frame.
    pub fn contains(&self, dimension: Dimension, block: [i32; 3]) -> bool {
        dimension == self.dimension && (0..=1).any(|a| (0..=2).any(|b| self.at(a, b) == block))
    }
}

pub type PortalKey = (Dimension, [i32; 3]);

/// A portal three blocks in front of someone at `pos` looking along `yaw`.
pub fn in_front(dimension: Dimension, pos: [f64; 3], yaw: f32) -> Portal {
    let rad = (yaw as f64).to_radians();
    let (fx, fz) = (-rad.sin(), rad.cos());
    let x = (pos[0] + fx * 3.0).floor() as i32;
    let z = (pos[2] + fz * 3.0).floor() as i32;
    // Face the frame: looking along z means the frame spans x.
    let axis = if fz.abs() >= fx.abs() { Axis::X } else { Axis::Z };
    Portal { dimension, origin: [x, pos[1].floor() as i32, z], axis }
}

/// How far vanilla looks for an existing portal on the destination side.
pub fn search_radius(dimension: Dimension) -> f64 {
    match dimension {
        Dimension::Nether => 16.0,
        Dimension::Overworld => 128.0,
    }
}

/// Where in `dimension` a traveller from `from` at `pos` should come out.
pub fn scaled_target(from: Dimension, to: Dimension, pos: [f64; 3]) -> [f64; 3] {
    let scale = from.scale() / to.scale();
    let border = 29_999_984.0;
    [(pos[0] * scale).clamp(-border, border), pos[1], (pos[2] * scale).clamp(-border, border)]
}

const SEARCH: i32 = 16;

/// Generates the destination area and picks a spot for a new portal there, with the
/// block changes needed to build it.
pub async fn build_destination(
    server: Arc<Server>,
    dimension: Dimension,
    target: [f64; 3],
) -> Result<(Portal, Vec<Edit>)> {
    let (tx, tz) = (target[0].floor() as i32, target[2].floor() as i32);
    let mut tasks = tokio::task::JoinSet::new();
    for cx in (tx - SEARCH - 2) >> 4..=(tx + SEARCH + 2) >> 4 {
        for cz in (tz - SEARCH - 2) >> 4..=(tz + SEARCH + 2) >> 4 {
            let server = server.clone();
            tasks.spawn(async move { server.level(dimension).chunk(cx, cz).await });
        }
    }
    let mut chunks = HashMap::new();
    while let Some(chunk) = tasks.join_next().await {
        let chunk = chunk.context("loading the portal destination")??;
        chunks.insert((chunk.x, chunk.z), chunk);
    }
    let info = server.level(dimension).info();
    let (lo, hi) = match dimension {
        // Stay under the bedrock roof and above the lava sea.
        Dimension::Nether => (info.min_y + 32, info.min_y + 120),
        Dimension::Overworld => (info.min_y + 5, info.min_y + info.height as i32 - 6),
    };
    Ok(place(dimension, target, lo, hi, &chunks))
}

fn place(
    dimension: Dimension,
    target: [f64; 3],
    lo: i32,
    hi: i32,
    chunks: &HashMap<(i32, i32), Arc<Chunk>>,
) -> (Portal, Vec<Edit>) {
    let block = |x: i32, y: i32, z: i32| {
        chunks.get(&(x >> 4, z >> 4)).map_or(blocks::STONE, |c| c.block((x & 15) as usize, y, (z & 15) as usize))
    };
    let solid_floor = |x, y, z| {
        let b = block(x, y, z);
        blocks::is_solid(b) && !blocks::is_liquid(b)
    };
    let open = |x, y, z| {
        let b = block(x, y, z);
        !blocks::is_solid(b) && !blocks::is_liquid(b)
    };
    // Overworld heights mean nothing in the nether: aim inside the usable range.
    let (tx, ty, tz) = (target[0].floor() as i32, (target[1].floor() as i32).clamp(lo, hi), target[2].floor() as i32);
    let fits = |x: i32, y: i32, z: i32| {
        (-1..=2).all(|a| solid_floor(x + a, y - 1, z))
            && (-1..=2).all(|a| (0..=3).all(|b| open(x + a, y + b, z)))
            && (0..=1).all(|a| (0..=1).all(|b| open(x + a, y + b, z - 1) && open(x + a, y + b, z + 1)))
    };

    let mut best: Option<(i64, [i32; 3])> = None;
    for dx in -SEARCH..=SEARCH {
        for dz in -SEARCH..=SEARCH {
            let horizontal = (dx * dx + dz * dz) as i64;
            if best.is_some_and(|(score, _)| horizontal >= score) {
                continue;
            }
            let (x, z) = (tx + dx, tz + dz);
            for y in lo..=hi {
                if fits(x, y, z) {
                    let score = horizontal + ((y - ty) as i64).pow(2);
                    if best.is_none_or(|(s, _)| score < s) {
                        best = Some((score, [x, y, z]));
                    }
                }
            }
        }
    }

    let mut edits = Vec::new();
    let origin = match best {
        Some((_, origin)) => origin,
        None => {
            // Nowhere to stand: hollow out a pocket on an obsidian platform, as vanilla does.
            let y = ty.clamp(lo, hi);
            for a in -1..=2 {
                for side in -1..=1 {
                    edits.push(([tx + a, y - 1, tz + side], blocks::OBSIDIAN));
                    for b in 0..=3 {
                        edits.push(([tx + a, y + b, tz + side], blocks::AIR));
                    }
                }
            }
            [tx, y, tz]
        }
    };
    let portal = Portal { dimension, origin, axis: Axis::X };
    edits.extend(portal.blocks());
    (portal, edits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::Chunk;

    #[test]
    fn frame_shape() {
        let p = Portal { dimension: Dimension::Overworld, origin: [10, 64, 20], axis: Axis::X };
        let blocks = p.blocks();
        assert_eq!(blocks.len(), 20);
        assert_eq!(blocks.iter().filter(|(_, s)| *s == crate::world::blocks::NETHER_PORTAL_X).count(), 6);
        assert!(blocks.contains(&([9, 63, 20], crate::world::blocks::OBSIDIAN)));
        assert!(blocks.contains(&([11, 66, 20], crate::world::blocks::NETHER_PORTAL_X)));
        assert!(blocks.contains(&([12, 67, 20], crate::world::blocks::OBSIDIAN)));
        assert_eq!(p.arrival(), [11.0, 64.0, 20.5]);
    }

    #[test]
    fn faces_the_player() {
        let south = in_front(Dimension::Overworld, [0.5, 64.0, 0.5], 0.0);
        assert_eq!((south.axis, south.origin), (Axis::X, [0, 64, 3]));
        let west = in_front(Dimension::Overworld, [0.5, 64.0, 0.5], 90.0);
        assert_eq!((west.axis, west.origin), (Axis::Z, [-3, 64, 0]));
    }

    #[test]
    fn coordinates_scale_by_eight() {
        assert_eq!(scaled_target(Dimension::Overworld, Dimension::Nether, [80.0, 70.0, -16.0]), [10.0, 70.0, -2.0]);
        assert_eq!(scaled_target(Dimension::Nether, Dimension::Overworld, [10.0, 70.0, -2.0]), [80.0, 70.0, -16.0]);
    }

    fn flat_chunks(top: i32) -> HashMap<(i32, i32), Arc<Chunk>> {
        let mut chunks = HashMap::new();
        for cx in -3..=3 {
            for cz in -3..=3 {
                let mut c = Chunk::empty(cx, cz, 0, 256, 0);
                for x in 0..16 {
                    for z in 0..16 {
                        for y in 0..top {
                            c.set_block(x, y, z, crate::world::blocks::NETHERRACK);
                        }
                    }
                }
                chunks.insert((cx, cz), Arc::new(c));
            }
        }
        chunks
    }

    #[test]
    fn places_on_open_ground_or_builds_a_pocket() {
        let (portal, edits) = place(Dimension::Nether, [5.0, 80.0, 5.0], 32, 120, &flat_chunks(70));
        assert_eq!(portal.origin, [5, 70, 5], "stands on the ground nearest the target");
        assert_eq!(edits.len(), 20, "only the frame is built");

        let (portal, edits) = place(Dimension::Nether, [5.0, 200.0, 5.0], 32, 120, &flat_chunks(200));
        assert_eq!(portal.origin, [5, 120, 5], "target height is clamped into range");
        assert!(edits.contains(&([5, 119, 4], crate::world::blocks::OBSIDIAN)), "platform under the pocket");
        assert!(edits.contains(&([5, 120, 4], crate::world::blocks::AIR)), "pocket is hollowed out");
    }

    #[test]
    fn prefers_nearby_floors_to_matching_heights() {
        // Floor at 70 everywhere; the target height (from the overworld) is far above.
        let (portal, _) = place(Dimension::Nether, [5.0, 144.0, 5.0], 32, 120, &flat_chunks(70));
        assert_eq!(portal.origin, [5, 70, 5]);
    }
}
