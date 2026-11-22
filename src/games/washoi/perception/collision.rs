//! A map's collision, for the movement rollouts that prove a move before the
//! bot makes it (`navigation/rollout.rs`). Washoi's server traces and moves
//! players through the map's BSP brushes and patches; this is the same sweep,
//! `shagger_shared::physics::sweep_collision_backend`, over the same lumps.
//!
//! The file is what `tools/bake-washoi-collision.mjs` writes: "SHAGCOL1", a
//! u32 header length, a UTF-8 JSON header `{ map, bsp_sha256 }` naming the map
//! and the full BSP's SHA-256, then the BSP with the lumps collision never
//! reads emptied. A BSP names no map, so the hash ties it to the navigation
//! baked from the same BSP.

use std::{
    io::{self, Read},
    path::Path,
};

use serde::Deserialize;
use shagger_shared::{
    bsp::bsp_loader::load_collision,
    collision::shared_collision_world::SharedCollisionWorld,
    physics::{
        sweep_backend_adapter::SweepBackendAdapter, sweep_collision_backend::SweepCollisionBackend,
    },
};

const MAGIC: &[u8; 8] = b"SHAGCOL1";
/// `washoi_snd1`, the largest baked map, is 18 MB.
const MAX_FILE: u64 = 64 * 1024 * 1024;
const MAX_HEADER: usize = 4096;

/// What one bot moves through: its own sweep over the shared map.
pub(in crate::games::washoi) type BotCollision = SharedCollisionWorld<SweepBackendAdapter>;

#[derive(Deserialize)]
struct Header {
    map: String,
    bsp_sha256: String,
}

/// A map's parsed collision, loaded once per session.
pub struct MapCollision {
    map: String,
    bsp_sha256: String,
    backend: SweepCollisionBackend,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

impl MapCollision {
    pub fn load(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(MAX_FILE + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_FILE {
            return Err(invalid("collision file exceeds size limit"));
        }
        Self::parse(&bytes)
    }

    pub(in crate::games::washoi) fn parse(bytes: &[u8]) -> io::Result<Self> {
        let rest = bytes
            .strip_prefix(MAGIC)
            .ok_or_else(|| invalid("not a SHAGADELIC collision file"))?;
        let (length, rest) = rest
            .split_first_chunk::<4>()
            .ok_or_else(|| invalid("collision file is truncated"))?;
        let length = u32::from_le_bytes(*length) as usize;
        if length > MAX_HEADER || length > rest.len() {
            return Err(invalid("collision header is too long"));
        }
        let (header, bsp) = rest.split_at(length);
        let header: Header = serde_json::from_slice(header).map_err(io::Error::from)?;
        if header.map.trim().is_empty() {
            return Err(invalid("collision file names no map"));
        }
        if header.bsp_sha256.len() != 64
            || !header
                .bsp_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(invalid("collision file has no BSP SHA-256"));
        }
        let bsp =
            load_collision(bsp).map_err(|_| invalid("collision file holds no readable BSP"))?;
        Ok(Self {
            map: header.map,
            bsp_sha256: header.bsp_sha256.to_ascii_lowercase(),
            backend: SweepCollisionBackend::new(bsp),
        })
    }

    pub fn map(&self) -> &str {
        &self.map
    }

    /// The SHA-256 of the full BSP the file was trimmed from, lowercase hex.
    pub fn bsp_sha256(&self) -> &str {
        &self.bsp_sha256
    }

    /// A collision world of the bot's own. A trace writes the sweep's
    /// scratch state, and each bot moves the map's lifts and doors from its
    /// own snapshots, so bots don't share one.
    pub(in crate::games::washoi) fn world(&self) -> BotCollision {
        SharedCollisionWorld::new(SweepBackendAdapter {
            backend: self.backend.clone(),
        })
    }
}

#[cfg(test)]
pub(in crate::games::washoi) mod tests {
    use shagger_shared::{
        collision::collision_world::CollisionWorld, server::simulation::types::Vec3,
    };

    use super::*;

