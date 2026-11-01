# Moria : le dehors et le bois, document de conception

*Date : 2026-11-01 · Auteur : Nicolas Lallier (avec Claude) · Spec mère : `2026-10-31-moria-design.md`*

**Légende :**
- **[Décidé]** : choix de Nicolas.
- **[Proposé]** : choix de Claude, accepté par Nicolas (« tout est beau »), réglable plus tard.
- **[Au constructeur]** : laissé à celui qui construit.

---

## 1. Intention

Jusqu'ici, la carte de chaque joueur est une montagne pleine de 48 × 48 cases sur 8 étages, et la seule ouverture est une petite entrée au bord ouest. Ce chantier donne un **dehors** à chaque forteresse : une forêt, une rivière, un lac et des collines, au pied de la montagne. Il ajoute aussi une deuxième ressource de construction, **le bois**.

Nicolas veut pouvoir « aller dehors pour couper du bois ». Le bois sert d'abord à fabriquer des **meubles** (lit, table, chaise). Plus tard, il servira aussi à chauffer, à construire des murs et à poser des planchers.

## 2. Objectifs, non-objectifs, anti-objectifs

**Objectifs**
- **[Décidé]** Une seule carte continue. En franchissant l'entrée de la forteresse, le héros arrive dehors sans changement d'écran ni chargement.
- **[Décidé]** Le dehors est **privé**. Chaque joueur a le sien, comme sa montagne, et il n'est partagé avec aucune autre forteresse.
- **[Décidé]** Le relief est de type « A » : le sol de la forêt est **au même étage que l'entrée** de la forteresse. Les collines **montent** d'un ou deux étages, et la montagne s'élève plus haut encore. Ce choix a été fait sur une maquette en coupe le 2026-11-01.
- **[Décidé]** Les **collines permettent de changer d'étage**. Leurs pentes se gravissent et se descendent à pied.
- **[Décidé]** On peut **creuser sous la forêt**, comme sous la montagne.
- **[Décidé]** Le lac est à **une centaine de cases** de l'entrée.
- **[Décidé]** Le bois se récolte **comme la pierre**, de deux façons. Le héros abat un arbre lui-même, en contrôle direct. Ou le joueur **désigne** des arbres, et les nains vont les couper.
- **[Décidé]** Le bois est une **ressource** à part entière, à côté de la pierre, de l'or et du mithril.
- **[Décidé]** Le lit, la table et la chaise se construisent **en bois ou en pierre**, au choix.

**Non-objectifs (pour ce chantier)**
- **[Décidé, plus tard]** Le chauffage, les murs en bois et les planchers en bois. Le chauffage demande un système de température qui n'existe pas encore.
- Pas de repousse des arbres. **[Proposé, accepté]**
- Pas de traversée de l'eau (ni nage, ni pont), pas de pêche. **[Proposé, accepté]** L'eau ne se creuse pas et ne se modifie pas (§8).
- Pas de dehors partagé, pas de rencontres entre joueurs dehors, pas de créatures ni de factions dehors.
- Pas de cycle jour/nuit, pas de météo.

**Anti-objectifs** (ceux de la spec mère restent valables)
- **Le serveur lag.** Une carte plus grande ne doit pas faire déraper le tick de 100 ms ni les vues envoyées au service des nains.
- **Les nains sont stupides.** Exemples à proscrire :
  - un nain qui part couper un arbre inaccessible (de l'autre côté de l'eau) ;
  - un nain qui reste coincé sur une pente ;
  - un nain qui fait un détour absurde parce qu'il ne sait pas emprunter les collines ;
  - un nain qui coupe l'arbre le plus lointain quand un autre est à côté.
- **Perdre la forteresse de quelqu'un.** Un serveur déployé tourne déjà. Les forteresses existantes ne doivent pas disparaître sans que Nicolas l'ait voulu (voir §8).

## 3. La carte

### Taille et disposition **[Proposé, accepté]**
- La carte d'une forteresse passe de **48 × 48** à environ **176 × 96** cases :
  - la montagne, de 48 cases de large, est à l'est ;
  - un dehors de **128 cases** de large s'étend à l'ouest ;
  - la hauteur double, pour que la forêt ait de la place.
- **Étages :** **4 étages au-dessus du sol**, pour les collines et le haut de la montagne, plus les **8 étages souterrains** existants, soit 12 en tout.
- L'entrée de la forteresse s'ouvre sur le dehors, au niveau du sol.
- **[Au constructeur]** Les dimensions exactes et la représentation interne des étages au-dessus du sol. Aujourd'hui `z = 0` est la surface et `z` croît vers le bas. Les options sont un décalage (sol à `z = 4`) ou une autre convention. Le client affiche les étages à l'utilisateur de façon lisible ; le libellé exact (« Niveau +2 », « Niveau −3 »…) est au constructeur.

