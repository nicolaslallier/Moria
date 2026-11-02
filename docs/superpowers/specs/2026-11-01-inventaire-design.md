# Moria : le conteneur d'inventaire, document de conception

*Date : 2026-11-01 · Auteur : Nicolas Lallier (avec Claude) · Specs liées : `2026-10-31-moria-design.md`. Projet suivant : le conteneur « nourriture » (vaches, champs), qui dépend de celui-ci.*

**Légende :**
- **[Décidé]** : choix de Nicolas.
- **[Proposé]** : choix de Claude, à confirmer par Nicolas à la relecture.
- **[Au constructeur]** : laissé à celui qui construit.

---

## 1. Intention

La nourriture (vaches, champs, bientôt plus) va devenir grosse. Nicolas veut qu'elle vive dans **son propre conteneur**, branché sur le **bus d'événements de l'Infra**, comme `dwarf-ai`. Cette nourriture arrive dans « l'inventaire principal », c'est-à-dire les `Resources` de chaque forteresse. Pour que le conteneur nourriture n'ait pas à écrire dans le serveur, **l'inventaire devient lui aussi un conteneur**.

Ce document couvre **l'inventaire seulement** (projet 1 sur 2) et le passage du bus sur RabbitMQ qu'il impose. Le conteneur nourriture aura son propre document.

## 2. Objectifs, non-objectifs, anti-objectifs

**Objectifs**
- **[Décidé]** Un conteneur `moria-inventory` possède **toutes** les ressources (pierre, or, mithril, bois, minerai de fer, charbon, fer, portes de fer, nourriture), par forteresse.
- **[Décidé]** Il se branche sur le **RabbitMQ de l'Infra**. Le serveur et `dwarf-ai` quittent le NATS privé de Moria pour le même RabbitMQ.
- **[Décidé]** Le serveur garde un **cache** de l'inventaire. Si l'inventaire est arrêté, **le jeu continue** sur ce cache.

**Non-objectifs**
- Le conteneur nourriture (projet 2).
- Changer le protocole client/serveur (reste à la version 10) ou le client : le client voit les mêmes `Resources` qu'avant.
- Plusieurs serveurs de jeu en parallèle, ou des objets individuels (outils, inventaire par nain) : l'inventaire reste un compteur par ressource.

**Anti-objectifs (un échec même si « ça marche »)**
- Une construction ou une fouille bloquée parce que le bus ou l'inventaire est arrêté.
- Un stock perdu parce que l'inventaire a redémarré.
- Un lot de changements appliqué deux fois après une reconnexion.

## 3. Le bus : RabbitMQ à la place de NATS

