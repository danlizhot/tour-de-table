# Tour de table — notes pour Claude

Transcription locale de rencontres en français québécois : un exécutable Rust (egui + whisper.cpp)
et un programme auxiliaire de rédaction (llama.cpp). Voir `README.md` pour le détail fonctionnel.

## Principe directeur

L'application vise des organisations qui doivent la **déployer et la prendre en charge
facilement**. On n'ajoute un paramètre que s'il est impossible de décider automatiquement
(détecter, mesurer, choisir une valeur sûre). **Jamais de choix de modèle** proposé à
l'utilisateur pour une nouvelle fonctionnalité : l'application décide.

## Architecture

Fils d'exécution, du son au texte :

```
audio (recorder.rs, audio/)  capture → 16 kHz → détection de voix → énoncés
        │ crossbeam (Utterance)
whisper (stt.rs)             modèle gardé en mémoire, regroupement adaptatif
        │ crossbeam (Reply)
interface (ui/)              affichage seulement ; `pump()` vide les canaux à chaque image
```

* **Aucun travail lourd dans le fil de l'interface** : décodage d'un import, téléchargement,
  rédaction du compte rendu, lecture de fichiers volumineux… tout part dans un fil
  (`std::thread::spawn`) qui répond par un canal `crossbeam_channel`, lu sans bloquer
  (`try_recv`) dans `TourDeTableApp::pump`. L'avancement passe par des atomiques.
* `src/ui/` : `mod.rs` (état `TourDeTableApp`, boucle `eframe::App`, `pump`, barre du haut et
  historique), `live.rs` (rencontre en cours, import, contrôles), `past.rs` (fiche d'une
  rencontre, compte rendu), `agenda.rs` (ordre du jour), `verification.rs` (frontières d'un
  import à vérifier), `settings.rs` (Paramètres),
  `widgets.rs` (couleurs, composants, logo). Garder chaque fichier sous ~700 lignes.
* `storage.rs` : une rencontre = un dossier de Musique/Tour de table (nom fixé à la création,
  champ `dossier`) avec `rencontre.json`, sauvegardé après chaque
  segment (écriture atomique `.tmp` + `rename`).
* `summary.rs` : compte rendu par le programme auxiliaire.
* `nettoyage.rs` : nettoyage de la transcription, par règles seulement (`data/nettoyage.txt`) ;
  une passe par le modèle a été retirée, trop lente sur processeur seul (plus que le temps
  réel avec un 8B). Enchaînement : transcription → nettoyage →
  compte rendu. `Meeting::segments` (brut) n'est jamais modifié ; résultat dans
  `Meeting::nettoyage`. File d'attente et exclusion avec l'enregistrement dans `ui/nettoyage.rs`.
* `quebec.rs` : invite québécoise, typographie OQLF, lexique, filtre d'hallucinations.

### Crate auxiliaire `tourdetable-redaction` (`redaction/`)

whisper.cpp et llama.cpp embarquent chacun leur propre copie de **ggml** : réunis dans un même
exécutable, leurs symboles entrent en conflit. La rédaction tourne donc dans un exécutable
séparé, livré à côté de `tourdetable`, qui reçoit une requête JSON par ligne sur stdin et répond
à chacune par des lignes JSON sur stdout (`{"progres": n}`, puis `{"texte": …}` ou
`{"erreur": …}`). Le modèle reste chargé entre les requêtes : `summary::Session` garde le
programme ouvert pour toute une série (nettoyage, compte rendu par point).

Modèle de rédaction : Qwen3.5 4B sur tous les postes (`models::modele_redaction`),
jamais choisi par l'utilisateur. Avec 8 Go de mémoire vive (`models::palier`), Whisper est
déchargé (`stt::Request::Unload`) pendant la rédaction.

**Ne JAMAIS ajouter `llama-cpp-2` (ni rien qui lie llama.cpp/ggml) aux dépendances du crate
principal.** Toute fonction qui a besoin d'un modèle de langue passe par ce programme.

## Commandes

```
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release --workspace
```

La CI (`.github/workflows/build.yml`) lance `cargo test --workspace` et
`cargo build --release --workspace` sur Windows, macOS (`--features metal,tourdetable-redaction/metal`)
et Linux, puis vérifie qu'aucune DLL du runtime Visual C++ n'est importée sous Windows.
Prérequis Linux : `libasound2-dev cmake clang`.

Le code n'a jamais été passé par `rustfmt` (lignes d'environ 130 colonnes, sans
`rustfmt.toml`). Ne pas reformater des fichiers non touchés dans un commit fonctionnel.

## Conventions

* **Tout en français** : code (noms quand c'est naturel), commentaires, messages d'erreur,
  journal, interface, messages de commit.
* **Typographie OQLF** dans l'interface et les textes : « 14 h 30 », « 2 500 $ », pas d'espace
  avant ? ! ;, espace avant le deux-points, guillemets « » avec espaces.
* **Aucune donnée ne quitte l'ordinateur.** Seule connexion permise : téléchargement d'un
  modèle sur clic explicite (Hugging Face). Pas de
  télémétrie. Toute nouvelle connexion exige de mettre à jour `CONFIDENTIALITE.md` (et la page
  publiée, voir README).
* **Compatibilité ascendante de `rencontre.json`** (et de la configuration) : tout nouveau champ
  de `Meeting`, `Agenda` ou d'une structure sérialisée porte `#[serde(default)]`, pour que les
  rencontres existantes se chargent toujours. Ne jamais renommer ni retirer un champ sérialisé.
* Les erreurs remontent à l'utilisateur par `notify(…, true)` avec un message clair en français,
  et sont journalisées (`log::error!`).
