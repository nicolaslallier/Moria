//! Small, deterministic terrain textures, baked on startup without external assets.
use bevy::{
    asset::RenderAssetUsages,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use moria_core::Tile;

/// Every tile drawn from `pixels`; the stairs use the user's art instead.
pub const BAKED: [Tile; 17] = [
    Tile::Rock,
    Tile::Gold,
    Tile::Mithril,
    Tile::Floor,
    Tile::Wall,
    Tile::Bed,
    Tile::Table,
    Tile::Chair,
    Tile::Grass,
    Tile::Tree,
    Tile::Water,
    Tile::Slope,
    Tile::Soil,
    Tile::Sky,
    Tile::WoodBed,
    Tile::WoodTable,
    Tile::WoodChair,
];

/// Compile-time exhaustive (no wildcard): a new `Tile` variant fails to build until it is
/// classified here, and a baked one must also be added to `BAKED`.
pub fn is_baked(t: Tile) -> bool {
    match t {
        Tile::Stair => false,
        Tile::Rock
        | Tile::Gold
        | Tile::Mithril
        | Tile::Floor
        | Tile::Wall
        | Tile::Bed
        | Tile::Table
        | Tile::Chair
        | Tile::Grass
        | Tile::Tree
        | Tile::Water
        | Tile::Slope
        | Tile::Soil
        | Tile::Sky
        | Tile::WoodBed
        | Tile::WoodTable
        | Tile::WoodChair => true,
    }
}

pub fn image(tile: Tile) -> Image {
    Image::new(
        Extent3d {
            width: 96,
            height: 96,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        pixels(tile),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}

fn pixels(tile: Tile) -> Vec<u8> {
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
            if matches!(tile, Tile::Gold | Tile::Mithril) {
                let vein = 44 + ((y as f32 * 0.11).sin() * 14.0) as i32;
                let branch = 22 + y / 2;
                let distance = (x - vein).abs().min((x - branch).abs());
                if distance < 4 && (x + y * 3) % 17 < 14 {
                    let ore = if tile == Tile::Gold {
                        [213, 158, 54]
                    } else {
                        [102, 194, 219]
                    };
                    let glint = if distance < 2 { 30 } else { -28 };
                    rgb = ore.map(|c| (c + noise + glint).clamp(0, 255) as u8);
                }
            }
            if let Some(c) = furniture(tile, x, y).or_else(|| outdoor(tile, x, y, noise)) {
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
        Tile::Water => {
            let wave = ((x as f32 * 0.2 + (y as f32 * 0.35).sin() * 3.0).sin() * 12.0) as i32;
            [38 + wave, 82 + wave, 150 + wave - noise]
        }
        // Earthy grass darkening toward the top, with contour ridges: reads as a rise.
        Tile::Slope => {
            let ridge = if y % 16 < 3 { -26 } else { 0 };
            let rise = y / 4;
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
        Tile::Chair | Tile::WoodChair => &[
            (28, 22, 68, 72, dark),
            (32, 34, 64, 68, top), // seat; the dark strip above is the back
        ],
        _ => &[],
    };
    boxes
        .iter()
        .rev()
        .find(|&&(x0, y0, x1, y1, _)| (x0..x1).contains(&x) && (y0..y1).contains(&y))
        .map(|b| b.4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baked_list_matches_the_exhaustive_mapping() {
        assert!(BAKED.iter().all(|&t| is_baked(t)));
        assert!(!is_baked(Tile::Stair));
    }

    #[test]
    fn terrain_is_opaque_and_ore_retains_the_rock_background() {
        let rock = pixels(Tile::Rock);
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
            let data = pixels(tile);
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
        assert_ne!(pixels(Tile::Gold), pixels(Tile::Mithril));
        assert_ne!(pixels(Tile::Floor), rock);
        assert_ne!(pixels(Tile::Bed), pixels(Tile::Floor));
        assert_ne!(pixels(Tile::Table), pixels(Tile::Chair));
    }

    #[test]
    fn outdoor_and_wood_tiles_have_their_own_look() {
        let (grass, floor) = (pixels(Tile::Grass), pixels(Tile::Floor));
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
        ];
        for t in new {
            let data = pixels(t);
            assert_eq!(data.len(), 96 * 96 * 4);
            assert!(data.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
            assert_ne!(data, grass, "{t:?} looks like grass");
            assert_ne!(data, floor, "{t:?} looks like floor");
            for u in new.into_iter().filter(|&u| u != t) {
                assert_ne!(data, pixels(u), "{t:?} looks like {u:?}");
            }
        }
        // Wood furniture is browner (more red than blue) than its stone twin.
        let warmth = |t| {
            pixels(t)
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
}
