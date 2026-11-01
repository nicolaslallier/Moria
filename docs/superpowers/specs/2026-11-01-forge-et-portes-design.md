# Moria : le fer, la forge et les portes, document de conception

*Date : 2026-11-01 · Auteur : Nicolas Lallier (avec Claude) · Specs liées : `2026-10-31-moria-design.md`, `2026-11-01-dehors-et-bois-design.md`, `2026-11-01-lumieres-design.md`*

**Légende :**
- **[Décidé]** : choix de Nicolas.
- **[Proposé]** : choix de Claude, à confirmer par Nicolas à la relecture.
- **[Au constructeur]** : laissé à celui qui construit.

---

## 1. Intention

Aujourd'hui, creuser donne une ressource tout de suite, et construire la dépense tout de suite. Rien ne se **transforme**. Nicolas veut une première vraie chaîne de fabrication : trouver du **minerai de fer**, le **fondre à la forge** avec du **charbon** pour en faire des **lingots**, puis **forger les lingots à l'enclume** en objets. Le premier objet forgé est la **porte en fer**.

Les portes servent à **fermer des pièces** : Nicolas veut pouvoir **tenir ses propres nains hors d'une pièce**. Il n'y a pas d'ennemis dans le jeu, et cette conception n'en ajoute pas.

## 2. Les nouvelles ressources

