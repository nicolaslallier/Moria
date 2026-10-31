use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Race {
    Dwarf,
    Elf,
    Halfling,
    Human,
    Dragonborn,
    Gnome,
    HalfElf,
    HalfOrc,
    Tiefling,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    Barbarian,
    Bard,
    Cleric,
    Druid,
    Fighter,
    Monk,
    Paladin,
    Ranger,
    Rogue,
    Sorcerer,
    Warlock,
    Wizard,
}

impl Race {
    pub const ALL: [Race; 9] = [
        Race::Dwarf,
        Race::Elf,
        Race::Halfling,
        Race::Human,
        Race::Dragonborn,
        Race::Gnome,
        Race::HalfElf,
        Race::HalfOrc,
        Race::Tiefling,
    ];

    pub fn label_fr(self) -> &'static str {
        match self {
            Race::Dwarf => "Nain",
            Race::Elf => "Elfe",
            Race::Halfling => "Halfelin",
            Race::Human => "Humain",
            Race::Dragonborn => "Drakéide",
            Race::Gnome => "Gnome",
            Race::HalfElf => "Demi-elfe",
            Race::HalfOrc => "Demi-orc",
            Race::Tiefling => "Tieffelin",
        }
    }

    /// Bonus per ability, indexed STR, DEX, CON, INT, WIS, CHA.
    fn bonus(self) -> [u8; 6] {
        match self {
            Race::Dwarf => [0, 0, 2, 0, 0, 0],
            Race::Elf | Race::Halfling => [0, 2, 0, 0, 0, 0],
            Race::Human => [1; 6],
            Race::Dragonborn => [2, 0, 0, 0, 0, 1],
            Race::Gnome => [0, 0, 0, 2, 0, 0],
            Race::HalfElf => [0, 1, 1, 0, 0, 2],
            Race::HalfOrc => [2, 0, 1, 0, 0, 0],
            Race::Tiefling => [0, 0, 0, 1, 0, 2],
        }
    }
}

impl Class {
    pub const ALL: [Class; 12] = [
        Class::Barbarian,
        Class::Bard,
        Class::Cleric,
        Class::Druid,
        Class::Fighter,
        Class::Monk,
        Class::Paladin,
        Class::Ranger,
        Class::Rogue,
        Class::Sorcerer,
        Class::Warlock,
        Class::Wizard,
    ];

    pub fn label_fr(self) -> &'static str {
        match self {
            Class::Barbarian => "Barbare",
            Class::Bard => "Barde",
            Class::Cleric => "Clerc",
            Class::Druid => "Druide",
            Class::Fighter => "Guerrier",
            Class::Monk => "Moine",
            Class::Paladin => "Paladin",
            Class::Ranger => "Rôdeur",
            Class::Rogue => "Roublard",
            Class::Sorcerer => "Ensorceleur",
            Class::Warlock => "Occultiste",
            Class::Wizard => "Magicien",
        }
    }

    /// Ability indices (STR0 DEX1 CON2 INT3 WIS4 CHA5), highest priority first.
    fn priority(self) -> [usize; 6] {
        match self {
            Class::Barbarian | Class::Fighter => [0, 2, 1, 4, 5, 3],
            Class::Bard => [5, 1, 2, 4, 3, 0],
            Class::Cleric => [4, 2, 0, 5, 1, 3],
            Class::Druid => [4, 2, 1, 3, 5, 0],
            Class::Monk | Class::Ranger => [1, 4, 2, 0, 3, 5],
            Class::Paladin => [0, 5, 2, 4, 1, 3],
            Class::Rogue => [1, 2, 3, 5, 4, 0],
            Class::Sorcerer | Class::Warlock => [5, 2, 1, 4, 3, 0],
            Class::Wizard => [3, 2, 1, 4, 5, 0],
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Abilities {
    pub str: u8,
    pub dex: u8,
    pub con: u8,
    pub int: u8,
    pub wis: u8,
    pub cha: u8,
}

pub fn modifier(score: u8) -> i8 {
    (score as i8 - 10).div_euclid(2)
}

/// Standard array 15,14,13,12,10,8 by class priority, plus the race bonus.
pub fn roll_abilities(race: Race, class: Class) -> Abilities {
    let mut s = race.bonus();
    for (rank, idx) in class.priority().into_iter().enumerate() {
        s[idx] += [15, 14, 13, 12, 10, 8][rank];
    }
    Abilities {
        str: s[0],
        dex: s[1],
        con: s[2],
        int: s[3],
        wis: s[4],
        cha: s[5],
    }
}

/// `max(1, base - str_mod)`.
pub fn hero_dig_ticks(tile_ticks: u16, a: &Abilities) -> u16 {
    (tile_ticks as i32 - modifier(a.str) as i32).max(1) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dwarf_fighter_stats() {
        assert_eq!(
            roll_abilities(Race::Dwarf, Class::Fighter),
            Abilities {
                str: 15,
                con: 16,
                dex: 13,
                wis: 12,
                cha: 10,
                int: 8
            }
        );
    }
    #[test]
    fn human_wizard_stats() {
        assert_eq!(
            roll_abilities(Race::Human, Class::Wizard),
            Abilities {
                int: 16,
                con: 15,
                dex: 14,
                wis: 13,
                cha: 11,
                str: 9
            }
        );
    }
    #[test]
    fn strong_hero_digs_faster() {
        let a = |s| Abilities {
            str: s,
            dex: 10,
            con: 10,
            int: 10,
            wis: 10,
            cha: 10,
        };
        assert_eq!(hero_dig_ticks(10, &a(17)), 7);
        assert_eq!(hero_dig_ticks(10, &a(8)), 11);
        assert_eq!(hero_dig_ticks(2, &a(30)), 1);
        assert_eq!(modifier(8), -1);
    }
    #[test]
    fn all_lists_complete() {
        assert_eq!((Race::ALL.len(), Class::ALL.len()), (9, 12));
    }
}
