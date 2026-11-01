//! Day/night cycle and lighting.

use crate::geo::TilePos;
use crate::mountain::Mountain;
use crate::tile::Tile;

/// One real hour = one game day, at 100 ms per tick.
pub const DAY_TICKS: u64 = 36_000;
const HOUR_TICKS: u64 = 1_500;
const DUSK_START: u64 = 10 * HOUR_TICKS;
const NIGHT_START: u64 = 12 * HOUR_TICKS;
const DAWN_START: u64 = 22 * HOUR_TICKS;

/// Part of the day, as `daylight` sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DayPhase {
    Day,
    Dusk,
    Night,
    Dawn,
}

pub fn phase(tick: u64) -> DayPhase {
    match tick % DAY_TICKS {
        t if t < DUSK_START => DayPhase::Day,
        t if t < NIGHT_START => DayPhase::Dusk,
        t if t < DAWN_START => DayPhase::Night,
        _ => DayPhase::Dawn,
    }
}

/// Daylight level: 1.0 by day, 0.0 at night, linear through dusk and dawn.
pub fn daylight(tick: u64) -> f32 {
    let t = tick % DAY_TICKS;
    let ramp = |from: u64| (t - from) as f32 / (2 * HOUR_TICKS) as f32;
    match phase(tick) {
        DayPhase::Day => 1.0,
        DayPhase::Dusk => 1.0 - ramp(DUSK_START),
        DayPhase::Night => 0.0,
        DayPhase::Dawn => ramp(DAWN_START),
    }
}

/// Game hour 0..=23; tick 0 is 6 h.
pub fn hour(tick: u64) -> u8 {
    ((6 + tick % DAY_TICKS / HOUR_TICKS) % 24) as u8
}

/// Light radius of the hero and of each dwarf.
pub const BODY_LIGHT: u8 = 4;
/// Minimum outdoor brightness at night.
pub const MOONLIGHT: f32 = 0.12;

/// Tiles that stop light (they still receive it).
pub fn blocks_light(t: Tile) -> bool {
    t.is_stone()
}

/// Largest radius of a built light source.
pub const MAX_LIGHT_RADIUS: u8 = 8;

/// Radius of a built light source, at most `MAX_LIGHT_RADIUS`.
pub fn light_radius(t: Tile) -> Option<u8> {
    match t {
        Tile::Candle => Some(3),
        Tile::Torch => Some(5),
        Tile::Lamp | Tile::Stove => Some(6),
        Tile::Campfire => Some(7),
        Tile::Hearth => Some(8),
        _ => None,
    }
}

/// Inclusive rectangle of one level.
#[derive(Clone, Copy, Debug)]
pub struct Area {
    pub x0: i16,
    pub y0: i16,
    pub x1: i16,
    pub y1: i16,
}

/// Brightness 0.0..=1.0 over an `Area`.
pub struct LightMap {
    area: Area,
    w: usize,
    v: Vec<f32>,
}

impl LightMap {
    fn idx(&self, x: i16, y: i16) -> Option<usize> {
        let a = self.area;
        (x >= a.x0 && x <= a.x1 && y >= a.y0 && y <= a.y1)
            .then(|| (y - a.y0) as usize * self.w + (x - a.x0) as usize)
    }

    /// Brightness at `(x, y)`; 0.0 outside the area.
    pub fn get(&self, x: i16, y: i16) -> f32 {
        self.idx(x, y).map_or(0.0, |i| self.v[i])
    }

    fn raise(&mut self, x: i16, y: i16, b: f32) {
        if let Some(i) = self.idx(x, y) {
            self.v[i] = self.v[i].max(b);
        }
    }
}

/// True if the Bresenham line a -> b crosses a blocker strictly between its ends.
fn blocked(m: &Mountain, z: u8, (x0, y0): (i16, i16), (x1, y1): (i16, i16)) -> bool {
    let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
    let (sx, sy) = ((x1 - x0).signum(), (y1 - y0).signum());
    let (mut x, mut y, mut err) = (x0, y0, dx + dy);
    while (x, y) != (x1, y1) {
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
        if (x, y) != (x1, y1) && m.get(TilePos::new(x, y, z)).is_some_and(blocks_light) {
            return true;
        }
    }
    false
}

