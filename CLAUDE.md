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
  rencontre, compte rendu), `onglets.rs` (onglets de la fiche : ordre du jour, transcription, compte rendu, un volet
  ou deux côte à côte), `agenda.rs` (ordre du jour), `verification.rs` (frontières à
  corriger, « Découper par points »), `settings.rs` (Paramètres), `installation.rs` (installation automatique des
  modèles au premier démarrage, plein écran),
  `widgets.rs` (couleurs, composants, logo). Garder chaque fichier sous ~700 lignes. Dans chaque onglet, les
  boutons sont au bas du volet, sous le texte qui défile (`onglets::pied`). `ecoute.rs` : écoute d'un paragraphe ou
  d'un point (`audio/lecture.rs`, lecture par morceaux de 20 s dans un fil), jamais pendant un enregistrement.
* `storage.rs` : une rencontre = un dossier de Musique/Tour de table (nom fixé à la création,
  champ `dossier`) avec `rencontre.json`, sauvegardé après chaque
  segment (écriture atomique `.tmp` + `rename`). « Archiver » comprime le dossier dans `Archives/<dossier>.zip`
  (`storage::archiver`, dans un fil ; écrit en `.part`, relu, puis le dossier est retiré) ; l'ancienne corbeille
  du dossier de données y passe au démarrage (`archiver_corbeille`).
* `summary.rs` : compte rendu par le programme auxiliaire. Par défaut, les sujets sont d'abord répartis entre les
  points avec la transcription complète (`repartition.rs`), puis chaque point est rédigé d'après ses sujets et ses
  passages de la transcription, `POSTES` points à la fois ; le mot de bienvenue, les adoptions et la fin gardent la
  transcription complète. Pour une rencontre d'une heure ou plus, la liste des sujets est demandée par tranches
  d'au plus 40 minutes, plusieurs à la fois (`repartition::tranches`). `TOURDETABLE_DECOUPE` revient à la transcription du point seulement. Le préfixe commun
  (ordre du jour + transcription) est gardé par le programme auxiliaire. Banc d'essai : 25/32 contre 24/32 avant
  les passages (12 novembre 2026), surtout moins de doublons.
* `nettoyage.rs` : nettoyage de la transcription, par règles seulement (`data/nettoyage.txt`, et `annees.rs` pour
  les années dites en deux nombres) ;
  une passe par le modèle a été retirée, trop lente sur processeur seul (plus que le temps
  réel avec un 8B). Enchaînement : transcription → nettoyage →
  compte rendu. `Meeting::segments` (brut) n'est jamais modifié ; résultat dans
  `Meeting::nettoyage`. File d'attente et exclusion avec l'enregistrement dans `ui/nettoyage.rs`.
