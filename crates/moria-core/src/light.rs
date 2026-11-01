//! Day/night cycle and lighting.

/// One real hour = one game day, at 100 ms per tick.
pub const DAY_TICKS: u64 = 36_000;
const HOUR_TICKS: u64 = 1_500;
const DUSK_START: u64 = 10 * HOUR_TICKS;
const NIGHT_START: u64 = 12 * HOUR_TICKS;
const DAWN_START: u64 = 22 * HOUR_TICKS;

/// Daylight level: 1.0 by day, 0.0 at night, linear through dusk and dawn.
pub fn daylight(tick: u64) -> f32 {
    let t = tick % DAY_TICKS;
    let ramp = |from: u64| (t - from) as f32 / (2 * HOUR_TICKS) as f32;
    if t < DUSK_START {
        1.0
    } else if t < NIGHT_START {
        1.0 - ramp(DUSK_START)
    } else if t < DAWN_START {
        0.0
    } else {
        ramp(DAWN_START)
    }
}

/// Game hour 0..=23; tick 0 is 6 h.
pub fn hour(tick: u64) -> u8 {
    ((6 + tick % DAY_TICKS / HOUR_TICKS) % 24) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn hours() {
        assert_eq!(hour(0), 6);
        assert_eq!(hour(1_500), 7);
        assert_eq!(hour(15_000), 16);
        assert_eq!(hour(27_000), 0);
        assert_eq!(hour(DAY_TICKS - 1), 5);
    }
}
