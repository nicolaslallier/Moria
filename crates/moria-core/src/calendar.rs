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
    fn every_season_arm_and_last_ticks() {
        assert_eq!(season(2 * SEASON_TICKS - 1), Season::Summer);
        assert_eq!(season(2 * SEASON_TICKS), Season::Autumn);
        assert_eq!(season(3 * SEASON_TICKS - 1), Season::Autumn);
        assert_eq!(season(YEAR_TICKS - 1), Season::Winter);
        assert_eq!(day_of_season(DAY_TICKS - 1), 0);
        assert_eq!(day_of_season(DAY_TICKS), 1);
        assert_eq!(day_of_season(SEASON_TICKS - 1), 6);
        assert_eq!(year(YEAR_TICKS - 1), 0);
    }

    #[test]
    fn huge_tick_does_not_panic() {
        for t in [u64::MAX, u64::MAX - 1, u64::MAX / 2] {
            let _ = season(t);
            assert!(day_of_season(t) <= 6, "{t}");
            assert_eq!(year(t), t / YEAR_TICKS);
        }
    }

    #[test]
    fn year_is_28_days_of_four_seasons() {
        // Walk a year day by day: 7 days per season, in order, then year 1 starts.
        let mut seen = vec![];
        for d in 0..28 {
            let t = d * DAY_TICKS;
            assert_eq!(u64::from(day_of_season(t)), d % 7, "day {d}");
            assert_eq!(year(t), 0);
            seen.push(season(t));
        }
        seen.dedup();
        use Season::*;
        assert_eq!(seen, vec![Spring, Summer, Autumn, Winter]);
        assert_eq!((season(28 * DAY_TICKS), year(28 * DAY_TICKS)), (Spring, 1));
    }
}
