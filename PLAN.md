# PDrive for Linux — plan des prochaines évolutions

Document de reprise : état du projet au 27/10/2026 et évolutions prévues.

## État actuel

Client tiers non officiel pour Proton Drive, intégré à Nautilus (GNOME Fichiers). Projet personnel, non commercial, destiné à être publié sous GPL-3.0-or-later.

- `crates/pdsdk` : liaisons Rust avec le SDK officiel de Proton (C#, compilé en NativeAOT, appelé via son interface C et Protobuf).
- `crates/pdcore` : connexion par navigateur, session dans le trousseau (Secret Service), clés du compte, hôte HTTP du SDK.
- `crates/pdrive` : outil en ligne de commande (`login`, `logout`, `ls`, `selftest`).
- `crates/pdrived` : service de montage FUSE (`~/ProtonDrive`), avec :
  - téléchargement à la demande et lecture progressive ;
  - écriture, avec envoi à la fermeture et révisions ;
  - événements Drive rejoués pour la mise à jour en direct ;
  - miniatures Proton, cache limité, reprise des envois interrompus ;
  - service D-Bus et compte CloudProviders.
- `extension/` : extension Nautilus en C (emblèmes d'état, « Keep Available Offline », « Free Up Space », rechargement au montage).
- `scripts/` : compilation des bibliothèques natives, installation utilisateur (`install.sh`) et système (`install-system.sh`).

Contraintes à respecter dans toutes les évolutions (conditions de Proton pour les applications tierces) :

- pas de logo Proton, ni de présentation laissant croire à un client officiel ;
- mention « application tierce non officielle » à la connexion ;
- en-tête `x-pm-appversion` honnête (`external-drive-pdrive_for_linux@<version>-<canal>`) ;
- synchronisation par événements, sans interrogation répétée de l'arborescence ;
- usage du SDK officiel : la migration cryptographique de Proton est annoncée pour fin 2026 / début 2027, il faudra suivre ses versions.

## 1. Préparer la publication sur GitHub

### README

- [ ] Présentation, avec la mention bien visible « client tiers non officiel, non affilié à Proton AG ».
- [ ] Fonctionnalités et captures d'écran : emblèmes, menu contextuel, barre latérale, fenêtre de statut quand elle existera.
- [ ] Installation :
  - prérequis : Fedora/Debian, FUSE 3, .NET 10 SDK pour compiler le SDK, `nautilus-devel` pour l'extension ;
  - `install.sh`, `install-system.sh` ;
  - `pdrive login`, activation du service.
- [ ] Architecture, avec un schéma : SDK NativeAOT, `pdrived`, FUSE, D-Bus, extension, CloudProviders.
- [ ] Limites connues : permissions et date seule non conservées, fichiers « Partagés avec moi » et photos non gérés, `O_NOATIME` refusé sur les fichiers non mis en cache.
- [ ] Configuration (`~/.config/pdrive/config.toml`), journaux (`journalctl --user -u pdrive`), désinstallation.

### Licences

- [ ] Fichier `LICENSE` : GPL-3.0-or-later, texte complet.
- [ ] Mentions des composants tiers sous MIT : SDK Proton Drive (`ProtonDriveApps/sdk`), `dotnet-crypto`, `proton-crypto-rs`. Prévoir un fichier `THIRD_PARTY_NOTICES` livré avec les paquets.
- [ ] En-têtes SPDX dans les sources.

### Tests automatisés

- [ ] Tests unitaires de l'arborescence (`tree.rs`) : listages, déplacements, éléments locaux, invalidations.
- [ ] Journaux d'envoi et reprise (`writes.rs`) : (dé)sérialisation, nom de conflit, fichiers temporaires ignorés.
- [ ] Cache de contenu : purge LRU avec épingles, conservation du début des fichiers, adoption après envoi.
- [ ] Miniatures : nom MD5 de l'URI, métadonnées `Thumb::URI` et `Thumb::MTime`.
- [ ] Liaisons SDK : test du pont avec un hôte factice (l'équivalent de `pdrive selftest`), sans réseau.
- [ ] Tests d'intégration FUSE optionnels, sur un compte de test, hors CI par défaut.
- [ ] CI GitHub Actions : `cargo fmt --check`, `clippy -D warnings`, `cargo test`, compilation de l'extension.

### Identifiant D-Bus définitif

- [ ] Remplacer `io.github.pdrive.PDrive` par `io.github.<compte-github>.PDrive`. Fichiers concernés :
  - `crates/pdrived/src/dbus.rs` : nom de bus, chemins d'objets, interface `…PDrive1` ;
  - `extension/pdrive-nautilus.c` ;
  - `packaging/io.github.pdrive.PDrive.ini`, à renommer.
- [ ] Prévoir la migration : supprimer l'ancien `.ini` de `/usr/local/share/cloud-providers/` à l'installation.

### Paquets RPM et DEB, workflow de release

- [ ] Étape de compilation des bibliothèques natives reproductible en CI : .NET 10 et NativeAOT, Rust, versions figées du SDK et de `dotnet-crypto`, comme dans `build-native.sh`.
- [ ] Installation système plutôt que dans `~/.local` :
  - `/usr/bin/pdrive`, `/usr/bin/pdrived` ;
  - bibliothèques dans `/usr/lib64/pdrive/` (Fedora) ou `/usr/lib/<triplet>/pdrive/` (Debian), avec rpath correspondant ;
  - extension dans le dossier des extensions Nautilus ;
  - `.ini` dans `/usr/share/cloud-providers/` ;
  - icônes dans `/usr/share/icons/hicolor/` ;
  - unité systemd utilisateur dans `/usr/lib/systemd/user/`.
- [ ] RPM : fichier `.spec`, publication via COPR.
- [ ] DEB : `debian/` ou `cargo-deb`, avec dépendances `fuse3`, `libnautilus-extension4`, `gnome-keyring` ou fournisseur Secret Service.
- [ ] Workflow GitHub Actions déclenché par un tag `v*` : compilation, paquets, sommes de contrôle, release GitHub.
- [ ] Version unique propagée partout : `Cargo.toml`, `x-pm-appversion`, paquets.

## 2. Compléter les fonctions

### Traductions

- [ ] Rendre traduisibles les textes du service (menu de la barre latérale, état, infobulle) et de l'extension (menu contextuel).
- [ ] gettext : `gettext-rs` côté Rust, gettext classique côté C. Domaine `pdrive`, catalogues `po/`.
- [ ] Première langue : le français.

### Icône dans la barre de notification et fenêtre de statut

Objectif : une icône de dossier violette dans la barre du haut, comme sur macOS. Un clic ouvre une fenêtre de statut comparable au client officiel macOS, **sans le bouton « Add storage »**.

Contenu de la fenêtre :

- [ ] **En-tête :** avatar (initiales), adresse du compte, espace utilisé (« Using X GB of Y GB »), bouton de réglages.
  - Quota : `GET core/v4/users`, à vérifier : `UsedSpace`/`MaxSpace` ou équivalents Drive.
- [ ] **Liste d'activité récente :** fichiers en cours d'envoi ou de téléchargement (« Syncing… » avec indicateur) et opérations terminées, avec heure relative et taille. Exemples : « Uploaded 2 min ago | 0,3 MB », « Renamed », « Moved to trash ».
  - Nécessite un journal d'activité dans `pdrived` : envois, téléchargements, renommages, suppressions, changements distants. Il serait exposé par D-Bus (méthode `GetRecentActivity` et signal `ActivityChanged`).
- [ ] **Barre d'état :** « Syncing… », « Up to date », « Not mounted », « Not signed in », erreurs.
- [ ] **Boutons :**
  - « Open folder » (ouvre `~/ProtonDrive`) ;
  - « View online » (ouvre `https://drive.proton.me`) ;
  - pas de « Add storage ».
- [ ] **État de connexion :** si aucune session, la fenêtre propose « Se connecter », qui lance la connexion par navigateur, l'équivalent de `pdrive login`, avec la mention « application tierce non officielle ». Après connexion, le service démarre ou monte automatiquement.
- [ ] **Réglages :** point de montage, taille du cache, déconnexion, démarrage automatique.

Choix techniques à trancher :

- **Icône dans la barre :** GNOME Shell n'affiche pas les icônes de notification nativement. Options :
  1. StatusNotifierItem (protocole `org.kde.StatusNotifierItem`, crate `ksni`). Il exige l'extension GNOME « AppIndicator and KStatusNotifierItem Support », présente par défaut sur Ubuntu mais pas sur Fedora Workstation.
  2. Extension GNOME Shell dédiée (JavaScript), qui afficherait l'icône et un menu ou une fenêtre natifs, en parlant au service par D-Bus. C'est la meilleure intégration, mais elle est à maintenir selon les versions de GNOME Shell.
  3. Les deux : StatusNotifierItem par défaut, plus une extension Shell optionnelle.
- **Fenêtre de statut :** application GTK4/libadwaita en Rust (`gtk4-rs`, `libadwaita-rs`), lancée à la demande et pilotée par D-Bus. Elle pourrait aussi être déclenchée depuis le menu de l'entrée « Proton Drive » de la barre latérale.
- **Icône :** dossier violet, à dessiner sans reprendre le logo Proton. Variantes : synchronisé, en cours de synchronisation, erreur, déconnecté.

## 3. Autres pistes (backlog)

- [ ] Fichiers « Partagés avec moi » (SDK : `EnumerateSharedWithMeNodeUids`), sous un sous-dossier dédié.
- [ ] Photos (client Photos du SDK).
- [ ] Miniatures envoyées avec les nouveaux fichiers (le SDK accepte des miniatures à l'envoi).
- [ ] Corbeille de Nautilus sur le montage (aujourd'hui, la suppression passe directement dans la corbeille Proton).
- [ ] Notification de bureau si la session expire ou si un envoi échoue définitivement.
- [ ] Mettre à jour régulièrement le SDK Proton, notamment pour la migration cryptographique annoncée.
