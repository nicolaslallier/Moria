//! Small, deterministic terrain textures, baked on startup without external assets.
use bevy::{
    asset::RenderAssetUsages,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use moria_core::Tile;

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
            if let Some(c) = furniture(tile, x, y) {
                rgb = c.map(|c| (c + noise).clamp(0, 255) as u8);
            }
            data.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    data
}

/// Top-down carved-stone furniture over the floor: `(x0, y0, x1, y1, rgb)` boxes, last wins.
fn furniture(tile: Tile, x: i32, y: i32) -> Option<[i32; 3]> {
    const STONE: [i32; 3] = [158, 150, 136];
    const DARK: [i32; 3] = [104, 98, 88];
    let boxes: &[(i32, i32, i32, i32, [i32; 3])] = match tile {
        Tile::Bed => &[
            (18, 8, 78, 88, DARK),
            (22, 12, 74, 84, STONE),
            (26, 16, 70, 30, [226, 220, 204]), // pillow
            (24, 36, 72, 82, [150, 48, 44]),   // blanket
        ],
        Tile::Table => &[
            (12, 20, 84, 70, DARK),
            (16, 24, 80, 66, STONE),
            (40, 36, 56, 52, [190, 150, 70]), // tankard
        ],
        Tile::Chair => &[
            (28, 22, 68, 72, DARK),
            (32, 34, 64, 68, STONE), // seat; the dark strip above is the back
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
}