- **[Décidé]** Le **minerai de fer** et le **charbon** se trouvent **en filons dans la roche**, comme l'or.
- **[Proposé]** Deux nouvelles cases, `IronOre` et `Coal`. Creuser l'une donne **1 minerai de fer**, l'autre **1 charbon**. Le temps de creusage est de 15 ticks pour le minerai (comme l'or) et de 12 pour le charbon.
- **[Proposé]** Les filons apparaissent dans la montagne à **tous les étages, sous le sol** (`z >= GROUND_Z`). On en compte environ **10 de charbon** et **8 de fer** par étage, soit un peu moins que l'or (12) : ils doivent être faciles à trouver près de l'entrée. On en met aussi **à moitié moins** sous la terre du dehors, comme l'or (`z > GROUND_Z + 2`).
- **[Proposé, après relecture]** Les filons de fer et de charbon sont tirés **après** tout le reste de la génération, ou avec un RNG séparé, pour qu'un monde neuf garde la même roche, le même or et le même dehors pour une même graine.
- **[Proposé]** Le stock commun (`Resources`) gagne **quatre compteurs** : `iron_ore` (minerai), `coal` (charbon), `iron` (lingots) et `iron_door` (portes en fer forgées, pas encore posées). Ces quatre compteurs s'affichent dans le HUD à côté de la pierre, du bois et de l'or.
- **[Proposé]** La porte en fer forgée est rangée dans le stock comme une ressource. C'est le premier **objet fabriqué** du jeu. On ne crée pas encore de système d'inventaire général : un compteur suffit tant qu'il n'y a qu'un seul objet.

## 3. Les ateliers : forge et enclume

- **[Décidé]** La **forge** transforme du minerai de fer et du charbon en **lingots de fer**.
- **[Décidé]** L'**enclume** transforme des lingots en objets. Dans cette version, elle ne fait **que la porte en fer**.
- **[Proposé]** Forge et enclume se construisent **comme des meubles**, avec le menu Construire, par désignation ou avec `Build` par le héros. Elles se posent sur une case de sol (`Floor` ou `Grass`), se traversent comme les autres meubles et se démontent en les creusant, ce qui rend leur coût. Leurs cases s'appellent `Forge` et `Anvil`.
- **[Proposé]** Les recettes et les coûts :

| Quoi | Coût | Travail du nain |
|---|---|---|
| Construire une forge | 6 pierre | comme un meuble |
| Construire une enclume | 3 lingots | comme un meuble |
| 1 lingot (à la forge) | 2 minerai + 1 charbon | 30 ticks (3 s) |
| 1 porte en fer (à l'enclume) | 2 lingots | 40 ticks (4 s) |

  L'enclume coûte des lingots : on doit donc d'abord construire la forge et fondre quelques lingots avant de pouvoir forger. C'est voulu, cela donne une progression.
- **[Proposé]** La forge **éclaire** comme une petite source de lumière, avec une portée de 4 cases, parce que c'est un feu. L'enclume n'éclaire pas.

## 4. Les commandes

- **[Décidé]** C'est **le joueur qui commande**, par exemple « fais 5 lingots ». Un **nain va travailler tout seul** à l'atelier.
- **[Proposé]** Une commande est attachée **à un atelier précis**. Elle porte une recette et un nombre restant, par exemple « forge en (140, 48, 5) : 5 lingots ». Un atelier n'a qu'une commande à la fois. En passer une nouvelle **ajoute** au nombre restant, et on peut l'**annuler**. Le plafond est de 99 par atelier.
- **[Proposé]** Pour passer une commande, **le héros se place à côté de l'atelier et appuie sur le bouton « utiliser »** (le même que pour les portes, voir §5). Un petit menu s'ouvre avec la recette de l'atelier, son coût et les choix **+1**, **+5** et **Annuler la commande**. *(Cohérent avec les portes : on agit sur les choses en allant à côté.)*
- **[Proposé]** Le nombre restant s'affiche **sur l'atelier**, sous forme d'un petit chiffre, pour voir d'un coup d'œil ce qui est en cours.
- **[Proposé]** Le travail réutilise le circuit actuel des désignations. Un atelier qui a une commande en cours et **assez de matière en stock pour une unité** est proposé au service des nains (`moria-dwarf-ai`) comme une tâche, au même titre qu'une case à creuser. Un nain libre s'y rend, se place sur une case voisine et travaille le nombre de ticks de la recette. À la fin, le serveur **prend** la matière dans le stock, **ajoute** le produit et retire 1 au nombre restant. Le nain redevient libre, et l'atelier se repropose s'il reste quelque chose à faire. **Un seul nain** travaille à un atelier à la fois.
- **[Proposé]** Si la matière **manque**, la commande **attend** sans être annulée : l'atelier n'est simplement plus proposé aux nains. Si la matière disparaît pendant que le nain travaille (dépensée ailleurs), l'unité n'est pas produite et rien n'est pris. La matière est vérifiée et prise **à la fin** du travail.
- **[Proposé]** Si on démonte un atelier, sa commande disparaît avec lui.
- **[Proposé]** Les commandes sont **sauvegardées** avec le monde. `SAVE_VERSION` passe à 3.
- **[Proposé]** Pendant ce travail, la tâche du nain s'affiche avec son atelier (dans `DwarfJob`), pour que le client le montre au travail comme pour le creusage.

## 5. Les portes

- **[Décidé]** Trois portes : en **bois**, en **pierre** et en **fer**.
- **[Décidé]** Une porte sert à **empêcher ses propres nains d'entrer** dans une pièce.
- **[Décidé]** Pour **verrouiller ou déverrouiller** une porte, **le héros va à côté et appuie sur un bouton**.
- **[Proposé]** Les coûts : porte en bois **3 bois**, porte en pierre **3 pierre**, porte en fer **1 porte en fer forgée** (prise dans le stock, voir §2). Les trois se posent comme des meubles (menu Construire, désignation ou `Build`), sur une case de sol. Elles se démontent en les creusant, ce qui rend leur coût, et la porte en fer revient dans le stock comme objet.
- **[Proposé]** Les trois portes ont **exactement le même comportement**. Elles ne diffèrent que par leur coût et leur dessin. Aucune n'est plus « solide », puisqu'il n'y a pas d'ennemis.
- **[Proposé]** Une porte a **deux états**, **ouverte** et **verrouillée**. Une porte qui vient d'être posée est ouverte.
  - **Ouverte** : les nains et le héros la traversent comme un meuble.
  - **Verrouillée** : **les nains ne la traversent pas**. Pour eux, c'est un mur, dans leur recherche de chemin comme dans l'attribution des tâches. Un nain ne prend donc jamais un travail qu'il ne peut atteindre qu'en passant par une porte verrouillée.
  - **Le héros passe toujours**, même par une porte verrouillée : c'est lui qui a la clé.
- **[Proposé]** Le bouton « utiliser » est une nouvelle action du héros vers une direction, comme creuser. Contre une porte, il bascule ouvert ↔ verrouillé. Contre un atelier, il ouvre le menu de commande (§4). Contre autre chose, il ne fait rien.
- **[Proposé]** Si on verrouille une porte pendant qu'un nain est en chemin à travers elle, le nain abandonne sa tâche et en reprend une autre, comme quand une case désignée devient inaccessible aujourd'hui. Un nain qui se retrouve **enfermé** dans une pièce reste enfermé jusqu'à ce que le héros déverrouille : le joueur voit la situation et la règle lui-même.
- **[Proposé]** Une porte **bloque la lumière et la vue** comme un mur, verrouillée ou non, **sauf quand quelqu'un (nain ou héros) se tient dedans**. Elle-même reçoit la lumière, comme une paroi. Fermer une pièce la cache donc vraiment.
- **[Proposé]** Une porte verrouillée se reconnaît à l'écran, avec par exemple une barre ou un cadenas dessiné dessus.
- **[Au constructeur]** Comment représenter l'état verrouillé. On peut utiliser des variantes de cases séparées (`WoodDoor`, `WoodDoorLocked`, etc.), ce qui ne demande aucun nouveau message puisque le changement passe par `TickDelta::tiles` comme les autres cases. On peut aussi garder une donnée à part dans `Mountain`. La première solution est la plus simple.

## 6. Non-objectifs

- Rien d'autre à l'enclume que la porte en fer : ni outils, ni armes, ni pièces de poêle. Plus tard, si on veut.
- Pas de transport de matière : tout passe par le stock commun, comme aujourd'hui. Le nain ne porte pas le minerai jusqu'à la forge.
- Pas de file de plusieurs recettes par atelier, et pas de commande « en continu ».
- Pas d'ennemis, et pas de portes plus ou moins solides.
- Pas de clés, ni de portes réservées à certains nains.
- Pas de porte qui s'ouvre et se ferme toute seule avec une animation.

## 7. Anti-objectifs

- **Une commande qui ne bouge jamais sans qu'on sache pourquoi.** Si la matière manque, le joueur doit pouvoir le voir, au moins dans le menu de l'atelier, qui montre le coût à côté du stock.
- **Des nains qui tournent en rond** devant une porte verrouillée, ou qui reprennent sans fin une tâche inaccessible.

## 8. Contraintes techniques

- Le protocole change (nouvelles cases, nouvelles désignations, nouvelles ressources, action « utiliser », commandes). `PROTOCOL_VERSION` passe à **8**. Les variants s'ajoutent **à la fin** de `Tile` et de `DesignationKind` (encodage postcard).
- `SAVE_VERSION` passe à **3**. Aucune lecture de la v2 n'est nécessaire (voir ci-dessous).
- **[Décidé]** **Le monde est remis à zéro au déploiement**, pour que toutes les montagnes aient leurs filons de fer et de charbon. Concrètement, on régénère `world.bin` avec `moria-worldgen` avant de démarrer le nouveau serveur. Le serveur actuel refuse déjà une sauvegarde d'une autre version (`world::load`), donc il ne peut pas démarrer par erreur sur l'ancien monde.
- **[Conséquence]** Les comptes vivent dans `world.bin` : la remise à zéro **efface aussi les comptes**, et chaque joueur recrée le sien.
- **[Au constructeur]** Comment le client connaît les commandes en cours, au `Snapshot` comme dans le `TickDelta`.
- **[Au constructeur]** Comment le service `moria-dwarf-ai` reçoit les ateliers à servir, par exemple comme des désignations d'un nouveau genre placées sur la case de l'atelier, et comment il traite une porte verrouillée dans `walkable`.
- **[Au constructeur]** Les dessins des nouvelles cases (procéduraux dans `terrain.rs`, comme les meubles) : les deux filons, la forge, l'enclume et les trois portes, ouvertes et verrouillées.
- Les règles du dépôt s'appliquent : TDD (RED puis GREEN) pour chaque tâche.

## 9. Ordre de construction

1. **Filons et portes** : minerai de fer et charbon dans la roche, nouvelles ressources, portes en bois et en pierre, bouton « utiliser » et verrouillage.
2. **La forge** : l'atelier, les commandes et le travail des nains, les lingots.
3. **L'enclume et la porte en fer** : forger la porte, la poser.

Chaque étape est jouable et livrable seule.
