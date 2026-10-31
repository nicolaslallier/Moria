//! Images baked into the binary: the release ships a lone executable, no assets folder.
//! Sources are the user's art, shrunk once (tiles 96px, heads 96px, portraits 256px).

use bevy::asset::RenderAssetUsages;
use bevy::image::{CompressedImageFormats, ImageSampler, ImageType};
use bevy::prelude::*;
use moria_core::srd::Race;

macro_rules! png {
    ($images:expr, $f:expr) => {
        $images.add(
            Image::from_buffer(
                include_bytes!(concat!("../assets/", $f, ".png")),
                ImageType::Extension("png"),
                CompressedImageFormats::NONE,
                true,
                ImageSampler::Default,
                RenderAssetUsages::default(),
            )
            .expect(concat!("embedded ", $f, ".png")),
        )
    };
}

/// One file per race, in `Race::ALL` order.
macro_rules! per_race {
    ($images:expr, $prefix:literal) => {
        [
            png!($images, concat!($prefix, "dwarf")),
            png!($images, concat!($prefix, "elf")),
            png!($images, concat!($prefix, "halfling")),
            png!($images, concat!($prefix, "human")),
            png!($images, concat!($prefix, "dragonborn")),
            png!($images, concat!($prefix, "gnome")),
            png!($images, concat!($prefix, "halfelf")),
            png!($images, concat!($prefix, "halforc")),
            png!($images, concat!($prefix, "tiefling")),
        ]
    };
}

#[derive(Resource)]
pub struct Art {
    pub rock: Handle<Image>,
    pub floor: Handle<Image>,
    pub stair: Handle<Image>,
    pub worker_head: Handle<Image>,
    pub castle: Handle<Image>,
    heads: [Handle<Image>; 9],
    portraits: [Handle<Image>; 9],
}

fn race_index(r: Race) -> usize {
    Race::ALL.iter().position(|&x| x == r).unwrap_or(0)
}

impl Art {
    pub fn head(&self, r: Race) -> Handle<Image> {
        self.heads[race_index(r)].clone()
    }

    pub fn portrait(&self, r: Race) -> Handle<Image> {
        self.portraits[race_index(r)].clone()
    }
}

impl FromWorld for Art {
    fn from_world(world: &mut World) -> Self {
        let mut images = world.resource_mut::<Assets<Image>>();
        Art {
            rock: png!(images, "rock"),
            floor: png!(images, "floor"),
            stair: png!(images, "stair"),
            worker_head: png!(images, "head_worker"),
            castle: png!(images, "castle"),
            heads: per_race!(images, "head_"),
            portraits: per_race!(images, "portrait_"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_image_decodes() {
        let mut w = World::new();
        w.init_resource::<Assets<Image>>();
        let art = Art::from_world(&mut w);
        assert_ne!(art.head(Race::Dwarf), art.head(Race::Tiefling));
        assert_eq!(w.resource::<Assets<Image>>().len(), 23);
    }
}