### Ce qu'on trouve dehors **[Proposé, accepté]**
- **Forêt** : clairsemée près de l'entrée, dense vers le lac.
- **Rivière** : elle part de la montagne et descend jusqu'au lac.
- **Lac** : à une centaine de cases de l'entrée.
- **Collines** : une à deux, qui montent d'un ou deux étages au-dessus du sol, avec des **pentes** praticables.
- **Sous-sol** : sous le sol extérieur, de la terre puis de la roche. Le sous-sol extérieur peut contenir des filons, mais moins qu'en montagne. **[Au constructeur]**
- **[Au constructeur]**
  - le générateur précis (bruit, nombre et forme des collines, tracé de la rivière, densité exacte de la forêt) ;
  - la règle doit garantir qu'**au moins une partie de la forêt est atteignable à pied depuis l'entrée**.
- Les nouveaux types de cases, au minimum : herbe, arbre, eau, pente, terre, et « ciel » (vide au-dessus du sol).

### Se déplacer **[Décidé + au constructeur]**
- **[Décidé]** Les pentes des collines font changer d'étage. Le principe est celui des « rampes » de Dwarf Fortress : on marche sur la pente et on se retrouve un étage plus haut ou plus bas, sans escalier.
- **[Proposé, accepté]** L'eau bloque le passage, pour le héros comme pour les nains.
- **[Au constructeur]** Les règles exactes de déplacement sur les pentes, pour le héros, les nains et la recherche de chemin.

## 4. Le bois

- **[Décidé]** Couper un arbre se fait comme creuser la roche. Le héros le fait lui-même, ou le joueur désigne des arbres et les nains les coupent.
- **[Proposé, accepté]** Un arbre coupé donne **3 bois** et laisse un sol d'herbe praticable.
- **[Au constructeur]** La durée pour abattre un arbre, sur l'échelle des durées de creusage existantes (roche 10 ticks, or 15, mithril 25), et l'éventuel effet de la Force du héros.
- La désignation de coupe suit **exactement** le circuit des désignations existantes :
  1. le joueur désigne une zone ;
  2. le service des nains attribue les tâches ;
  3. le serveur valide et exécute ;
  4. la ressource est ajoutée et la désignation retirée.
- Elle hérite donc des protections déjà en place contre les nains stupides : attribution au plus proche, pas deux nains sur la même tâche, rejet des cibles qui ne sont plus valides.
- Dans le client, un nouveau mode de désignation « Couper » s'ajoute aux modes existants (creuser, escalier, mur).

## 5. Les meubles en bois

- **[Décidé]** Le lit, la table et la chaise existent **en bois et en pierre**.
- **[Proposé, accepté]** Les coûts en bois vont de **2 à 4 bois** par meuble. L'ordre de grandeur est table 4, lit 3, chaise 2, à ajuster en jouant.
- **[Décidé, ordre]** La PR #13 (les meubles en pierre) est fusionnée **d'abord**. Le bois s'ajoute par-dessus et réutilise le même menu de construction (touche 3) et le même circuit de construction par les nains et le héros.
- **[Au constructeur]**
  - La représentation : un matériau sur le meuble, ou des types distincts.
  - Le choix du matériau dans le menu de construction.
  - L'apparence visuelle distincte bois/pierre (textures générées, comme le reste).
- La réservation des ressources pour les tâches des nains, qui existe déjà pour la pierre, s'applique aussi au bois.

## 6. Contraintes techniques (héritées)

- Rust, serveur central qui fait foi, service des nains séparé via NATS, client Bevy. Rien de lent ne bloque le tick.
- **Changement de protocole :** les nouveaux types de cases, le bois dans les ressources, le mode « Couper » et les meubles par matériau changent le protocole. Il faut incrémenter `PROTOCOL_VERSION`, qui vaut actuellement 2. Les positions figées du `Hello` et des refus de version restent intactes.
- **Changement de format de sauvegarde :** il faut incrémenter `SAVE_VERSION`, qui vaut actuellement 1.
- **Limites de taille à relever de façon cohérente** :
  - la validation des vues dans `moria-dwarf-ai`, aujourd'hui limitée à `MOUNTAIN_W/H/D` ;
  - la validation des snapshots dans le client (w, h ≤ 256, d ≤ 64 : 176 × 96 × 12 passe).
  - Ce sont des protections de sécurité, il faut les relever sans les supprimer.
