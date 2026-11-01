# Moria : les lumières, document de conception

*Date : 2026-11-01 · Auteur : Nicolas Lallier (avec Claude) · Specs liées : `2026-10-31-moria-design.md`, `2026-11-01-dehors-et-bois-design.md`*

**Légende :**
- **[Décidé]** : choix de Nicolas.
- **[Proposé]** : choix de Claude, à confirmer par Nicolas à la relecture.
- **[Au constructeur]** : laissé à celui qui construit.

---

## 1. Intention

Aujourd'hui, tout l'étage affiché est visible en permanence, de jour comme de nuit, en surface comme au fond de la mine. Nicolas veut que **le noir existe** : la nuit dehors, et toujours dans les mines. Dans le noir, on ne voit que ce qu'une lumière éclaire. Explorer une galerie devient alors une avancée à la torche, et éclairer sa forteresse devient un vrai travail de construction.

## 2. Ce qu'on voit à l'écran

- **[Décidé]** Dans le noir, le héros a un **cercle de lumière** autour de lui. Le reste de l'écran est **noir**.
- **[Décidé]** **Chaque nain** a aussi son propre cercle de lumière. On voit donc les nains au travail, même loin du héros.
- **[Décidé]** **La roche bloque la lumière.** On ne voit pas derrière un coin de galerie. Une case est visible si une source de lumière a une ligne de vue dégagée jusqu'à elle, dans sa portée.
- **[Décidé]** Des **sources de lumière construites** éclairent aussi (§4).
- **[Proposé]** Les cases qui bloquent la lumière sont celles qui sont « solides » au sens de `Tile::is_stone` : roche, or, mithril, terre et murs. Elles reçoivent elles-mêmes la lumière : on voit la paroi qui borde une galerie, mais pas ce qu'il y a derrière. Les arbres, les meubles et les sources de lumière **ne bloquent pas**.
- **[Proposé]** Le cercle s'éteint en douceur sur sa dernière case (un fondu), pas d'un bord net.
- **[Décidé]** **Souvenir en sombre** : une case qu'on a déjà vue reste affichée, **assombrie**, quand plus aucune lumière ne l'éclaire. Une case jamais vue reste noire.
- **[Décidé]** Le souvenir vaut **pour tous les étages**, y compris un étage qu'on regarde à distance (`browse_z`) où le héros n'est pas.
- **[Proposé]** Une case « vue » est une case qui a été éclairée à l'écran, par n'importe quelle lumière (héros, nain, source construite, jour), à n'importe quel étage affiché.
- **[Proposé]** Une case en souvenir montre son **état actuel**, pas son état au moment où on l'a vue (le serveur envoie déjà toute la carte, c'est plus simple). Si les nains creusent dans le noir, la galerie apparaît donc dans le souvenir.
- **[Proposé]** Dans une zone en souvenir, les **têtes des nains sont cachées** : on ne voit les nains que là où il y a de la lumière (et un nain éclaire toujours sa propre case). Les **contours de désignation restent visibles** dans le souvenir, pour qu'on puisse planifier un tunnel.
- **[Proposé]** Le souvenir est **propre à chaque joueur** et se **garde d'une connexion à l'autre** : le client l'enregistre dans un fichier local, par serveur et par compte. Le perdre à chaque reconnexion rendrait le souvenir presque inutile.
- **[Proposé]** Le souvenir sert aussi la nuit **dehors** : une forêt explorée le jour reste visible en sombre la nuit.
- **[Au constructeur]** Le format du fichier de souvenir (un bit par case suffit : 176 × 96 × 8 étages ≈ 17 ko).

## 3. Le jour, la nuit et les mines

- **[Décidé]** **1 heure réelle = 1 jour de jeu.** Avec le tick de 100 ms, un jour dure 36 000 ticks.
- **[Décidé]** La nuit tombe en surface. **[Proposé]** Les **mines** sont **toujours noires**, jour comme nuit.
- **[Proposé]** Une case est **dehors** quand la case juste au-dessus d'elle est du ciel (`Tile::Sky`). C'est la règle déjà utilisée par `Mountain::ground_at`. Une case dehors reçoit la lumière du jour. **[Proposé, après relecture]** Une case solide qui touche (sur ses 4 côtés) une case éclairée par le jour est éclairée aussi. Ainsi, le flanc d'une colline ou la face de la montagne restent visibles à midi, comme aujourd'hui, et seule l'entrée d'une grotte est éclairée, pas son fond. Toutes les autres cases sont dans le noir, sauf là où une source les éclaire.
- **[Proposé]** Le cycle : environ 25 minutes de plein jour, 5 minutes de crépuscule, 25 minutes de nuit et 5 minutes d'aube. Pendant le crépuscule et l'aube, la lumière du jour monte ou baisse progressivement.
- **[Proposé]** La nuit n'est pas totalement noire dehors. Il reste une très faible lueur de lune, assez pour deviner la forme du terrain mais pas les détails. Dans les mines, en revanche, le noir est complet (hors souvenir).
- **[Proposé]** L'heure du jeu vient du **tick du monde côté serveur** (`World::tick`). Il est déjà sauvegardé, donc l'heure survit aux redémarrages et tous les clients voient la même heure. L'heure est `tick % 36 000`. Au tick 0, la partie commence le matin.
- **[Proposé]** Un **indicateur d'heure** (soleil ou lune) s'affiche dans le HUD, pour qu'on sache quand la nuit approche.

