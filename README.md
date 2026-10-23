<p align="center"><img src="assets/icon-256.png" width="128" alt="Logo de Tour de table"></p>

# Tour de table

> *Tout le monde parle, rien ne se perd.*

**Transcription de rencontres en français québécois, 100 % locale.**

Tour de table capte en même temps le **micro de la salle** et le **son des participants à distance**
(Teams, Zoom, Meet, Webex… tout ce qui sort de l'ordinateur), puis transcrit en français québécois
avec Whisper, directement sur votre machine. Aucun son ni texte ne quitte l'ordinateur.

![Capture d'écran](assets/capture.png)

Inspiré de [Meetily](https://github.com/Zackriya-Solutions/meetily), réécrit entièrement en Rust :

| | Meetily | Tour de table |
|---|---|---|
| Pile | Tauri + Next.js + serveur Python FastAPI | **Un seul exécutable Rust** |
| Interface | WebView (navigateur embarqué) | egui natif (rendu GPU) |
| Taille | Plusieurs centaines de Mo | **~12 Mo**, whisper.cpp inclus |
| RAM au repos | WebView + Python | **~90 Mo** |
| Langue | Générique | **Français québécois** (invite, typographie OQLF, lexique) |

## Pourquoi Rust?

* whisper.cpp est en C/C++ : Rust s'y lie sans surcoût (pas de cgo, pas de ramasse-miettes
  qui interrompt le fil audio en temps réel).
* Le fil audio et la transcription tournent sans pauses imprévisibles, et l'exécutable est petit.
* L'écosystème couvre déjà tout le nécessaire : `cpal` pour l'audio (WASAPI loopback sous Windows,
  « process tap » sous macOS), `whisper-rs` et `egui`.

## Mémoire : conçu pour 8 Go, optimisé pour 16 Go

| Modèle | Téléchargement | RAM en transcription | Pour qui |
|---|---|---|---|
| **Large v3 Turbo q5** (défaut) | 574 Mo | ~1,2 Go (mesuré : 0,9 à 1,15 Go) | 8 Go et plus : le meilleur rapport précision/vitesse |
| Large v3 q5 | 1,1 Go | ~3 Go | 16 Go : précision maximale, ~3× plus lent |
| Medium q5 | 539 Mo | ~1,7 Go | Portables sans GPU |
| Small q5 | 190 Mo | ~0,8 Go (mesuré : 0,55 à 0,77 Go) | Machines modestes |

* Modèles **quantifiés en 5 bits** : environ 3× moins de mémoire, avec une perte de précision minime.
* Un seul modèle est chargé à la fois et il reste en mémoire entre les rencontres (démarrage instantané).
* Le résumé (optionnel) se fait **après** la rencontre. Le modèle de langue n'est donc jamais en
  concurrence avec Whisper pour la RAM. Avec 8 Go, prenez `llama3.2:3b` (~2 Go) ; avec 16 Go,
  `qwen2.5:7b` ou `mistral` (~5 Go).
* Le contexte audio de l'encodeur est ajusté à la durée de chaque énoncé (au moins 15 s), au lieu
  des 30 s fixes de Whisper.
* **Regroupement adaptatif** : si la transcription prend du retard, les énoncés en attente sont
  fusionnés en une seule passe d'encodeur (jusqu'à 28 s), et Tour de table rattrape tout seul son retard.
* Le nombre de fils de calcul est réglé automatiquement : tous les cœurs moins deux, au plus 8.

## Spécialisation québécoise

* **Invite québécoise** : Whisper imite le texte qui précède. Tour de table lui fournit un contexte rédigé
  en québécois (« on commence-tu? », « tsé », « pantoute », « courriel », « dîner »…), suivi de la
  fin de la transcription en cours pour garder les noms cohérents d'un énoncé à l'autre.
* **Typographie OQLF** : « 14h30 » devient « 14 h 30 », « $10 » devient « 10 $ », pas d'espace
  avant ? ! ;, et une espace avant le deux-points.
* **Lexique** (Paramètres) : noms propres, organismes et acronymes, un par ligne, pour guider la
  reconnaissance. On peut aussi y mettre des corrections, p. ex. `hydro québec => Hydro-Québec`.
* **Filtre d'hallucinations** : Whisper invente parfois « Sous-titrage Société Radio-Canada » ou
  « Merci d'avoir regardé » sur du silence. Ces phrases sont retirées.
* **Modèles affinés** : vous pouvez déposer n'importe quel modèle ggml multilingue (p. ex. un Whisper
  affiné sur du français québécois et converti avec `convert-h5-to-ggml.py` de whisper.cpp) dans le
  dossier des modèles. Il apparaît alors dans les Paramètres.

## Ordre du jour et compte rendu

Déposez l'ordre du jour **avant** la rencontre (sur l'écran de départ) ou **après** (dans la fiche
de la rencontre). Vous pouvez glisser le fichier dans la fenêtre, utiliser « Parcourir… », ou le
saisir à la main.