/// Brightness of every tile of `area` on level `z`, from `sources` `(x, y, radius)`,
/// built lights near the area, and daylight/moonlight outdoors.
pub fn light_map(
    m: &Mountain,
    z: u8,
    area: Area,
    sources: &[(i16, i16, u8)],
    tick: u64,
) -> LightMap {
    let w = (area.x1 - area.x0 + 1).max(0) as usize;
    let h = (area.y1 - area.y0 + 1).max(0) as usize;
    let mut map = LightMap {
        area,
        w,
        v: vec![0.0; w * h],
    };
    let (mw, mh, _) = m.size();
    let tile = |x: i16, y: i16, z: u8| m.get(TilePos::new(x, y, z));

    // Outdoors: the sun or the moon.
    let sky = daylight(tick).max(MOONLIGHT);
    let outdoor = |x: i16, y: i16| {
        tile(x, y, z).is_some()
            && (z == 0 || tile(x, y, z) == Some(Tile::Sky) || tile(x, y, z - 1) == Some(Tile::Sky))
    };
    for y in area.y0.max(0)..=area.y1.min(mh - 1) {
        for x in area.x0.max(0)..=area.x1.min(mw - 1) {
            let wall = tile(x, y, z).is_some_and(blocks_light)
                && [(1, 0), (-1, 0), (0, 1), (0, -1)]
                    .iter()
                    .any(|(dx, dy)| outdoor(x + dx, y + dy));
            if outdoor(x, y) || wall {
                map.raise(x, y, sky);
            }
        }
    }

    // Built lights that can reach the area.
    let mut all = sources.to_vec();
    let r = MAX_LIGHT_RADIUS as i16;
    for y in (area.y0 - r).max(0)..=(area.y1 + r).min(mh - 1) {
        for x in (area.x0 - r).max(0)..=(area.x1 + r).min(mw - 1) {
            if let Some(r) = tile(x, y, z).and_then(light_radius) {
                all.push((x, y, r));
            }
        }
    }

    // ponytail: one Bresenham ray per tile per source; switch to recursive shadowcasting if a profile shows it.
    for &(sx, sy, r) in &all {
        let r = r as i16;
        for y in (sy - r).max(area.y0)..=(sy + r).min(area.y1) {
            for x in (sx - r).max(area.x0)..=(sx + r).min(area.x1) {
                let d = f32::hypot((x - sx) as f32, (y - sy) as f32);
                if d <= r as f32 && tile(x, y, z).is_some() && !blocked(m, z, (sx, sy), (x, y)) {
                    map.raise(x, y, if d <= (r - 1) as f32 { 1.0 } else { 0.5 });
                }
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::MountainId;

    fn mine() -> Mountain {
        let mut m = Mountain::new(MountainId(0), 20, 20, 3, Tile::Rock);
        for x in 2..=15 {
            m.set(TilePos::new(x, 5, 1), Tile::Floor);
        }
        m
    }

    fn hillside() -> Mountain {
        let mut m = Mountain::new(MountainId(0), 20, 20, 3, Tile::Rock);
        for y in 0..20 {
            for x in 0..10 {
                m.set(TilePos::new(x, y, 0), Tile::Sky);
                m.set(TilePos::new(x, y, 1), Tile::Grass);
            }
        }
        m
    }

    fn area() -> Area {
        Area {
            x0: 0,
            y0: 0,
            x1: 19,
            y1: 19,
        }
    }

    #[test]
    fn built_lights_shine_in_the_mine() {
        let mut m = mine();
        m.set(TilePos::new(5, 5, 1), Tile::Torch);
        assert_eq!(light_radius(Tile::Torch), Some(5));
        let l = light_map(&m, 1, area(), &[], 20_000);
        assert_eq!(l.get(8, 5), 1.0);
        assert!(!blocks_light(Tile::Torch));
    }

    #[test]
    fn underground_is_dark_without_light() {
        let l = light_map(&mine(), 1, area(), &[], 0);
        assert_eq!(l.get(5, 5), 0.0);
    }

    #[test]
    fn corridor_lights_its_walls_not_beyond() {
        let l = light_map(&mine(), 1, area(), &[(5, 5, BODY_LIGHT)], 0);
        assert_eq!(l.get(5, 5), 1.0);
        assert_eq!(l.get(7, 5), 1.0);
        assert_eq!(l.get(5, 4), 1.0);
        assert_eq!(l.get(5, 3), 0.0);
        assert_eq!(l.get(9, 5), 0.5);
        assert_eq!(l.get(10, 5), 0.0);
    }

    #[test]
    fn outdoors_follows_the_sun_and_cliffs_stay_visible() {
        let m = hillside();
        let day = light_map(&m, 1, area(), &[], 0);
        assert_eq!(day.get(3, 3), 1.0);
        assert_eq!(day.get(10, 3), 1.0);
        assert_eq!(day.get(12, 3), 0.0);
        let night = light_map(&m, 1, area(), &[], 20_000);
        assert_eq!(night.get(3, 3), MOONLIGHT);
    }

    #[test]
    fn light_at_map_corner_does_not_panic() {
        let a = Area {
            x0: -5,
            y0: -5,
            x1: 25,
            y1: 25,
        };
        let l = light_map(&mine(), 1, a, &[(0, 0, 8)], 0);
        assert_eq!(l.get(-3, -3), 0.0);
    }

    #[test]
    fn day_night_cycle() {
        assert_eq!(daylight(0), 1.0);
        assert_eq!(daylight(14_999), 1.0);
        assert!((daylight(16_500) - 0.5).abs() < 1e-3); // mid-dusk
        assert_eq!(daylight(18_000), 0.0);
        assert_eq!(daylight(32_999), 0.0);
        assert!((daylight(34_500) - 0.5).abs() < 1e-3); // mid-dawn
        assert_eq!(daylight(DAY_TICKS), 1.0); // next morning
        let _ = daylight(u64::MAX); // no panic
    }

    #[test]
    fn phases_change_at_the_daylight_boundaries() {
        assert_eq!(phase(14_999), DayPhase::Day);
        assert_eq!(phase(15_000), DayPhase::Dusk);
        assert_eq!(phase(18_000), DayPhase::Night);
        assert_eq!(phase(33_000), DayPhase::Dawn);
        assert_eq!(phase(DAY_TICKS), DayPhase::Day);
    }

    #[test]
    fn no_light_exceeds_the_max_radius() {
        for t in Tile::ALL {
            assert!(light_radius(t).unwrap_or(0) <= MAX_LIGHT_RADIUS, "{t:?}");
        }
    }

    #[test]
    fn hours() {
        assert_eq!(hour(0), 6);
        assert_eq!(hour(1_500), 7);
        assert_eq!(hour(15_000), 16);
        assert_eq!(hour(27_000), 0);
        assert_eq!(hour(DAY_TICKS - 1), 5);
    }
}
