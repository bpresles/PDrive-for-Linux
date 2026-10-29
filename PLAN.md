# PDrive for Linux — plan des prochaines évolutions

Document de reprise : état du projet au 28/10/2026 et évolutions prévues.

## État actuel

Client tiers non officiel pour Proton Drive, intégré à Nautilus (GNOME Fichiers). Projet personnel, non commercial, destiné à être publié sous GPL-3.0-or-later.

- `crates/pdsdk` : liaisons Rust avec le SDK officiel de Proton (C#, compilé en NativeAOT, appelé via son interface C et Protobuf).
- `crates/pdcore` : connexion par navigateur, session dans le trousseau (Secret Service), clés du compte, hôte HTTP du SDK.
- `crates/pdrive` : outil en ligne de commande (`login`, `logout`, `ls`, `selftest`).
- `crates/pdrive-app` : application de bureau (icône de notification, fenêtre de statut, réglages), qui lance et surveille `pdrived`.
- `crates/pdi18n` : traductions gettext partagées (catalogues dans `po/`).
- `crates/pdrived` : daemon de montage FUSE (`~/ProtonDrive`, lien vers le montage réel `~/.local/share/pdrive/.mount`), avec :
  - téléchargement à la demande et lecture progressive ;
  - écriture, avec envoi à la fermeture et révisions ;
  - événements Drive rejoués pour la mise à jour en direct ;
  - miniatures Proton, cache limité, reprise des envois interrompus ;
  - service D-Bus (état, compte, quota, activité récente) et compte CloudProviders.
- `extension/` : extension Nautilus en C (emblèmes d'état, « Keep Available Offline », « Free Up Space », rechargement au montage).
- `scripts/` : compilation des bibliothèques natives, installation utilisateur (`install.sh`) et système (`install-system.sh`).

Contraintes à respecter dans toutes les évolutions (conditions de Proton pour les applications tierces) :

- pas de logo Proton, ni de présentation laissant croire à un client officiel ;
- mention « application tierce non officielle » à la connexion ;
- en-tête `x-pm-appversion` honnête (`external-drive-pdrive_for_linux@<version>-<canal>`) ;
- synchronisation par événements, sans interrogation répétée de l'arborescence ;
- usage du SDK officiel : la migration cryptographique de Proton est annoncée pour fin 2026 / début 2027, il faudra suivre ses versions.

## 1. Préparer la publication sur GitHub

Dépôt : https://github.com/bpresles/PDrive-for-Linux (privé jusqu'à la fin de cette partie).

### Avant le passage en public

- [x] Réécrire l'auteur des commits (fait le 27/10/2026) : les premiers portaient une adresse professionnelle, alors que c'est un projet personnel. Les remplacer par `bpresles@gmail.com` avec `git filter-repo --mailmap`, puis forcer le push tant que le dépôt est privé. L'adresse personnelle est déjà configurée localement (`git config user.email`) pour les nouveaux commits.
- [ ] Éventuellement, renommer la branche `master` en `main`.

### Reste à faire (bilan du 28/10/2026)

- [ ] Captures d'écran pour le README dans `docs/screenshots/` : emblèmes, menu contextuel, barre latérale, fenêtre de statut, icône de notification (marqueurs TODO dans `README.md`).
- [x] Inventaire des licences (29/10/2026) : `scripts/audit-licenses.sh` (cargo-about, `project.assets.json` et `.nuspec`) génère `licenses/rust-pdrive.tsv`, `rust-proton-crypto.tsv` et `nuget-sdk.tsv` ; `THIRD_PARTY_NOTICES`, le tag `License:` du spec et `debian/copyright` complétés. Aucune licence incompatible avec la GPL-3.0 dans ce qui est lié. À noter : bzip2-1.0.6 (libbz2-rs-sys, permissive), notices tierces du runtime .NET désormais livrées dans l'archive native (`package-native.sh`), SonarAnalyzer (licence source-available, outil de compilation non distribué). Reste : livrer avec les paquets le recueil des textes de licence des crates (`scripts/audit-licenses.sh --texts <fichier>`, non branché dans `make-source-tarball.sh`) et la relecture humaine.
- [x] Mise en forme du code (29/10/2026) : `cargo fmt --all` fait dans un commit à part, avec `rustfmt.toml` (`max_width = 160`, `use_small_heuristics = "Max"` : environ 70 modifications au lieu de 450 avec les valeurs par défaut) ; `cargo fmt --check` est actif en CI.
- [x] Tests en CI (28-29/10/2026) : `.github/workflows/ci.yml` a maintenant un job `native` (bibliothèques natives, `actions/cache` indexé sur `native-ubuntu-24.04-dotnet10-<révisions figées>`, artefact `native-ci`) et un job `rust` (conteneur Debian 13, rustup stable, `cargo fmt --check`, `clippy -D warnings`, `cargo test --workspace`, test du pont `--ignored`). Conteneur Debian 13 et non le runner : `pdrive-app` exige libadwaita >= 1.6 (feature `v1_6`) et Ubuntu 24.04 n'a que la 1.5. Vérifié : syntaxe (`actionlint` 1.7.12, sans erreur) et les commandes du job `rust` exécutées telles quelles dans un conteneur `debian:trixie` (Rust 1.99.1, paquets apt `ca-certificates curl git build-essential pkg-config libglib2.0-dev libgtk-4-dev libadwaita-1-dev` suffisants). Non vérifié : l'exécution sur GitHub (job `native` et cache jamais lancés, pas de .NET en local).
- [x] Reporter le nouvel identifiant `io.github.bpresles.PDrive` dans `debian/`, `packaging/rpm/`, `.github/` et `packaging/README.md` (les seules mentions de l'ancien nom avertissent de l'ancien `.ini` de `/usr/local`).
- [x] Variable `PDSDK_RUNTIME_LIB_DIR` dans `crates/pdsdk/build.rs` (dossier des bibliothèques à l'exécution, distinct du dossier de lien) : elle fixe le rpath et le chemin de `libproton_crypto.so` donné au SDK. Corrige le RPM 0.1.0, où `pdrived` cherchait `libproton_crypto.so` dans le dossier de build (`patchelf` ne corrigeait que le rpath). `patchelf` n'est plus utilisé, et le spec et `debian/rules` vérifient que les binaires ne pointent plus vers le dossier de build.
- [ ] Simplifier les paquets : dans `extension/Makefile`, `$(LDFLAGS)` à l'édition de liens et `install -Dm0755` (contournés aujourd'hui dans le spec et `debian/rules`).
- [ ] Reconstruire et tester le paquet DEB avec cette correction.
- [ ] Pages de manuel pour `pdrive`, `pdrived` et `pdrive-app` (seuls avertissements de rpmlint et lintian).
- [ ] Vérifier que le fournisseur Secret Service de KDE (`kf6-kwallet`, `kwallet6`) implémente bien l'API Secret Service, sinon revoir les dépendances des paquets.
- [x] Tester le passage au nouvel identifiant après installation : quitter l'app, `install.sh`, `install-system.sh`, `nautilus -q`, relancer ; vérifier une seule entrée dans la barre latérale et un seul `pdrived`.

### README

- [x] Présentation, avec la mention bien visible « client tiers non officiel, non affilié à Proton AG ».
- [x] Fonctionnalités rédigées (README.md). Reste à faire : fournir les captures d'écran dans `docs/screenshots/` (marqueurs TODO dans le README : emblèmes, menu contextuel, barre latérale, fenêtre de statut, icône de notification).
- [x] Installation :
  - prérequis : Fedora/Debian, FUSE 3, .NET 10 SDK pour compiler le SDK, `nautilus-devel` pour l'extension ;
  - `install.sh`, `install-system.sh` ;
  - lancement de l'application, connexion, démarrage automatique ;
  - sous GNOME, activation de l'extension AppIndicator pour l'icône de notification.
- [x] Architecture, avec un schéma : SDK NativeAOT, `pdrived`, FUSE, D-Bus, extension, CloudProviders.
- [x] Limites connues : permissions et date seule non conservées, fichiers « Partagés avec moi » et photos non gérés, `O_NOATIME` refusé sur les fichiers non mis en cache.
- [x] Configuration (`~/.config/pdrive/config.toml`), journaux, désinstallation.

### Licences

- [x] Fichier `LICENSE` : GPL-3.0-or-later, texte complet.
- [x] Mentions des composants tiers sous MIT : SDK Proton Drive (`ProtonDriveApps/sdk`), `dotnet-crypto`, `proton-crypto-rs`. Fichier `THIRD_PARTY_NOTICES` écrit ; reste à auditer les paquets NuGet du SDK et les crates Rust de `dotnet-crypto` (mentions « TO COMPLETE » du fichier), et à le livrer avec les paquets.
- [x] En-têtes SPDX dans les sources `.rs`, `.c`, scripts shell et `Makefile` (les scripts de paquets de l'autre agent et les fichiers de `debian/`, `packaging/rpm/`, `.github/` n'en ont pas encore).

### Tests automatisés

- [x] Tests unitaires de l'arborescence (`tree.rs`) : listages, déplacements, éléments locaux, invalidations.
- [x] Journaux d'envoi et reprise (`writes.rs`) : (dé)sérialisation, nom de conflit, fichiers temporaires ignorés. La reprise est testée par ses parties pures (`load_journal`, `sweep_leftovers`) ; `recover_uploads` complet exigerait un faux client Drive.
- [x] Cache de contenu : purge LRU avec épingles, conservation du début des fichiers, adoption après envoi.
- [x] Miniatures : nom MD5 de l'URI, métadonnées `Thumb::URI` et `Thumb::MTime`.
- [x] Liaisons SDK : test du pont avec un hôte factice (l'équivalent de `pdrive selftest`), sans réseau : `crates/pdsdk/tests/bridge.rs`, ignoré par défaut (`cargo test -p pdsdk --test bridge -- --ignored`, bibliothèques de `native/linux-x64/`).
- [x] Aussi couverts : `activity`, `mountpoint`, `pins`, `config` du daemon, `pdi18n`, `config.toml` de `pdrive-app`, erreurs du SDK.
- [x] Tests d'intégration FUSE optionnels, sur un compte de test, hors CI par défaut : squelette `crates/pdrived/tests/fuse_integration.rs` (`#[ignore]`, `PDRIVE_FUSE_TEST=1`, session D-Bus dédiée). Reste à l'étoffer (reprise après arrêt, conflits) et à le valider sur un compte de test.
- [x] CI GitHub Actions : voir « Tests en CI » ci-dessus (`fmt --check`, `clippy -D warnings`, `cargo test`, test du pont, compilation de l'extension).

### Identifiant D-Bus définitif

- [x] Remplacer `io.github.pdrive.PDrive` par `io.github.bpresles.PDrive` (dépôt `bpresles/PDrive-for-Linux`). Fichiers concernés :
  - `crates/pdrived/src/dbus.rs` : nom de bus, chemins d'objets, interface `…PDrive1` ;
  - `extension/pdrive-nautilus.c` ;
  - `packaging/io.github.pdrive.PDrive.ini`, à renommer.
- [x] Prévoir la migration : supprimer l'ancien `.ini` de `/usr/local/share/cloud-providers/` à l'installation.
  - Fait aussi : `.desktop`, icône d'application, `pdrive-app` (renomme l'ancien fichier autostart au démarrage), `install.sh` (supprime l'ancien `.desktop` et l'ancienne icône), `po/` régénéré. Reste : reporter le nouvel identifiant dans `debian/`, `packaging/rpm/`, `.github/` et `packaging/README.md`, puis tester après installation (relancer l'app, `install-system.sh`, `nautilus -q`).

### Paquets RPM et DEB, workflow de release

Stratégie retenue : les bibliothèques natives sont précompilées une seule fois (CI ou `build-native.sh`, versions figées), publiées en archive avec leur somme SHA-256 (`scripts/package-native.sh`) et embarquées telles quelles dans les paquets (Source1 du SRPM, entrée du build DEB) ; les crates Rust sont vendorisées dans l'archive source (`scripts/make-source-tarball.sh`). Les paquets se construisent donc sans .NET ni réseau, mais ne visent pas les dépôts officiels Fedora ou Debian. Détails et limites : `packaging/README.md`.

- [ ] Étape de compilation des bibliothèques natives reproductible en CI : .NET 10 et NativeAOT, Rust, versions figées du SDK et de `dotnet-crypto`, comme dans `build-native.sh`. Job `native` écrit dans `release.yml`, non exécuté (pas de .NET en local) ; `package-native.sh` vérifié.
- [x] Installation système plutôt que dans `~/.local` (vérifiée sur le contenu des paquets RPM et DEB construits en conteneur) :
  - `/usr/bin/pdrive`, `/usr/bin/pdrived` ;
  - bibliothèques dans `/usr/lib64/pdrive/` (Fedora) ou `/usr/lib/<triplet>/pdrive/` (Debian), avec rpath correspondant ;
  - extension dans le dossier des extensions Nautilus ;
  - `.ini` dans `/usr/share/cloud-providers/` ;
  - icônes dans `/usr/share/icons/hicolor/` ;
  - `/usr/bin/pdrive-app` et son `.desktop` dans `/usr/share/applications/` (plus d'unité systemd, voir partie 2) ;
  - catalogues de traduction dans `/usr/share/locale/<langue>/LC_MESSAGES/pdrive.mo`.
- [x] RPM : `packaging/rpm/pdrive-for-linux.spec`, construit en conteneur Fedora 44 (`scripts/build-packages.sh rpm`), rpmlint sans erreur.
- [ ] RPM : publication via COPR. À faire : créer le projet COPR, renseigner la variable `COPR_PROJECT` et le secret `COPR_CONFIG` (job `copr` de `release.yml`, envoi du SRPM), tester une première construction ; auditer les licences des crates vendorisées et des paquets NuGet embarqués (`THIRD_PARTY_NOTICES`).
- [x] DEB : `debian/` (debhelper 13), construit en conteneur Debian 13 (rustc par rustup, les dépendances exigent Rust 1.92), lintian sans erreur. Non testé : installation sur Debian/Ubuntu, Ubuntu 24.04 (libadwaita 1.5 trop ancienne).
- [ ] Workflow GitHub Actions déclenché par un tag `v*` (`.github/workflows/release.yml`) : écrit, jamais exécuté. À faire : un premier tag de test sur un dépôt d'essai, vérification du job `native`.
- [ ] Version unique propagée partout : `scripts/check-version.sh` vérifie `Cargo.toml`, le spec, `debian/changelog` et le tag (utilisé par le workflow). Reste `x-pm-appversion`.

## 2. Compléter les fonctions

Réalisé le 28/10/2026 (commits « Desktop application with tray icon and status window », « Translations with gettext, French first », « Single sidebar entry… »). Testé : compilation, clippy, enregistrement de l'icône sous Hyprland/waybar, API D-Bus, montage caché et journaux. Reste à valider à l'usage : fenêtre de statut et réglages, connexion/déconnexion depuis l'app, rendu sous KDE et GNOME (extension AppIndicator), menus traduits dans Nautilus.

### Application de bureau et cycle de vie du daemon

Le service systemd utilisateur (`pdrive.service`) est remplacé par une application de bureau qui lance le daemon.

- [x] Nouvelle crate `crates/pdrive-app` (binaire `pdrive-app`) : application GTK4/libadwaita à instance unique (`gtk::Application`, identifiant `io.github.pdrive.PDrive.App`, renommé avec le reste en partie 1).
  - Au démarrage, elle lance `pdrived` comme processus enfant si une session existe et si le nom D-Bus du daemon n'est pas déjà pris, puis le surveille : redémarrage en cas d'échec avec délai croissant, comme `Restart=on-failure`.
  - Elle reste active sans fenêtre (`hold`) : fermer la fenêtre de statut ne quitte pas l'application.
  - Seule l'entrée « Quitter » du menu de l'icône la ferme : arrêt propre du daemon (SIGTERM, démontage, envois en attente repris au prochain démarrage), puis sortie.
  - Relancer l'application alors qu'elle tourne déjà affiche la fenêtre de statut.
- [x] Démarrage automatique avec la session, activé depuis les réglages : fichier XDG autostart `~/.config/autostart/io.github.pdrive.PDrive.App.desktop` (`Exec=pdrive-app --background`), reconnu par GNOME, KDE, XFCE, Cinnamon, MATE, LXQt et la plupart des compositeurs. Désactivé par défaut, proposé après la première connexion.
- [x] Fichier `.desktop` de l'application (menu des applications) et icône d'application.
- [x] Migration : `install.sh` désactive et supprime `pdrive.service` ; `pdrived` reste lançable seul pour le débogage.
- [x] Journaux : sortie du daemon relayée par l'application vers le journal (`journalctl --user -t pdrive` ou fichier dans `$XDG_STATE_HOME/pdrive/`).
- [x] Le daemon s'arrête si l'application meurt (`PR_SET_PDEATHSIG`).
- [x] Option « Ouvrir la fenêtre d'état au démarrage » (activée par défaut), pour un lancement depuis le menu ; `--background` (session) n'ouvre jamais la fenêtre.
- [x] Une seule entrée dans la barre latérale : `x-gvfs-hide` n'atteint jamais GIO (`fusermount3` n'écrit pas `/run/mount/utab`). Le FUSE est donc monté dans un chemin caché (`$XDG_DATA_HOME/pdrive/.mount`, ignoré par GLib et gvfs) et `~/ProtonDrive` en devient un lien symbolique. Propriété D-Bus `MountTarget` pour le chemin réel.

### Traductions

- [x] Rendre traduisibles les textes du daemon (menu de la barre latérale, état, infobulle), de l'application (icône, menu, fenêtre de statut, réglages) et de l'extension (menu contextuel).
- [x] gettext : `gettext-rs` côté Rust, gettext classique côté C. Domaine `pdrive`, catalogues `po/`.
- [x] Première langue : le français (`po/fr.po`, mise à jour avec `scripts/update-po.sh`).

### Icône dans la barre de notification et fenêtre de statut

Objectif : une icône de dossier violette dans la barre de notification, sous GNOME comme sous KDE et les autres environnements. Un clic ouvre une fenêtre de statut comparable au client officiel macOS, **sans le bouton « Add storage »**. Un clic droit ouvre un menu : « Ouvrir le dossier », « Afficher la fenêtre de statut », « Monter / Démonter », « Réglages », « Quitter ».

Contenu de la fenêtre :

- [x] **En-tête :** avatar (initiales), adresse du compte, espace utilisé (« Using X GB of Y GB »), bouton de réglages.
  - Quota : `GET core/v4/users`, champs `UsedSpace`/`MaxSpace` du compte (total du compte, pas la seule part Drive).
- [x] **Liste d'activité récente :** fichiers en cours d'envoi ou de téléchargement (« Syncing… » avec indicateur) et opérations terminées, avec heure relative et taille. Exemples : « Uploaded 2 min ago | 0,3 MB », « Renamed », « Moved to trash ».
  - Nécessite un journal d'activité dans `pdrived` : envois, téléchargements, renommages, suppressions, changements distants. Il serait exposé par D-Bus (méthode `GetRecentActivity` et signal `ActivityChanged`).
- [x] **Barre d'état :** « Syncing… », « Up to date », « Not mounted », « Not signed in », erreurs.
- [x] **Boutons :**
  - « Open folder » (ouvre `~/ProtonDrive`) ;
  - « View online » (ouvre `https://drive.proton.me`) ;
  - pas de « Add storage ».
- [x] **État de connexion :** si aucune session, la fenêtre propose « Se connecter », qui lance la connexion par navigateur, l'équivalent de `pdrive login`, avec la mention « application tierce non officielle ». Après connexion, le service démarre ou monte automatiquement.
- [x] **Réglages :** point de montage, taille du cache, déconnexion, démarrage automatique, ouverture de la fenêtre au démarrage.

Choix techniques (tranchés le 28/10/2026) :

- **Icône dans la barre : StatusNotifierItem** (protocole `org.kde.StatusNotifierItem` et menu `com.canonical.dbusmenu`), avec la crate `ksni` dans `pdrive-app`.
  - C'est le seul mécanisme commun : natif sous KDE Plasma, LXQt, XFCE, Cinnamon, MATE, Budgie, et sous les compositeurs avec waybar ou équivalent (Hyprland, Sway). Sous GNOME, il passe par l'extension « AppIndicator and KStatusNotifierItem Support », installée par défaut sur Ubuntu et empaquetée par Fedora (`gnome-shell-extension-appindicator`). Elle sera mise en dépendance recommandée des paquets.
  - L'ancien protocole XEmbed n'est pas retenu : il ne fonctionne pas sous Wayland.
  - Si aucun `org.kde.StatusNotifierWatcher` n'est présent au démarrage, l'application ouvre la fenêtre de statut, qui affiche un bandeau expliquant comment activer l'extension sous GNOME. `ksni` s'enregistre de lui-même dès qu'un watcher apparaît.
  - Une extension GNOME Shell dédiée est écartée pour l'instant : elle ne servirait qu'à GNOME et serait à maintenir à chaque version (voir le backlog).
- **Fenêtre de statut et réglages :** dans `pdrive-app` (`gtk4-rs`, `libadwaita-rs`), avec les données du daemon lues par D-Bus. L'entrée « Proton Drive » de la barre latérale gagne une action « Afficher l'état » qui active l'application.
- **Interface D-Bus du daemon** (`io.github.pdrive.PDrive1`), à compléter :
  - propriétés `State` (`syncing`, `idle`, `unmounted`, `error`), `Account` (adresse), `UsedSpace` et `MaxSpace` (octets), avec `PropertiesChanged` ;
  - méthodes `GetRecentActivity` (liste d'entrées : chemin, type, taille, horodatage, état) et `Mount` / `Unmount` ;
  - signal `ActivityChanged`.
- **Connexion :** sans session, l'application ne lance pas le daemon et sa fenêtre propose « Se connecter », qui exécute `pdrive login` (mention « application tierce non officielle » affichée dans la fenêtre), puis lance le daemon. « Se déconnecter » arrête le daemon puis exécute `pdrive logout`.
- **Icône :** dossier violet, à dessiner en SVG sans reprendre le logo Proton. Variantes : synchronisé, en cours de synchronisation, erreur, déconnecté. Elles sont installées dans le thème `hicolor` et transmises par nom (`IconName`) ; pas de repli en pixmap pour l'instant.

## 3. Autres pistes (backlog)

- [ ] Fichiers « Partagés avec moi » (SDK : `EnumerateSharedWithMeNodeUids`), sous un sous-dossier dédié.
- [ ] Photos (client Photos du SDK).
- [ ] Miniatures envoyées avec les nouveaux fichiers (le SDK accepte des miniatures à l'envoi).
- [ ] Corbeille de Nautilus sur le montage (aujourd'hui, la suppression passe directement dans la corbeille Proton).
- [ ] Notification de bureau si la session expire ou si un envoi échoue définitivement.
- [ ] Extension GNOME Shell dédiée (optionnelle), si l'extension AppIndicator s'avère insuffisante.
- [ ] Mettre à jour régulièrement le SDK Proton, notamment pour la migration cryptographique annoncée.