    /// A minimal IBSP: one solid brush, a box from -64 to 64 on every axis,
    /// in one world model, with the 17 lumps a Q3 map has.
    pub(in crate::games::washoi) fn tiny_bsp() -> Vec<u8> {
        let mut lumps: Vec<Vec<u8>> = vec![Vec::new(); 17];
        let f = |values: &[f32]| {
            values
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<u8>>()
        };
        let i = |values: &[i32]| {
            values
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<u8>>()
        };
        lumps[0] = b"{\n\"classname\" \"worldspawn\"\n}\n\0".to_vec();
        // A texture: a 64-byte name, surface flags, content flags (SOLID).
        let mut texture = b"textures/test/solid".to_vec();
        texture.resize(64, 0);
        texture.extend(i(&[0, 1]));
        lumps[1] = texture;
        // Six planes of a box, Q3 axes (z up): normal, distance.
        lumps[2] = f(&[
            1.0, 0.0, 0.0, 64.0, -1.0, 0.0, 0.0, 64.0, 0.0, 1.0, 0.0, 64.0, 0.0, -1.0, 0.0, 64.0,
            0.0, 0.0, 1.0, 64.0, 0.0, 0.0, -1.0, 64.0,
        ]);
        // One node splitting nothing, and one leaf holding the brush.
        lumps[3] = i(&[0, -1, -1, -1024, -1024, -1024, 1024, 1024, 1024]);
        lumps[4] = i(&[0, 0, -1024, -1024, -1024, 1024, 1024, 1024, 0, 0, 0, 1]);
        lumps[6] = i(&[0]);
        // The world model: bounds, faces, brushes.
        lumps[7] = [
            f(&[-1024.0, -1024.0, -1024.0, 1024.0, 1024.0, 1024.0]),
            i(&[0, 0, 0, 1]),
        ]
        .concat();
        // The brush: first side, side count, texture. Its six sides.
        lumps[8] = i(&[0, 6, 0]);
        lumps[9] = i(&[0, 0, 1, 0, 2, 0, 3, 0, 4, 0, 5, 0]);
        let head = 8 + 8 * 17;
        let mut directory = Vec::new();
        let mut body = Vec::new();
        for lump in &lumps {
            let offset = if lump.is_empty() {
                0
            } else {
                head + body.len()
            };
            directory.extend(i(&[offset as i32, lump.len() as i32]));
            body.extend(lump);
            while body.len() % 4 != 0 {
                body.push(0);
            }
        }
        [b"IBSP".to_vec(), i(&[46]), directory, body].concat()
    }

    pub(in crate::games::washoi) fn file(map: &str, sha: &str, bsp: &[u8]) -> Vec<u8> {
        let header = format!(r#"{{"map":"{map}","bsp_sha256":"{sha}"}}"#);
        [
            MAGIC.as_slice(),
            &(header.len() as u32).to_le_bytes(),
            header.as_bytes(),
            bsp,
        ]
        .concat()
    }

    pub(in crate::games::washoi) const SHA: &str =
        "0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn loads_a_trimmed_bsp_and_gives_each_bot_its_own_world() {
        let collision = MapCollision::parse(&file("box", SHA, &tiny_bsp())).unwrap();
        assert_eq!(collision.map(), "box");
        assert_eq!(collision.bsp_sha256(), SHA.to_ascii_lowercase());
        let (mut one, mut two) = (collision.world(), collision.world());
        // A point traced down onto the box's top, at y = 64 in game space.
        for world in [&mut one, &mut two] {
            let hit = world.trace(
                Vec3::new(0.0, 200.0, 0.0),
                Vec3::new(0.0, 0.0, 0.0),
                None,
                None,
            );
            assert!(hit.hit, "{hit:?}");
            assert!((hit.hit_point.unwrap().y - 64.0).abs() < 0.1, "{hit:?}");
            assert_eq!(world.point_contents(Vec3::new(0.0, 0.0, 0.0)) & 1, 1);
        }
    }

    /// `tests/fixtures/box.collision`, which the control tests load: the
    /// tiny map under `SHA`. Set `SHAGADELIC_WRITE_FIXTURES` to rewrite it.
    #[test]
    fn the_checked_in_box_is_the_tiny_map() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/box.collision");
        let bytes = file("box", SHA, &tiny_bsp());
        if std::env::var_os("SHAGADELIC_WRITE_FIXTURES").is_some() {
            std::fs::write(path, &bytes).unwrap();
        }
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn refuses_what_is_not_a_collision_file() {
        let bsp = tiny_bsp();
        let refused = |bytes: &[u8]| {
            MapCollision::parse(bytes)
                .err()
                .map(|error| error.to_string())
        };
        assert_eq!(
            refused(&bsp).as_deref(),
            Some("not a SHAGADELIC collision file")
        );
        assert_eq!(
            refused(&file(" ", SHA, &bsp)).as_deref(),
            Some("collision file names no map")
        );
        assert_eq!(
            refused(&file("box", "abc", &bsp)).as_deref(),
            Some("collision file has no BSP SHA-256")
        );
        assert_eq!(
            refused(&file("box", SHA, b"IBSP")).as_deref(),
            Some("collision file holds no readable BSP")
        );
        assert_eq!(
            refused(&MAGIC[..]).as_deref(),
            Some("collision file is truncated")
        );
    }
}
