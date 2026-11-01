//! French or English for every text the client shows.

use std::sync::atomic::{AtomicBool, Ordering};

// ponytail: one global flag, also read by the network thread; a per-text table if a 3rd language comes.
static EN: AtomicBool = AtomicBool::new(false);

pub fn en() -> bool {
    EN.load(Ordering::Relaxed)
}

pub fn set_en(on: bool) {
    EN.store(on, Ordering::Relaxed);
}

/// The text in the current language.
pub fn tr(fr: &'static str, en_text: &'static str) -> &'static str {
    if en() { en_text } else { fr }
}

/// A table label (build, action, resource, race...) in the current language; tables keep the French.
pub fn word(fr: &str) -> &str {
    if en() { english(fr).unwrap_or(fr) } else { fr }
}

fn english(fr: &str) -> Option<&'static str> {
    Some(match fr {
        // Builds and workshops.
        "Mur" => "Wall",
        "Lit" => "Bed",
        "Table" => "Table",
        "Chaise" => "Chair",
        "Lit en bois" => "Wooden bed",
        "Table en bois" => "Wooden table",
        "Chaise en bois" => "Wooden chair",
        "Chandelle" => "Candle",
        "Torche" => "Torch",
        "Lampe" => "Lamp",
        "Feu de bois" => "Campfire",
        "Foyer" => "Hearth",
        "Poêle" => "Stove",
        "Forge" => "Forge",
        "Enclume" => "Anvil",
        "Porte en bois" => "Wooden door",
        "Porte en pierre" => "Stone door",
        "Porte en fer" => "Iron door",
        "Champ" => "Field",
        "gratuit" => "free",
        "Lingot de fer" => "Iron ingot",
        // Bottom bar and order menu.
        "Creuser [1]" => "Dig [1]",
        "Escalier [2]" => "Stairs [2]",
        "Construire [3]" => "Build [3]",
        "Couper [4]" => "Chop [4]",
        "+1" => "+1",
        "+5" => "+5",
        "Annuler la commande" => "Cancel the order",
        // Resources: on the bar, then inside a sentence.
        "Pierre" => "Stone",
        "Bois" => "Wood",
        "Or" => "Gold",
        "Mithril" => "Mithril",
        "Minerai de fer" => "Iron ore",
        "Charbon" => "Coal",
        "Lingots" => "Ingots",
        "Portes en fer" => "Iron doors",
        "Nourriture" => "Food",
        "pierre" => "stone",
        "bois" => "wood",
        "or" => "gold",
        "minerai de fer" => "iron ore",
        "charbon" => "coal",
        "lingots" => "ingots",
        "porte en fer forgée" => "forged iron door",
        // Abilities.
        "Force" => "Strength",
        "Dextérité" => "Dexterity",
        "Constitution" => "Constitution",
        "Intelligence" => "Intelligence",
        "Sagesse" => "Wisdom",
        "Charisme" => "Charisma",
        // Races.
        "Nain" => "Dwarf",
        "Elfe" => "Elf",
        "Halfelin" => "Halfling",
        "Humain" => "Human",
        "Drakéide" => "Dragonborn",
        "Gnome" => "Gnome",
        "Demi-elfe" => "Half-elf",
        "Demi-orc" => "Half-orc",
        "Tieffelin" => "Tiefling",
        // Classes.
        "Barbare" => "Barbarian",
        "Barde" => "Bard",
        "Clerc" => "Cleric",
        "Druide" => "Druid",
        "Guerrier" => "Fighter",
        "Moine" => "Monk",
        "Paladin" => "Paladin",
        "Rôdeur" => "Ranger",
        "Roublard" => "Rogue",
        "Ensorceleur" => "Sorcerer",
        "Occultiste" => "Warlock",
        "Magicien" => "Wizard",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::{
        ACTIONS, BUILDS, ORDER_ROWS, ability_rows, named, recipe_label, resource_bar,
    };
    use moria_core::srd::{Abilities, Class, Race};
    use moria_core::{DesignationKind, Resources};

    /// Every label shown from a table has an English word: a new one fails here.
    #[test]
    fn every_table_label_has_an_english_word() {
        let a = Abilities {
            str: 10,
            dex: 10,
            con: 10,
            int: 10,
            wis: 10,
            cha: 10,
        };
        let labels = BUILDS
            .iter()
            .map(|b| b.1)
            .chain(ACTIONS.iter().map(|a| a.0))
            .chain(ORDER_ROWS.iter().map(|r| r.0))
            .chain(resource_bar(Resources::default()).map(|r| r.1))
            .chain(named(Resources::default()).map(|r| r.1))
            .chain(ability_rows(a).map(|r| r.0))
            .chain(Race::ALL.map(Race::label_fr))
            .chain(Class::ALL.map(Class::label_fr))
            .chain([DesignationKind::Smelt, DesignationKind::Smith].map(recipe_label));
        for l in labels {
            assert!(english(l).is_some(), "no English for {l:?}");
        }
    }
}