## 4. Les sources de lumière à construire

**[Décidé]** Elles se construisent **comme les meubles** : on les désigne (ou le héros les pose avec `Build`) et les nains les fabriquent avec la ressource indiquée.

**[Décidé]** Le tableau (validé par Nicolas) :

| Objet | Coût | Portée de la lumière |
|---|---|---|
| Chandelle | 1 bois | 3 cases |
| Torche | 1 bois | 5 cases |
| Lampe | 1 pierre + 1 or | 6 cases |
| Feu de bois | 3 bois | 7 cases |
| Foyer | 4 pierre | 8 cases |
| Poêle | 3 pierre + 2 bois | 6 cases |
| *(héros, chaque nain)* | — | 4 cases |

- **[Proposé]** Une fois posées, les sources brillent **pour toujours**, sans combustible.
- **[Proposé, corrigé après relecture]** Elles se comportent **exactement comme un meuble** : dans le code, les meubles se traversent (`Mountain::walkable` inclut `is_furniture()`), et les sources aussi, pour qu'une torche ne bouche jamais un couloir. Elles se posent sur une case de sol (`Floor` ou `Grass`) et se démontent en les creusant, ce qui rend leur coût. Elles rejoignent `Tile::is_furniture()`.
- **[Proposé]** Elles apparaissent dans le **menu Construire**, après les meubles, avec leurs noms français ci-dessus.
- **[Au constructeur]** Le dessin des six tuiles (procédural dans `terrain.rs`, comme les meubles). Chacune doit se reconnaître d'un coup d'œil.

## 5. Non-objectifs

- Pas de combustible, pas d'extinction des feux.
- Pas de chaleur ni de température : le foyer et le poêle ne font qu'éclairer pour l'instant.
- Pas de couleur de lumière (une torche n'est pas plus orange qu'une lampe). Plus tard, si on veut.
- Pas de brouillard de guerre côté serveur : le serveur continue d'envoyer toute la carte, et le noir est un effet de rendu du client. Ce n'est pas une protection anti-triche.
- Le noir ne change pas le comportement des nains : ils travaillent la nuit comme le jour.

## 6. Anti-objectifs

- **Le client rame.** Le calcul de visibilité ne doit pas faire chuter le nombre d'images par seconde. Il se limite aux cases à l'écran, et on ne le refait que quand quelque chose a changé (déplacement, case modifiée, palier d'heure).
- **Le joueur est perdu.** Le cercle du héros doit toujours montrer assez de terrain pour se déplacer et creuser sans tâtonner.
- **Le jour devient pénible.** En plein jour dehors, l'écran doit rester exactement comme aujourd'hui.

## 7. Contraintes techniques

- Le protocole change (nouvelles tuiles et nouvelles désignations). `PROTOCOL_VERSION` passe à **6**, et les variants s'ajoutent **à la fin** de `Tile` et de `DesignationKind` (encodage postcard).
- **[Au constructeur]** Comment le client connaît l'heure dès le `Snapshot` : ajouter `tick` à `FortressView`, ou attendre le premier `TickDelta` (100 ms plus tard).
- **[Au constructeur]** L'algorithme de ligne de vue : shadowcasting récursif ou lancer de rayons sur les cases à l'écran. Il doit vivre dans `moria-core`, sans dépendance à Bevy, pour être testé.
- **[Au constructeur]** Comment assombrir à l'écran : teinte des sprites existants (`Sprite::color`, comme la variation actuelle) ou calque noir par-dessus.
- Les règles du dépôt s'appliquent : TDD (RED puis GREEN) pour chaque tâche.

## 8. Ordre de construction

1. **Le noir et la vision** : cycle jour/nuit, cercles du héros et des nains, la roche qui bloque, l'indicateur d'heure.
2. **Les sources de lumière** : les six objets, leur coût, leur construction par les nains, leur lumière.

Chaque étape est jouable et livrable seule.
