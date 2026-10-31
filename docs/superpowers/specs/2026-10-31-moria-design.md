# Moria — Document de conception

*Date : 2026-10-31 · Auteur : Nicolas Lallier (avec Claude)*

Légende : **[Décidé]** = choix de Nicolas · **[Proposé]** = choix de Claude, accepté sans objection mais à reconfirmer au besoin · **[Au constructeur]** = laissé à celui qui construit.

---

## 1. Intention

Moria est un **grand projet personnel sur plusieurs années**. Nicolas veut prendre son temps pour le faire, et le plaisir de construire compte autant que le jeu fini.

C'est un jeu multijoueur **coopératif** sur un serveur privé, pour un petit groupe d'amis. Chacun incarne un héros et fonde sa propre forteresse naine en creusant une montagne. Avec le temps, le jeu combine :

- un **RPG** pour le héros (races et classes inspirées de D&D 5e) ;
- une **gestion de PNJ** à la Dwarf Fortress (des nains autonomes à qui l'on donne des ordres) ;
- du **minage et une chaîne de production** à la Factorio ;
- une **gestion de ville** à la Cities: Skylines ;
- une **gestion d'armée et de diplomatie** à la Romance of the Three Kingdoms ;
- un **monde ouvert** autour de la montagne.

Ce sont des inspirations de genre. Moria est un jeu original : il ne doit reprendre ni le nom, ni le contenu, ni les assets de ces jeux.

### Pourquoi cette architecture
Nicolas veut **développer chaque concept indépendamment des autres** : un service par concept, qu'on peut construire, tester et redéployer seul. L'architecture distribuée est un objectif en soi. Ce n'est pas une contrainte de performance, puisque 10 joueurs tiendraient sur un seul processus.

## 2. Objectifs, non-objectifs, anti-objectifs

**Objectifs**
- Jusqu'à **10 joueurs simultanés** sur un serveur privé. **[Décidé]**
- Monde **persistant et toujours vivant** : quand un joueur se déconnecte, **ses nains continuent de travailler**. **[Décidé]** La simulation tourne donc en permanence pour toutes les forteresses, connectées ou non.
- **Coopération** entre joueurs : alliances, entraide, commerce. **[Décidé]**
- **Guerres uniquement contre des factions PNJ.** **[Décidé]**
- Chaque concept est développable indépendamment. **[Décidé]**

**Non-objectifs**
- Pas de **PvP**. Un joueur ne peut jamais attaquer la forteresse d'un autre. **[Décidé]**
- Pas de montée en charge vers des centaines ou des milliers de joueurs.
- Pas de client navigateur ni mobile.
- Pas de graphismes finaux au départ.

**Anti-objectifs** (le jeu est raté même s'il « fonctionne »)
- **Le serveur lag.** **[Décidé]**
- **Les nains sont stupides.** **[Décidé]** Interprétation **[Proposé]**, à transformer en scénarios de test :
  - un nain reste bloqué ou tourne en rond dans un tunnel ;
  - un nain ne fait rien alors qu'une tâche accessible attend ;
  - un nain choisit une tâche lointaine alors qu'une tâche équivalente est à côté ;
  - plusieurs nains se ruent sur la même tâche ;
  - un nain s'emmure lui-même ou creuse le sol sous ses pieds au mauvais moment.

## 3. Expérience de jeu

### Structure du monde **[Décidé]**
- **Un seul monde partagé**, qui contient **une montagne par faction**. Chaque joueur est une faction et possède sa montagne. *Supposition à confirmer : les factions PNJ ont elles aussi leurs montagnes ou territoires.*
- Le monde contient aussi des **montagnes, villes, îles et continents libres**, réservés à l'expansion future.
- Conséquence **[Proposé]** : le modèle de coordonnées est hiérarchique dès le départ (monde → région/montagne → tuile + étage Z). Le jalon 1 n'utilise que l'intérieur des montagnes, mais le monde existe déjà comme conteneur. Chaque montagne est une unité de simulation indépendante, ce qui permettra plus tard de répartir les montagnes entre plusieurs containers.

### Vue
**Tuiles 2D vues du dessus, avec des étages (niveaux Z)**, dans l'esprit de Dwarf Fortress. Le joueur voit un étage à la fois et monte ou descend avec les touches. **[Décidé]** (maquette « C », choisie le 2026-10-31)

### Héros
- Création de personnage : **race + classe à partir du SRD 5.1 de D&D** (Creative Commons BY 4.0, avec attribution obligatoire). **[Décidé : se baser sur D&D]** **[Proposé : utiliser le SRD 5.1 pour rester dans les règles de la licence]**
- Le joueur **contrôle son héros directement** au clavier : déplacement, creusage. **[Décidé]**
- Le héros est aussi un **chef** : il donne des ordres aux nains PNJ. **[Décidé]**

### Nains PNJ
- Ils reçoivent des **désignations** du joueur (zones à creuser, et plus tard à construire ou produire) et les exécutent de façon autonome. **[Décidé : on donne des ordres]** **[Proposé : par désignation de zones, comme dans Dwarf Fortress]**

### Graphismes
- **Carrés de couleur** au départ. **[Décidé]**
- Plus tard, Nicolas dessinera les tuiles avec ChatGPT (génération d'images). **[Décidé]** Le rendu doit donc permettre de remplacer une couleur par une image sans toucher à la logique.

## 4. Architecture technique

- **Langage : Rust partout.** **[Décidé]**
- **Client : programme Rust avec Bevy**, téléchargé par chaque ami. **[Décidé]**
- **Serveurs dans des containers Docker**, dont on peut augmenter le nombre à volonté. **[Décidé]**

### Hébergement **[Décidé]**
- Le serveur tourne **sur le Docker local de Nicolas**, géré par **Portainer** sur la machine `192.168.2.10` (réseau domestique).
- **[Proposé]** Les amis se connectent par Internet vers ce serveur maison. Il faut donc ouvrir un seul port réseau pour le serveur central ; les services, la file de messages et la base de données restent internes au réseau Docker. Le déploiement se fait avec un fichier Docker Compose importable comme « stack » dans Portainer.
- **[Proposé]** Comme le héros est contrôlé en direct à travers Internet, le client prédit localement les déplacements du héros et le serveur corrige au besoin. Une version simple suffit au jalon 1 ; les nains, eux, n'ont pas besoin de prédiction.

### Composants
1. **Serveur central** **[Décidé]**. C'est lui qui fait foi : il détient l'état du monde (tuiles, étages, héros, nains, objets), fait avancer la simulation à un rythme fixe (« tick ») et gère les connexions des clients.
2. **Un microservice par concept** **[Décidé]** : PNJ, production, ville, armées, RPG, monde ouvert, etc. Chacun a son propre container et son propre cycle de développement.
3. **Fonctions simples** **[Décidé]** pour les traitements en lot et les événements rares : génération d'une montagne, sauvegardes, tour d'une faction PNJ, invasion saisonnière, etc.
4. **Bibliothèque Rust partagée** **[Proposé]** : types communs (tuiles, races, classes, règles D&D, messages) utilisés par tous les composants.

### Règles de communication **[Proposé]**
- **Rien de lent ne bloque le tick.** Le serveur central n'appelle jamais un service de façon synchrone dans la boucle de simulation, pour respecter l'anti-objectif « pas de lag ».
- Chaque service travaille **à son propre rythme**. Il lit l'état dont il a besoin, puis renvoie des **décisions** (par exemple « le nain 12 va creuser la tuile X ») que le serveur central valide et applique.
- Les échanges passent par une **file de messages**. Si un service tombe ou est redéployé, le jeu continue et seul ce concept se met en pause.
- Le mouvement image par image et les règles physiques de base (peut-on marcher ici ? la tuile est-elle creusée ?) restent dans le serveur central. Les services prennent les **décisions**, le serveur central les **exécute**.

## 5. Feuille de route

### Jalon 1 : « La première soirée » **[Décidé]**
Les amis se connectent, **créent leur héros et creusent leur forteresse avec des nains**.

Contenu **[Proposé, accepté]** :
- connexion du client Bevy au serveur central ;
- création du héros (race et classe du SRD, caractéristiques de base) ;
- une zone de montagne par joueur, vue du dessus avec des étages ;
- quelques nains PNJ par forteresse ;
- le héros se déplace et creuse ;
- le joueur désigne des zones à creuser, et les nains les creusent seuls ;
- la roche creusée donne du minerai (pierre, or, mithril…) ;
- persistance : on retrouve sa forteresse à la reconnexion ;
- **au moins un service séparé dès le départ** (le service PNJ qui décide quoi creuser) pour prouver l'architecture serveur central + services.

Hors jalon 1 : combat, factions, production, ville, armées, monde ouvert.

### Ordre suivant **[Proposé, accepté]**
1. Production et ateliers (transformer le minerai)
2. Besoins et humeur des nains
3. Combat RPG et créatures des profondeurs
4. Ville et habitants
5. Factions PNJ, armées et guerres
6. Monde ouvert hors de la montagne

Chaque étape fera l'objet de sa propre conception avant d'être construite.

## 6. Contraintes

- 10 joueurs maximum, en même temps.
- Rust, Bevy, Docker. Ce sont des choix fermes.
- Projet personnel sur plusieurs années : il faut privilégier la clarté et la facilité de reprise après des mois de pause, plutôt que les optimisations prématurées.
- Licence SRD 5.1 : attribution requise dans le jeu ou le dépôt.

## 7. Laissé au constructeur **[Au constructeur]**

- Protocole client-serveur (transport, format de sérialisation).
- Choix de la file de messages et du stockage persistant.
- Fréquence du tick du serveur central.
- Taille de la carte d'une forteresse et nombre d'étages au jalon 1.
- Algorithmes de recherche de chemin et d'attribution des tâches des nains, à condition de respecter les scénarios « nains stupides » du §2.
- Orchestration des containers (Docker Compose suffit probablement pour 10 joueurs).
- Organisation du dépôt (workspace Cargo, un crate par service, etc.).

## 8. Critères de réussite du jalon 1

- 10 clients connectés, 10 forteresses actives, aucun lag perceptible.
- Tous les scénarios « nains stupides » du §2 ont un test qui passe.
- Arrêter puis redémarrer le service PNJ ne fait pas planter le jeu : les nains s'arrêtent, puis reprennent leur travail.
- Après un redémarrage du serveur, chaque forteresse est retrouvée telle quelle.

## 9. Choix tranchés après relecture **[Proposé]**

Ces points ont été soulevés par la relecture et tranchés par Claude. Ce sont des choix raisonnables, que Nicolas peut renverser.

1. **Mise à l'échelle :** le **serveur central est une seule instance** au jalon 1, ce qui suffit pour 10 joueurs. Ce sont les **services de concept** qu'on multiplie. Comme chaque montagne est une unité indépendante (§3), on pourra plus tard répartir les montagnes sur plusieurs serveurs centraux sans refaire le modèle.
2. **« Fonctions simples » :** ce sont des **containers Docker éphémères**, lancés pour une tâche puis arrêtés. Elles ne dépendent pas d'une plateforme FaaS ou cloud, pour rester 100 % sur le Docker local. Au jalon 1, la génération du monde et des montagnes en est la première.
3. **Propriété des données :** le serveur central possède **l'état physique du monde** (tuiles, positions, objets, héros, nains). Chaque service possède **les données propres à son concept** dans son propre espace de stockage, par exemple l'humeur des nains ou les relations diplomatiques. Les échanges passent uniquement par messages.
4. **Minerai au jalon 1 :** le minerai creusé va directement dans un **compteur de ressources de la forteresse**. Le transport d'objets par les nains (déplacer le minerai, entrepôts) arrive avec le jalon « Production et ateliers ».
5. **Identité des joueurs :** un **pseudo + mot de passe** par joueur, stocké sur le serveur. Pas de connexion externe.
6. **Systèmes d'exploitation du client :** builds **macOS, Windows et Linux**. Le client et le serveur vérifient leurs **numéros de version** à la connexion et refusent poliment si ça ne correspond pas. Pas de mise à jour automatique au jalon 1.
7. **Race, classe et caractéristiques au jalon 1 :** elles sont **enregistrées et affichées**, avec au plus un petit effet (par exemple la Force sur la vitesse de creusage). Les règles SRD complètes viendront avec le jalon « Combat RPG ».