* `voix.rs` : reconnaissance des voix (sherpa-onnx, onnxruntime lié statiquement) après la transcription, dans un
  processus à part (`tourdetable --voix <dossier>`, réponse JSON) qu'on peut tuer : la bibliothèque n'offre ni
  avancement ni arrêt. sherpa-onnx ne donne que les tours de parole ; les empreintes de morceaux de 4 s sont
  regroupées par `regroupement.rs` (liaison moyenne, seuil fixe 0,36 : le regroupement de sherpa-onnx basculait
  pour un arrondi de l'audio). Voix de moins de 30 s écartées ; deux voix de même nom n'en font qu'une. Environ un
  cinquième de la durée de la rencontre. `Meeting::voix` (tours de parole, noms) ; les paragraphes affichés et
  exportés sont coupés par voix (`storage::Decoupe::ParVoix`), mais le texte lu par le compte rendu ne change
  pas (`ParSource`) tant que l'effet des voix sur la rédaction n'est pas mesuré. File d'attente dans `ui/voix.rs`.
* `quebec.rs` : invite québécoise, typographie OQLF, lexique, filtre d'hallucinations.
* `gpu.rs` : puce graphique (Vulkan, Windows) choisie par étalonnage (`tourdetable --etalonner`, processus
  à part lancé par `ui/puce.rs` quand l'application est inoccupée) ; sentinelle autour des transcriptions
  sur la puce, repli de la rédaction sur le processeur après un plantage. Sans verdict : processeur, et
  `GGML_VK_VISIBLE_DEVICES` vide pour que ggml n'y touche pas. Le 35B-A3B ne rédige jamais sur une puce Vulkan
  (`LlmInfo::sur_puce_vulkan`) : beaucoup plus lent que le processeur sur une Iris Xe avec une longue transcription. Pas de *flash attention* avec Vulkan
  (charabia sur Intel). Un énoncé que Whisper fait tourner en boucle sur la puce (`stt::en_boucle`) est retranscrit sur le
  processeur : 16 min d'audio sur 68 bouclaient sur une Intel Iris Xe (13 novembre 2026).

### Crate auxiliaire `tourdetable-redaction` (`redaction/`)

whisper.cpp et llama.cpp embarquent chacun leur propre copie de **ggml** : réunis dans un même
exécutable, leurs symboles entrent en conflit. La rédaction tourne donc dans un exécutable
séparé, livré à côté de `tourdetable`, qui reçoit une requête JSON par ligne sur stdin et répond
à chacune par des lignes JSON sur stdout (`{"progres": n}`, puis `{"texte": …}` ou
`{"erreur": …}`). Une requête `invites` (plusieurs suites du même `prefixe`, `simultanees` à la fois) est rédigée
en un seul passage du modèle par jeton, toutes invites confondues, et répond `{"textes": […]}` ; `{"journal": …}`
donne les durées de lecture et d'écriture, recopiées dans le journal de l'application. Le modèle reste chargé entre les requêtes : `summary::Session` garde le
programme ouvert pour toute une série (nettoyage, compte rendu par point).

L'application est livrée sans modèle : `ui/installation.rs` les télécharge sans aucun clic
(aucune autre interface de téléchargement). Modèle de rédaction selon la mémoire vive (`models::modele_redaction`) : Qwen3.5 9B
jusqu'à 20 Go, Qwen3.6 35B-A3B (APEX I-Nano, 13 à 14 Go en rédaction) à partir de 24 Go ;
jamais choisi par l'utilisateur. **16 Go de mémoire vive minimum** : en dessous, un avertissement
s'affiche au démarrage (`models::memoire_insuffisante`). Un modèle remplacé va dans
`ANCIENS_MODELES_REDACTION` pour être supprimé des postes.

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
`cargo build --release --workspace`, puis vérifie qu'aucune DLL du runtime Visual C++ n'est importée
sous Windows. Une PR compile Windows seulement (`--features vulkan,tourdetable-redaction/vulkan`, SDK Vulkan
installé par la CI, `vulkan-1.dll` livré) ; Windows, macOS (`--features metal,tourdetable-redaction/metal`)
et Linux sont compilés pour une étiquette `v*` ou à la demande (« Run workflow »). Rien à la
fusion dans `main` : les minutes de CI sont limitées. Lancer donc les tests sur Linux en local.
Prérequis Linux : `libasound2-dev cmake clang`.

Le code n'a jamais été passé par `rustfmt` (lignes d'environ 130 colonnes, sans
`rustfmt.toml`). Ne pas reformater des fichiers non touchés dans un commit fonctionnel.

## Conventions

* **Tout en français** : code (noms quand c'est naturel), commentaires, messages d'erreur,
  journal, interface, messages de commit.
* **Typographie OQLF** dans l'interface et les textes : « 14 h 30 », « 2 500 $ », pas d'espace
  avant ? ! ;, espace avant le deux-points, guillemets « » avec espaces.
* **Aucune donnée ne quitte l'ordinateur.** Seule connexion permise : installation des modèles
  (Hugging Face), automatique au premier démarrage ou quand un modèle manque. Pas de
  télémétrie. Toute nouvelle connexion exige de mettre à jour `CONFIDENTIALITE.md` (et la page
  publiée, voir README).
* **Compatibilité ascendante de `rencontre.json`** (et de la configuration) : tout nouveau champ
  de `Meeting`, `Agenda` ou d'une structure sérialisée porte `#[serde(default)]`, pour que les
  rencontres existantes se chargent toujours. Ne jamais renommer ni retirer un champ sérialisé.
* Les erreurs remontent à l'utilisateur par `notify(…, true)` avec un message clair en français,
  et sont journalisées (`log::error!`).