- **Charge :** une carte de 176 × 96 × 12, soit environ 200 000 cases, contre 18 000 aujourd'hui, est envoyée au service des nains et au client. Il faut vérifier que les vues et snapshots restent sous la taille de trame maximale (1 MiB) et que le tick reste dans son budget avec 10 forteresses. **[Au constructeur]** Si besoin, compresser ou découper les vues.
- Le test de charge à 10 joueurs simulés (`examples/bots.rs`) doit continuer de passer : p99 < 150 ms.

## 7. Laissé au constructeur

- Les dimensions exactes, la convention des étages au-dessus du sol et leur affichage.
- Le générateur du dehors (collines, rivière, lac, forêt, sous-sol), avec la garantie d'accessibilité.
- Les règles de déplacement sur les pentes.
- Les durées de coupe et les coûts exacts des meubles.
- La représentation des meubles par matériau et le choix dans le menu.
- Les textures du dehors : herbe, arbre, eau, pente, ciel.
- L'éventuelle compression ou le découpage des vues et snapshots si la taille l'exige.

## 8. Décisions après relecture

### Tranchées par Nicolas
1. **[Décidé] Le monde existant est effacé et regénéré.** Il n'y a aucune migration des sauvegardes v1 : le serveur déployé repart d'un monde neuf, et les joueurs recréent leur compte et leur forteresse.
   - Le constructeur incrémente `SAVE_VERSION`. Un ancien `world.bin` est refusé avec le message clair déjà en place.
   - Le README et les étapes de déploiement doivent dire comment effacer le volume `moria-data`, puisque le service worldgen ne regénère pas un monde existant.
   - Il n'est pas nécessaire de prévoir un chargeur v1, ni de déplacer les coordonnées d'anciennes données.
2. **[Décidé] Le dehors se travaille exactement comme l'intérieur.**
   - On peut creuser les collines, le sol extérieur et le sous-sol.
   - On peut construire des murs et des meubles, et faire des escaliers, sur l'herbe ou le sol creusé.
   - Seule l'eau est intouchable : on ne la creuse pas, on ne bâtit pas dessus, et elle ne coule pas.

### Tranchées par Claude (choix d'un constructeur raisonnable, réversibles)
3. **Pentes :**
   - Une pente est une **rampe à la Dwarf Fortress** : une seule case qui relie son étage à l'étage du dessus. On y monte depuis n'importe quel côté vers la case haute adjacente, et on en redescend de la même façon. Ce n'est pas une paire d'escaliers.
   - Une pente se creuse comme le sol (elle devient un sol plat), en suivant la règle « le dehors comme l'intérieur ».
   - La recherche de chemin et le service des nains doivent la comprendre, pour respecter l'anti-objectif « nain coincé ou détour absurde ».
4. **Ciel et chute :**
   - Le « ciel » est du vide **infranchissable** : on ne marche pas dans le vide et on ne tombe pas du bord d'une colline.
   - On ne creuse pas vers le ciel, puisqu'il n'y a rien à creuser.
   - La montagne est de la roche pleine jusqu'à son sommet, aux étages au-dessus du sol. Ailleurs, ces étages sont du ciel, sauf là où une colline monte.
5. **Étages :**
   - Le constructeur choisit la représentation interne (par exemple sol à `z = 4`). Puisque le monde est regénéré, rien n'oblige à garder l'ancienne numérotation.
   - Le client affiche des étages lisibles, avec le sol en 0, les étages du dessus en positif et le dessous en négatif ou l'équivalent.
6. **Protocole et ordre de fusion :**
   - La PR #13 (meubles en pierre, encore en brouillon) doit être **terminée et fusionnée avant** ce chantier. C'est la première étape du plan.
   - Elle passe `PROTOCOL_VERSION` à 3. Ce chantier le passe à 4, en étendant le message de construction typé de la PR #13 avec le matériau.
   - Les anciens clients sont refusés poliment, comme prévu.
7. **Charge :** le constructeur a la licence de changer le modèle des vues envoyées au service des nains si la taille l'exige : vue réduite à la zone utile, cache, envoi par différences. Les contraintes restent les mêmes :
   - le serveur fait foi et revalide chaque décision ;
   - les validations de taille restent en place ;
   - le test de charge à 10 joueurs reste vert (p99 < 150 ms).
8. **Point de départ :** le héros et les 3 nains apparaissent comme aujourd'hui, dans la salle d'entrée de la montagne, avec des coordonnées décalées pour la nouvelle carte.
9. **Pas de porte ni de mur d'entrée** imposés. Il n'y a ni créatures ni météo dehors pour l'instant.