* **Formats** : Word (`.docx`), PDF texte, `.txt`, `.md`. La numérotation automatique des listes
  Word est reconstituée (1., 2., 2.1…). Un PDF numérisé en image n'a pas de texte lisible.
* **Avant la rencontre**, les sujets et les noms de l'ordre du jour sont ajoutés à l'invite de
  Whisper, ce qui améliore la reconnaissance des noms propres et du jargon.
* **« Rédiger le compte rendu »** suit l'ordre du jour point par point, avec la même numérotation.
  Pour chaque point : discussion, décisions et suivis. Viennent ensuite les sujets hors ordre du
  jour, un tableau des suivis (action, responsable, échéance) et la prochaine rencontre. Un point
  non abordé est indiqué comme tel, et le modèle a pour consigne de ne rien inventer.
* Le texte est rédigé en **français québécois standard** (terminologie et typographie de l'OQLF),
  puis la typographie est vérifiée par Tour de table (« 14 h 30 », « 2 500 $ »).
* **Garde-fou sur les chiffres** : tout montant ou nombre du compte rendu absent de la
  transcription et de l'ordre du jour est signalé dans un encadré « À vérifier ». Exemple réel
  avec `qwen2.5:7b` : « budget dépassé de 200 $ », alors que 2 300 $ avaient été dépensés sur
  2 500 $. Le compte rendu reste un **brouillon à relire** avant diffusion.
* Sans ordre du jour, Tour de table produit un résumé structuré.
* **Longues rencontres** : au-delà d'environ 24 000 caractères (~45 min), la transcription est
  d'abord condensée par tranches, puis le compte rendu est rédigé à partir de ces notes. Les petits
  modèles locaux gardent ainsi le fil, et la mémoire reste maîtrisée.
* Exporté dans `compte-rendu.md`, et inclus dans `transcription.md`.

**Modèle de langue intégré : rien à installer.** Le programme auxiliaire `tourdetable-redaction`,
livré avec l'application, fait tourner llama.cpp sur l'ordinateur. Le modèle se télécharge une
fois, d'un clic (bouton « Installer le modèle de rédaction », ou Paramètres) :

| RAM | Modèle (licence Apache 2.0) | Téléchargement | RAM pendant la rédaction |
|---|---|---|---|
| 8 Go | **Qwen3 4B Instruct** (défaut) | 2,5 Go | ~3,2 Go |
| 16 Go | **Qwen2.5 7B Instruct** | 4,7 Go | ~5,2 Go (mesuré : 5,1 Go) |

Mesuré avec Qwen2.5 7B sur le portable de test, sans GPU : environ 4 minutes pour le compte rendu
d'une rencontre courte. La transcription doit être terminée avant : les deux modèles ne calculent
jamais en même temps (Whisper reste chargé, environ 1,1 Go, mais au repos).

Ceux qui utilisent déjà [Ollama](https://ollama.com) peuvent le choisir dans les Paramètres
(« Ollama, avancé ») ; son contexte est alors dimensionné automatiquement (8 k à 16 k jetons).

## Sources audio

| Système | Micro de la salle | Participants à distance |
|---|---|---|
| **Windows 10/11** | WASAPI | Loopback WASAPI sur la sortie (aucun pilote à installer) |
| **macOS 14.6+** | CoreAudio | « Process tap » CoreAudio (autorisation demandée au 1er usage) |
| **Linux** | ALSA / PulseAudio | Source `.monitor` de PulseAudio ou PipeWire (`parec`) |

**Deux modes :**
* **Sources séparées** (défaut) : chaque segment est étiqueté **Salle** ou **À distance**.
  Idéal avec un casque, ou un haut-parleur de conférence qui annule l'écho.
* **Sources mixées** : à choisir si le son des participants à distance sort des haut-parleurs
  de la salle. Sinon, le micro les capterait aussi et ils seraient transcrits deux fois.

La capture loopback de Windows ne livre rien quand aucun son ne joue. Tour de table réaligne donc chaque
source sur l'horloge pour garder des horodatages justes.

## Utilisation

1. Lancez Tour de table, puis cliquez sur **Télécharger Large v3 Turbo**. C'est la seule connexion
   Internet, et elle ne se fait qu'une fois (Hugging Face, dépôt officiel de whisper.cpp).
2. Choisissez le micro et la sortie audio, puis cliquez sur **Démarrer**.
3. La transcription s'affiche au fil de la rencontre et elle est sauvegardée après chaque phrase.
   Vous ne perdez donc rien en cas de panne.
4. À l'arrêt : **Rédiger le compte rendu** (modèle de langue local, installé d'un clic la
   première fois), **Exporter** (Markdown, texte, sous-titres SRT) ou **Copier**.

**Importer un enregistrement existant** : bouton « Importer un enregistrement… » ou glisser le
fichier dans la fenêtre. Formats : MP4 (enregistrements Teams), M4A (Zoom, téléphones), MP3, WAV,
FLAC, OGG. Le fichier devient une rencontre comme les autres : ordre du jour, transcription et
compte rendu. Décodage entièrement intégré (Symphonia), sans logiciel à installer.

Ligne de commande, pour transcrire un enregistrement existant :

```
tourdetable --transcrire enregistrement.m4a [modele.bin]          # MP4, M4A, MP3, WAV, FLAC, OGG
tourdetable --compte-rendu ordre-du-jour.docx enregistrement.mp4   # ou transcription.txt
```

Emplacement des données (l'application s'appelait auparavant « Noter » : ses données sont
déplacées automatiquement au premier lancement) :

| Système | Dossier |
|---|---|
| Windows | `%APPDATA%\TourDeTable\TourDeTable\data\` |
| macOS | `~/Library/Application Support/ca.TourDeTable.TourDeTable/` |
| Linux | `~/.local/share/tourdetable/` |

## Dépannage

* **Journal** : chaque lancement écrit `journal.txt` dans le dossier de données (voir plus haut),
  et le lancement précédent est conservé dans `journal-precedent.txt`. Joignez-le à toute
  demande d'aide.
* **L'application ne s'ouvre pas** (machine virtuelle, bureau à distance, pilote graphique
  absent) : Tour de table essaie d'abord Direct3D 12 sous Windows, y compris son rendu logiciel
  WARP, qui fonctionne sans carte graphique, puis OpenGL. Si les deux échouent, une boîte de
  dialogue affiche la raison. Pour imposer un moteur, définissez la variable d'environnement
  `TOURDETABLE_AFFICHAGE` à `wgpu` ou à `glow`.

## Compilation

Prérequis : Rust (stable), CMake, Clang/LLVM. Sous Linux, ajoutez `libasound2-dev`.

```
cargo build --release                    # CPU (Windows, Linux)
cargo build --release --features metal   # macOS Apple Silicon (GPU)
cargo build --release --features vulkan  # GPU AMD/Intel/NVIDIA (SDK Vulkan requis)
cargo build --release --features cuda    # GPU NVIDIA (CUDA requis)
cargo test
```

Pour un exécutable à distribuer, ajoutez `GGML_NATIVE=OFF` (sinon il est optimisé pour le seul
processeur de la machine qui compile). Le flux GitHub Actions `.github/workflows/build.yml`
produit Windows (`.zip`), macOS (`.app`) et Linux (`.tar.gz`) à chaque étiquette `v*`.

Pour régénérer les icônes et les tuiles du Store à partir de `assets/logo.svg` : `cargo run --example gen_icons`.

### Microsoft Store

La compilation Windows produit aussi `tourdetable-windows-x64.msix`, le paquet pour le
Microsoft Store (manifeste : `packaging/windows/AppxManifest.xml`, identité du Partner Center).
Il n'est pas signé : le Store le signe à la publication. Les textes de la fiche du Store sont dans
`packaging/windows/fiche-store.md`, et la politique de confidentialité dans `CONFIDENTIALITE.md`. Celle-ci est publiée à
https://danlizhot.github.io/tour-de-table-confidentialite/ (dépôt public séparé). Après une
modification, régénérez la page avec
`python3 -I packaging/politique_html.py CONFIDENTIALITE.md <dépôt-public>/index.html`, puis poussez.

## Architecture

```
src/
  main.rs        point d'entrée (interface ou --transcrire)
  app.rs         interface egui, logo vectoriel
  audio/mod.rs   capture micro + loopback par plateforme
  audio/dsp.rs   mono, rééchantillonnage 16 kHz (FIR), détection de voix, découpage
  recorder.rs    session : alignement horloge, mixage, WAV
  stt.rs         fil Whisper (modèle gardé en mémoire)
  quebec.rs      invite, typographie, lexique, filtre d'hallucinations
  storage.rs     rencontres (JSON) et exports MD/TXT/SRT
  models.rs      catalogue et téléchargement des modèles
  agenda.rs      lecture de l'ordre du jour (Word, PDF, texte)
  summary.rs     compte rendu : modèle intégré ou Ollama (découpage des longues rencontres)
  audio/import.rs  import d'enregistrements (MP4, M4A, MP3, WAV, FLAC, OGG)
redaction/       programme auxiliaire de rédaction (llama.cpp), séparé à cause de ggml
  cli.rs         commandes --transcrire et --compte-rendu
```

Fils d'exécution : `audio` (capture → 16 kHz → VAD → énoncés) → `whisper` (transcription) →
interface. Aucun travail lourd ne se fait dans le fil de l'interface.

## Le logo

Une **table ronde** vue de haut, huit **participants** autour et, au centre, une **onde sonore** :
la conversation qui fait le tour de la table. Le tout sur le **bleu du drapeau du Québec**.
Source vectorielle : `assets/logo.svg`.

## Nous joindre

[tourdetable.app@outlook.com](mailto:tourdetable.app@outlook.com)

## Licence

MIT. Inspiré de Meetily (MIT, Zackriya Solutions).
