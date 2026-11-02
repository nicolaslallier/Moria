//! Seasons calendar: a pure function of the world tick.

use serde::{Deserialize, Serialize};

use crate::light::DAY_TICKS;

/// Game days in one season.
pub const SEASON_DAYS: u64 = 7;
/// Ticks in one season.
pub const SEASON_TICKS: u64 = SEASON_DAYS * DAY_TICKS;
/// Ticks in one year (4 seasons).
pub const YEAR_TICKS: u64 = 4 * SEASON_TICKS;

/// Season of the year; tick 0 is the first day of spring.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Season {
    Spring,
    Summer,
    Autumn,
    Winter,
}

/// Season at `tick`.
pub fn season(tick: u64) -> Season {
    match tick % YEAR_TICKS / SEASON_TICKS {
        0 => Season::Spring,
        1 => Season::Summer,
        2 => Season::Autumn,
        _ => Season::Winter,
    }
}

/// Day within the season, 0..=6.
pub fn day_of_season(tick: u64) -> u8 {
    (tick % SEASON_TICKS / DAY_TICKS) as u8
}

/// Year number; tick 0 is year 0.
pub fn year(tick: u64) -> u64 {
    tick / YEAR_TICKS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_zero_is_spring_day_zero() {
        assert_eq!(season(0), Season::Spring);
        assert_eq!(day_of_season(0), 0);
        assert_eq!(year(0), 0);
    }

    #[test]
    fn season_boundaries() {
        assert_eq!(season(SEASON_TICKS - 1), Season::Spring);
        assert_eq!(season(SEASON_TICKS), Season::Summer);
        assert_eq!(season(3 * SEASON_TICKS), Season::Winter);
        assert_eq!(season(YEAR_TICKS), Season::Spring);
        assert_eq!(year(YEAR_TICKS), 1);
    }

    #[test]
    fn day_of_season_cycles() {
        assert_eq!(day_of_season(6 * DAY_TICKS), 6);
        assert_eq!(day_of_season(7 * DAY_TICKS), 0);
    }

    #[test]
    fn year_length_is_28_days() {
        assert_eq!(YEAR_TICKS, 28 * DAY_TICKS);
    }
}
