//! Small, deterministic terrain textures, baked on startup without external assets.
use bevy::{
    asset::RenderAssetUsages,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use moria_core::Tile;

/// The generated texture of every tile but the stairs (user art).
pub fn bake(t: Tile) -> Option<Image> {
    (t != Tile::Stair).then(|| image(t, false))
}

/// Furniture standing outdoors, drawn on grass instead of the stone floor.
pub fn bake_on_grass(t: Tile) -> Option<Image> {
    t.is_furniture().then(|| image(t, true))
}

fn image(tile: Tile, on_grass: bool) -> Image {
    Image::new(
        Extent3d {
            width: 96,
            height: 96,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        pixels(tile, on_grass),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}

fn pixels(tile: Tile, on_grass: bool) -> Vec<u8> {
    let mut data = Vec::with_capacity(96 * 96 * 4);
    for y in 0_i32..96 {
        for x in 0_i32..96 {
            let noise = ((x * 73 + y * 179 + x * y * 13) % 11) - 5;
            let floor = matches!(tile, Tile::Floor | Tile::Stair) || tile.is_furniture();
            let wall = tile == Tile::Wall;
            let (bx, by) = if floor {
                ((x + (y / 48) * 24) % 48, y % 48)
            } else if wall {
                ((x + (y / 32) * 24) % 48, y % 32)
            } else {
                // Broad fractured stone instead of miniature repeated bricks.
                ((x + y / 3 + (y / 48) * 19) % 48, (y + x / 5) % 48)
            };
            let edge = bx.min(by);
            let shade = noise
                + if edge < 2 {
                    -24
                } else if edge < 4 {
                    13
                } else {
                    0
                };
            let base = if floor {
                [100, 94, 82]
            } else if wall {
                [88, 94, 105]
            } else {
                [65, 73, 83]
            };
            let mut rgb = base.map(|c| (c + shade).clamp(0, 255) as u8);
            if matches!(
                tile,
                Tile::Gold | Tile::Mithril | Tile::IronOre | Tile::Coal
            ) {
                let vein = 44 + ((y as f32 * 0.11).sin() * 14.0) as i32;
                let branch = 22 + y / 2;
                let distance = (x - vein).abs().min((x - branch).abs());
                if distance < 4 && (x + y * 3) % 17 < 14 {
                    let ore = match tile {
                        Tile::Gold => [213, 158, 54],
                        Tile::IronOre => [168, 112, 90],
                        Tile::Coal => [28, 28, 32],
                        _ => [102, 194, 219],
                    };
                    let glint = if distance < 2 { 30 } else { -28 };
                    rgb = ore.map(|c| (c + noise + glint).clamp(0, 255) as u8);
                }
            }
            let ground = on_grass.then(|| grass(x, y));
            if let Some(c) = furniture(tile, x, y)
                .or(ground)
                .or_else(|| outdoor(tile, x, y, noise))
            {
                rgb = c.map(|c| (c + noise).clamp(0, 255) as u8);
            }
            data.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    data
}

/// Grass with darker tufts, also the ground under a tree.
fn grass(x: i32, y: i32) -> [i32; 3] {
    let tuft = if (x * 7 + y * 13 + (x / 6) * (y / 5)) % 9 == 0 {
        -22
    } else {
        0
    };
    [72 + tuft, 122 + tuft, 58 + tuft]
}

/// Outdoor tiles, seen from above. `noise` is added by the caller.
fn outdoor(tile: Tile, x: i32, y: i32, noise: i32) -> Option<[i32; 3]> {
    let (dx, dy) = (x - 48, y - 48);
    let r2 = dx * dx + dy * dy;
    Some(match tile {
        Tile::Grass => grass(x, y),
        // A round canopy with a lit top-left and a shadow ring, over grass.
        Tile::Tree if r2 < 40 * 40 => {
            let lit = -(dx + dy) / 4 + if (x / 7 + y / 9) % 3 == 0 { 10 } else { 0 };
            [38 + lit, 92 + lit, 42 + lit]
        }
        Tile::Tree if r2 < 44 * 44 => [36, 70, 34],
        Tile::Tree => grass(x, y),
        // Seen from above: a black-and-white body, its pink muzzle toward the east, on grass.
        Tile::Cow => {
            let body = (x - 44).pow(2) * 9 + (y - 48).pow(2) * 25 < 26 * 26 * 9;
            let head = (x - 76).pow(2) + (y - 48).pow(2) < 10 * 10;
            if head {
                [214, 150, 150]
            } else if body && (x / 11 + y / 9) % 3 == 0 {
                [34, 30, 30]
            } else if body {
                [232, 228, 220]
            } else {
                grass(x, y)
            }
        }
        Tile::Water => {
            let wave = ((x as f32 * 0.2 + (y as f32 * 0.35).sin() * 3.0).sin() * 12.0) as i32;
            [38 + wave, 82 + wave, 150 + wave - noise]
        }
        // Earthy grass brightening toward the east (+x), with contour ridges: reads as a rise.
        Tile::Slope => {
            let ridge = if x % 16 < 3 { -26 } else { 0 };
            let rise = x / 4;
            [90 + rise + ridge, 112 + rise + ridge, 56 + rise / 2 + ridge]
        }
        Tile::Soil => {
            let pebble = if (x * 11 + y * 17 + x * y) % 23 == 0 {
                40
            } else {
                0
            };
            [112 + pebble, 80 + pebble, 50 + pebble]
        }
        // Dark, nearly flat: an empty level, not a surface.
        Tile::Sky => [16 - noise, 22 - noise, 46 - noise],
        _ => return None,
    })
}

/// Top-down carved-stone (or wooden) furniture over the floor: `(x0, y0, x1, y1, rgb)`
/// boxes, last wins.
fn furniture(tile: Tile, x: i32, y: i32) -> Option<[i32; 3]> {
    let wood = matches!(tile, Tile::WoodBed | Tile::WoodTable | Tile::WoodChair);
    let (top, dark) = if wood {
        // Plank grain along x.
        let grain = if y % 8 == 0 { -18 } else { 0 };
        ([156 + grain, 104 + grain, 58 + grain], [96, 60, 32])
    } else {
        ([158, 150, 136], [104, 98, 88])
    };
    let boxes: &[(i32, i32, i32, i32, [i32; 3])] = match tile {
        Tile::Bed | Tile::WoodBed => &[
            (18, 8, 78, 88, dark),
            (22, 12, 74, 84, top),
            (26, 16, 70, 30, [226, 220, 204]), // pillow
            (24, 36, 72, 82, [150, 48, 44]),   // blanket
        ],
        Tile::Table | Tile::WoodTable => &[
            (12, 20, 84, 70, dark),
            (16, 24, 80, 66, top),
            (40, 36, 56, 52, [190, 150, 70]), // tankard
        ],
        Tile::Forge => &[
            (10, 14, 86, 82, [104, 98, 88]), // stone body
            (16, 20, 80, 76, [128, 120, 106]),
            (28, 34, 68, 68, [28, 24, 22]), // mouth
            (32, 44, 64, 66, [214, 82, 24]),
            (40, 50, 56, 64, [255, 170, 48]),
        ],
        Tile::Anvil => &[
            (30, 52, 66, 84, [96, 60, 32]), // stump
            (14, 22, 82, 54, [40, 40, 46]), // iron block
            (20, 26, 76, 40, [72, 72, 82]), // face
            (62, 30, 90, 40, [40, 40, 46]), // horn
        ],
        Tile::WoodDoor | Tile::WoodDoorLocked => &[
            (12, 8, 84, 88, [96, 60, 32]),
            (16, 12, 44, 84, [156, 104, 58]), // planks
            (48, 12, 80, 84, [146, 96, 52]),
            (68, 44, 76, 52, [210, 190, 90]), // handle
        ],
        Tile::StoneDoor | Tile::StoneDoorLocked => &[
            (12, 8, 84, 88, [88, 84, 78]),
            (16, 12, 80, 84, [146, 142, 134]), // slab
            (22, 20, 74, 76, [132, 128, 120]),
            (68, 44, 76, 52, [60, 58, 54]), // ring
        ],
        Tile::IronDoor | Tile::IronDoorLocked => &[
            (12, 8, 84, 88, [30, 30, 36]),
            (16, 12, 80, 84, [76, 78, 90]),    // plate
            (22, 18, 28, 24, [150, 152, 164]), // rivets
            (68, 18, 74, 24, [150, 152, 164]),
            (22, 72, 28, 78, [150, 152, 164]),
            (68, 72, 74, 78, [150, 152, 164]),
            (68, 44, 76, 52, [150, 152, 164]),
        ],
        Tile::Chair | Tile::WoodChair => &[
            (28, 22, 68, 72, dark),
            (32, 34, 64, 68, top), // seat; the dark strip above is the back
        ],
        Tile::Candle => &[
            (42, 52, 54, 64, [210, 204, 190]), // holder
            (45, 36, 51, 56, [236, 228, 200]), // wax
            (46, 26, 50, 36, [255, 196, 64]),  // flame
        ],
        Tile::Torch => &[
            (44, 30, 52, 84, [96, 60, 32]),  // pole
            (38, 14, 58, 36, [214, 92, 28]), // fire
            (44, 18, 52, 30, [255, 204, 72]),
        ],
        Tile::Lamp => &[
            (34, 28, 62, 68, [58, 54, 50]),    // iron frame
            (38, 34, 58, 62, [255, 212, 96]),  // glass
            (44, 42, 52, 54, [255, 246, 190]), // wick
        ],
        Tile::Campfire => &[
            (16, 62, 80, 74, [96, 60, 32]), // logs
            (24, 70, 72, 80, [76, 46, 24]),
            (28, 30, 68, 64, [214, 82, 24]), // flames
            (38, 20, 58, 56, [255, 170, 48]),
            (44, 34, 52, 52, [255, 236, 130]),
        ],
        Tile::Hearth => &[
            (8, 14, 88, 82, [104, 98, 88]), // stone surround
            (20, 26, 76, 82, [28, 24, 22]), // firebox
            (30, 48, 66, 78, [214, 82, 24]),
            (38, 40, 58, 74, [255, 170, 48]),
        ],
        Tile::Stove => &[
            (22, 18, 74, 78, [34, 34, 38]), // iron body
            (28, 24, 68, 72, [58, 58, 64]),
            (36, 40, 60, 62, [28, 24, 22]),   // door
            (40, 48, 56, 58, [255, 150, 48]), // glow
            (44, 8, 52, 20, [120, 120, 126]), // chimney
        ],
        _ => &[],
    };
    let bar: &[(i32, i32, i32, i32, [i32; 3])] = &[(8, 40, 88, 54, [24, 22, 22])];
    let bar = if matches!(
        tile,
        Tile::WoodDoorLocked | Tile::StoneDoorLocked | Tile::IronDoorLocked
    ) {
        bar
    } else {
        &[]
    };
    bar.iter()
        .chain(boxes.iter().rev())
        .find(|&&(x0, y0, x1, y1, _)| (x0..x1).contains(&x) && (y0..y1).contains(&y))
        .map(|b| b.4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_stairs_are_not_baked() {
        for t in Tile::ALL {
            assert_eq!(bake(t).is_none(), t == Tile::Stair, "{t:?}");
        }
    }

    #[test]
    fn terrain_is_opaque_and_ore_retains_the_rock_background() {
        let rock = pixels(Tile::Rock, false);
        assert_eq!(rock.len(), 96 * 96 * 4);
        for tile in [
            Tile::Rock,
            Tile::Floor,
            Tile::Wall,
            Tile::Gold,
            Tile::Mithril,
            Tile::Bed,
            Tile::Table,
            Tile::Chair,
        ] {
            let data = pixels(tile, false);
            assert_eq!(data.len(), rock.len());
            assert!(data.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
            if matches!(tile, Tile::Gold | Tile::Mithril) {
                let same = data
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(rock.as_chunks::<4>().0.iter())
                    .filter(|(a, b)| a == b)
                    .count();
                assert!(same > 96 * 96 / 2, "ore should be a vein in rock");
                assert!(same < 96 * 96 * 9 / 10, "vein should be visible");
            }
        }
        assert_ne!(pixels(Tile::Gold, false), pixels(Tile::Mithril, false));
        assert_ne!(pixels(Tile::Floor, false), rock);
        assert_ne!(pixels(Tile::Bed, false), pixels(Tile::Floor, false));
        assert_ne!(pixels(Tile::Table, false), pixels(Tile::Chair, false));
    }

    #[test]
    fn outdoor_and_wood_tiles_have_their_own_look() {
        let (grass, floor) = (pixels(Tile::Grass, false), pixels(Tile::Floor, false));
        assert_ne!(grass, floor);
        let new = [
            Tile::Tree,
            Tile::Water,
            Tile::Slope,
            Tile::Soil,
            Tile::Sky,
            Tile::WoodBed,
            Tile::WoodTable,
            Tile::WoodChair,
            Tile::IronOre,
            Tile::Coal,
            Tile::Forge,
            Tile::Anvil,
            Tile::WoodDoor,
            Tile::StoneDoor,
            Tile::IronDoor,
            Tile::WoodDoorLocked,
            Tile::StoneDoorLocked,
            Tile::IronDoorLocked,
            Tile::Cow,
        ];
        for t in new {
            let data = pixels(t, false);
            assert_eq!(data.len(), 96 * 96 * 4);
            assert!(data.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
            assert_ne!(data, grass, "{t:?} looks like grass");
            assert_ne!(data, floor, "{t:?} looks like floor");
            for u in new.into_iter().filter(|&u| u != t) {
                assert_ne!(data, pixels(u, false), "{t:?} looks like {u:?}");
            }
        }
        // Wood furniture is browner (more red than blue) than its stone twin.
        let warmth = |t| {
            pixels(t, false)
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| i64::from(p[0]) - i64::from(p[2]))
                .sum::<i64>()
        };
        for (wood, stone) in [
            (Tile::WoodBed, Tile::Bed),
            (Tile::WoodTable, Tile::Table),
            (Tile::WoodChair, Tile::Chair),
        ] {
            assert!(warmth(wood) > warmth(stone) + 96 * 96 * 10, "{wood:?}");
        }
    }

    #[test]
    fn a_locked_door_has_a_bar_across() {
        for (open, locked) in [
            (Tile::WoodDoor, Tile::WoodDoorLocked),
            (Tile::StoneDoor, Tile::StoneDoorLocked),
            (Tile::IronDoor, Tile::IronDoorLocked),
        ] {
            assert_ne!(pixels(open, false), pixels(locked, false), "{open:?}");
        }
    }

    #[test]
    fn each_light_has_its_own_look() {
        let lights = [
            Tile::Candle,
            Tile::Torch,
            Tile::Lamp,
            Tile::Campfire,
            Tile::Hearth,
            Tile::Stove,
        ];
        for (i, a) in lights.iter().enumerate() {
            for b in &lights[i + 1..] {
                assert_ne!(pixels(*a, false), pixels(*b, false), "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn furniture_outdoors_sits_on_grass() {
        let grass = pixels(Tile::Grass, false);
        let center = (48 * 96 + 48) * 4..(48 * 96 + 49) * 4;
        for t in Tile::ALL.into_iter().filter(|t| t.is_furniture()) {
            let (out, inside) = (pixels(t, true), pixels(t, false));
            assert_eq!(out[..4], grass[..4], "{t:?}: corner is grass");
            assert_ne!(inside[..4], grass[..4], "{t:?}: indoor corner is floor");
            assert_eq!(
                out[center.clone()],
                inside[center.clone()],
                "{t:?}: same piece"
            );
        }
    }

    #[test]
    fn cow_stands_on_grass() {
        let (cow, grass) = (pixels(Tile::Cow, false), pixels(Tile::Grass, false));
        let center = (48 * 96 + 48) * 4..(48 * 96 + 49) * 4;
        assert_eq!(cow[..4], grass[..4], "corner is grass");
        assert_ne!(cow[center.clone()], grass[center], "a cow in the middle");
    }

    #[test]
    fn slope_rises_toward_the_east() {
        // Generated slopes climb toward +x, so the texture must brighten left to right.
        let data = pixels(Tile::Slope, false);
        let sum = |cols: std::ops::Range<usize>| -> u32 {
            (0..96)
                .flat_map(|y| cols.clone().map(move |x| (y, x)))
                .map(|(y, x)| u32::from(data[(y * 96 + x) * 4]))
                .sum()
        };
        assert!(sum(0..8) < sum(88..96), "{} vs {}", sum(0..8), sum(88..96));
    }
}
