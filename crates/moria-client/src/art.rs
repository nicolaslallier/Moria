//! Images baked into the binary: the release ships a lone executable, no assets folder.
//! Heads, portraits and stairs use the user's art; every other tile is generated in `terrain`.
//! The bytes go into Bevy's `embedded://` source, so decoding runs on the asset
//! task pool instead of blocking startup.

use bevy::asset::RenderAssetUsages;
use bevy::asset::io::embedded::EmbeddedAssetRegistry;
use bevy::image::ImageLoaderSettings;
use bevy::prelude::*;
use moria_core::Tile;
use moria_core::srd::Race;
use std::path::{Path, PathBuf};

macro_rules! files {
    ($($name:ident),* $(,)?) => {
        &[$((stringify!($name), include_bytes!(concat!("../assets/", stringify!($name), ".png")))),*]
    };
}

const FILES: &[(&str, &[u8])] = files!(
    rock,
    floor,
    stair,
    castle,
    head_worker,
    head_dwarf,
    head_elf,
    head_halfling,
    head_human,
    head_dragonborn,
    head_gnome,
    head_halfelf,
    head_halforc,
    head_tiefling,
    portrait_dwarf,
    portrait_elf,
    portrait_halfling,
    portrait_human,
    portrait_dragonborn,
    portrait_gnome,
    portrait_halfelf,
    portrait_halforc,
    portrait_tiefling,
);

/// File-name part per race; the exhaustive match makes a new race a compile error.
fn slug(r: Race) -> &'static str {
    match r {
        Race::Dwarf => "dwarf",
        Race::Elf => "elf",
        Race::Halfling => "halfling",
        Race::Human => "human",
        Race::Dragonborn => "dragonborn",
        Race::Gnome => "gnome",
        Race::HalfElf => "halfelf",
        Race::HalfOrc => "halforc",
        Race::Tiefling => "tiefling",
    }
}

#[derive(Resource)]
pub struct Art {
    /// Baked textures, one per `terrain::BAKED` tile.
    terrain: Vec<(Tile, Handle<Image>)>,
    pub stair: Handle<Image>,
    pub worker_head: Handle<Image>,
    pub castle: Handle<Image>,
    /// Indexed like `Race::ALL`, built from it.
    heads: Vec<Handle<Image>>,
    portraits: Vec<Handle<Image>>,
}

fn race_index(r: Race) -> usize {
    Race::ALL.iter().position(|&x| x == r).unwrap_or(0)
}

impl Art {
    pub fn head(&self, r: Race) -> Handle<Image> {
        self.heads[race_index(r)].clone()
    }

    /// The tile's texture; the stairs (not baked) fall back to their art.
    pub fn tile(&self, t: Tile) -> Handle<Image> {
        self.terrain
            .iter()
            .find(|e| e.0 == t)
            .map_or_else(|| self.stair.clone(), |e| e.1.clone())
    }

    pub fn portrait(&self, r: Race) -> Handle<Image> {
        self.portraits[race_index(r)].clone()
    }
}

impl FromWorld for Art {
    fn from_world(world: &mut World) -> Self {
        let registry = world.resource::<EmbeddedAssetRegistry>();
        for &(name, bytes) in FILES {
            let path = format!("moria/{name}.png");
            registry.insert_asset(PathBuf::from(&path), Path::new(&path), bytes);
        }
        let server = world.resource::<AssetServer>();
        // Nothing reads pixels on the CPU, so the decoded copy is dropped after upload.
        let load = |name: &str| -> Handle<Image> {
            server
                .load_builder()
                .with_settings(|s: &mut ImageLoaderSettings| {
                    s.asset_usage = RenderAssetUsages::RENDER_WORLD;
                })
                .load(format!("embedded://moria/{name}.png"))
        };
        let stair = load("stair");
        let worker_head = load("head_worker");
        let castle = load("castle");
        let heads = Race::ALL.map(|r| load(&format!("head_{}", slug(r)))).into();
        let portraits = Race::ALL
            .map(|r| load(&format!("portrait_{}", slug(r))))
            .into();
        let mut images = world.resource_mut::<Assets<Image>>();
        Art {
            terrain: crate::terrain::BAKED
                .map(|t| (t, images.add(crate::terrain::image(t))))
                .into(),
            stair,
            worker_head,
            castle,
            heads,
            portraits,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::image::{CompressedImageFormats, ImageSampler, ImageType};

    #[test]
    fn every_embedded_image_decodes() {
        for &(name, bytes) in FILES {
            Image::from_buffer(
                bytes,
                ImageType::Extension("png"),
                CompressedImageFormats::NONE,
                true,
                ImageSampler::Default,
                RenderAssetUsages::default(),
            )
            .unwrap_or_else(|e| panic!("{name}.png: {e}"));
        }
    }

    #[test]
    fn every_race_has_its_own_head_and_portrait() {
        for r in Race::ALL {
            for kind in ["head", "portrait"] {
                let want = format!("{kind}_{}", slug(r));
                assert!(FILES.iter().any(|&(n, _)| n == want), "missing {want}.png");
            }
        }
    }
}