- **[Décidé]** Bus = le RabbitMQ de l'Infra (`rabbitmq:5672` sur `infra-net`). Le NATS privé du stack Moria est supprimé quand les trois binaires sont passés.
- **Constat :** l'Infra n'a pas de NATS. Aujourd'hui le pont (`bridge.rs`) et `dwarf-ai` ne font que du publier/s'abonner, sans requête/réponse, avec `postcard` comme format. La migration est donc un changement de transport, pas de logique.
- **[Proposé]** Un exchange `moria` de type *topic*, durable, dans un **vhost `moria`** avec un **utilisateur dédié** (pas le compte par défaut de l'Infra). Clés de routage :

| Clé | De → vers | Contenu | Fiabilité |
|---|---|---|---|
| `ai.view` | serveur → dwarf-ai | `AiView` | perdable ; file à longueur 1 qui jette le plus ancien |
| `ai.assign` | dwarf-ai → serveur | `Assignments` | perdable |
| `inventory.apply` | serveur ou nourriture → inventaire | lot de changements (§5) | durable, avec confirmation d'éditeur |
| `inventory.state` | inventaire → tous | totaux par forteresse (§5) | perdable, republié chaque seconde |

- **[Proposé]** Une petite crate `moria-bus` (autour de `lapin`) donne aux trois binaires la même chose : connexion avec reprise toutes les 2 s, publier, consommer. Elle évite de copier trois fois la boucle de reconnexion de `bridge.rs` et `dwarf-ai`.
- **[Proposé]** `dwarf-ai` consomme `ai.view` dans une file partagée (comme son `queue_subscribe` actuel) : plusieurs instances se partageraient le travail.
- **[Au constructeur]** Les arguments de file (durée de vie, longueur max), le nom de la gauge de métrique. `moria_nats_connected` est lue par le tableau Grafana de l'Infra : la renommer demande une PR Infra.

## 4. Ce que possède l'inventaire

- **[Décidé]** Pour chaque `MountainId`, un `Resources` complet. Rien d'autre.
- **[Proposé]** La logique d'ajout, de retrait et de plancher à zéro **reste dans `moria-core`** (`Resources::add`, `take`, `covers`). Le serveur et l'inventaire appellent le même code, donc ne peuvent pas diverger.
- **[Proposé]** Sauvegarde dans son propre fichier (`/data/inventory.bin`, volume propre), écrit comme `world.bin` : fichier temporaire puis renommage, avec un numéro de version en tête. Le fichier contient **dans le même écrit** les totaux, la table des derniers numéros de lot par émetteur (§5) et l'indicateur « graine appliquée » de chaque montagne (§7). Il pèse quelques centaines d'octets : on l'écrit **après chaque lot appliqué**, et à l'arrêt (SIGTERM). Garantie visée : *au moins une fois avec déduplication*, jamais de perte ni de double application, même si l'inventaire est tué en plein lot.

## 5. Le protocole d'inventaire

**Lot de changements (`inventory.apply`)**
- **[Proposé]** Un lot = `{ émetteur, époque, numéro, montagne, ajouts: Resources, retraits: Resources, graine: bool }`. Le serveur envoie **un lot par montagne et par tick** ; le numéro est **par émetteur**, pas par montagne.
- **[Proposé]** `époque` = l'heure Unix en millisecondes au démarrage de l'émetteur. Un émetteur qui redémarre repart donc à 1 sans que l'inventaire prenne ses lots pour des doublons, et il n'a **pas** besoin d'attendre le premier état pour envoyer.
- **[Décidé]** Un lot qui ferait passer une ressource sous zéro est appliqué quand même, **ramené à zéro**, et la construction reste (`Resources::take` plafonne déjà à zéro).
- **[Proposé]** L'inventaire retient `(époque, dernier numéro appliqué)` **par émetteur** (serveur, nourriture). Un lot d'une époque plus ancienne ou d'un numéro déjà vu est **ignoré** ; une époque plus récente remet le compteur à zéro. C'est ce qui protège l'anti-objectif « appliqué deux fois ». La table est sauvegardée avec les totaux (§4).
- **[Proposé]** L'inventaire est **une seule tâche** : appliquer le lot, sauvegarder, puis seulement publier l'état. Un état publié ne contient donc jamais des totaux en avance sur le disque, ni un accusé en retard sur les totaux : totaux et accusés viennent du même instant.
- **[Décidé]** Le serveur envoie **un lot par tick** (somme des gains et coûts de ce tick), pas un message par action de nain.

**État (`inventory.state`)**
- **[Proposé]** L'inventaire publie **chaque seconde**, et aussi juste après chaque lot appliqué, les totaux de toutes les forteresses, avec, pour chaque émetteur, `(époque, dernier numéro appliqué)`. Ce message est à la fois le *heartbeat*, le *snapshot* de démarrage et l'accusé de réception. Il pèse quelques dizaines d'octets, donc pas besoin de requête de resynchronisation. Un serveur qui démarre attend au plus une seconde.
- **[Proposé]** Si le serveur ne reçoit plus d'état depuis 5 s, il considère l'inventaire « arrêté ».

## 6. Le cache du serveur

- **[Décidé]** Le serveur lit toujours son cache pour le menu de construction, `covers`, et la vue de l'IA. Aucune lecture réseau dans la boucle de simulation.
- **[Proposé]** Cache = **dernier état reçu de l'inventaire + les lots envoyés mais pas encore accusés**, rejoués par-dessus. Un état reçu fait oublier tous les lots de la même époque dont le numéro est ≤ au numéro accusé ; comme totaux et accusé sont du même instant (§5), un lot n'est jamais compté deux fois.
- **[Décidé]** Inventaire arrêté : fouille, coupe, construction et repas **continuent** contre le cache. Les lots s'accumulent en attente.
- **[Décidé]** Retour de l'inventaire : le serveur rejoue les lots en attente, dans l'ordre, puis adopte les totaux publiés.
- **[Proposé]** Le serveur ne signale rien au joueur quand l'inventaire est arrêté. Seule la métrique change. (La bannière « nourriture indisponible » du projet 2 est un autre cas.)
- **[Proposé]** La simulation **n'attend jamais** le bus : elle dépose ses lots dans un canal, et une tâche à part les publie et attend les confirmations d'éditeur.
- **[Proposé]** Les lots sont publiés de façon durable : la file du courtier garde ceux envoyés pendant la panne. Le serveur ne « rejoue » donc que les lots dont la publication a échoué (courtier arrêté). Cette liste est **bornée à 100 000 lots, en jetant les plus anciens avec une erreur dans les logs** (fusionner des lots déjà publiés sous un nouveau numéro pourrait les compter deux fois).
- **[Proposé, limite connue]** La liste d'attente est en mémoire. Si le serveur redémarre pendant que l'inventaire est arrêté, les changements de cette panne sont perdus. À durcir plus tard si ça arrive en vrai.
- **[Proposé]** Le seuil de 5 s (§5) ne pilote que la métrique : le jeu ne change pas de comportement.

## 7. Migration et déploiement

- **[Proposé]** `world.bin` **ne change pas de forme** (pas de `SAVE_VERSION` 6, pas de lecteur v5) : `Fortress.resources` reste sauvegardé comme dernière valeur du cache. À chaque démarrage, le serveur envoie un lot `graine` tiré des ressources chargées, pour chaque montagne, **tant que l'état de l'inventaire ne la déclare pas `seeded`**. L'inventaire applique une graine **une seule fois** par montagne (indicateur sauvegardé, §4), même si des lots ordinaires sont arrivés avant : ils ne font qu'ajouter. Les montagnes réclamées plus tard sont semées par la même règle.
- **[Proposé]** `moria-inventory` est une image construite par `server.yml` et épinglée dans `docker-compose.yml` comme `dwarf-ai`, avec le volume `moria-inventory` et le réseau `infra-net`.
- **[Décidé]** L'Infra est modifiée : le vhost `moria` et son utilisateur doivent exister avant le déploiement de Moria. C'est une PR dans le dépôt Infra, à fusionner **en premier**. L'identifiant de l'utilisateur arrive dans le stack Moria par une variable (comme `MORIA_INVITE`).
- **[Proposé]** Les applications **déclarent elles-mêmes** l'exchange et les files ; l'utilisateur Moria a donc les droits *configure/write/read* sur le vhost `moria`, et seulement celui-là. `ai.assign` est consommé par une file **exclusive du serveur** (un seul consommateur) ; `ai.view` par une file partagée.
- **[Proposé]** La gauge garde le nom `moria_nats_connected` pour ne pas casser le tableau Grafana de l'Infra ; son renommage est une PR Infra séparée, plus tard.
- **[Au constructeur]** Où créer le vhost et l'utilisateur (un fichier de définitions RabbitMQ, ou un script d'amorçage), et où ranger le mot de passe.

## 8. Tests

- **[Décidé]** Chaque tâche suit RED puis GREEN (règle du projet).
- **[Proposé]** La logique se teste **sans RabbitMQ** : application d'un lot et plafond à zéro, rejet d'un lot déjà vu, sauvegarde et relecture, cache = état + lots en attente, purge des lots accusés, migration d'un monde v5. Un test avec un vrai RabbitMQ n'est lancé que si une variable d'environnement le demande.

## 9. Ce qui reste hors de ce document

- Le conteneur nourriture : il publiera dans `inventory.apply` avec son propre émetteur. Son arrêt gèle les vaches et les champs et met la faim en pause (décidé, hors de ce document).
- Un éventuel rappel dans le client quand l'inventaire est arrêté.
