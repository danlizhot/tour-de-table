# Politique de confidentialité — Tour de table

*Dernière mise à jour : 22 octobre 2026*

Tour de table est une application de transcription de rencontres qui fonctionne **entièrement
sur votre ordinateur**. Cette politique explique quelles données l'application traite, où elles
sont conservées et ce qui peut, ou non, quitter votre appareil.

## En bref

* Aucun compte, aucune publicité, aucune télémétrie, aucun outil d'analyse.
* L'audio, les transcriptions, les ordres du jour et les comptes rendus restent sur votre ordinateur.
* L'éditeur de Tour de table ne reçoit, ne voit et ne conserve **aucune** de vos données.

## Données traitées

| Donnée | Usage | Où elle est conservée |
|---|---|---|
| Son du micro et son de l'ordinateur (Teams, Zoom…) | Transcription, pendant l'enregistrement seulement | En mémoire ; en fichier WAV seulement si l'option « Conserver l'audio » est cochée |
| Transcriptions et titres des rencontres | Affichage, exports, comptes rendus | Dossier de données de l'application, sur votre ordinateur |
| Ordres du jour déposés (Word, PDF, texte) | Guider la transcription, rédiger le compte rendu | Le texte extrait est enregistré avec la rencontre |
| Comptes rendus et résumés | Affichage et exports | Avec la rencontre |
| Paramètres (périphériques, lexique, modèle choisi) | Fonctionnement de l'application | Fichier de configuration local |

La reconnaissance vocale (Whisper) s'exécute sur votre processeur ou votre carte graphique.
Aucun service infonuagique de transcription n'est utilisé.

## Connexions réseau

Tour de table n'établit que les connexions suivantes :

1. **Téléchargement des modèles de reconnaissance vocale**, seulement quand vous cliquez sur
   « Télécharger ». Les fichiers proviennent de Hugging Face (`huggingface.co`), le dépôt public
   officiel du projet whisper.cpp. Comme tout site Web, ce serveur peut journaliser votre adresse
   IP. Aucune de vos données n'est transmise.
2. **Rédaction des comptes rendus et des résumés**, seulement quand vous cliquez sur le bouton
   correspondant. Le texte de la transcription et de l'ordre du jour est envoyé au logiciel Ollama,
   par défaut **sur votre propre ordinateur** (`http://localhost:11434`). Si vous remplacez cette
   adresse dans les Paramètres par celle d'un autre serveur, c'est vers ce serveur que le texte sera
   envoyé, sous votre responsabilité.

L'application ne fait aucune autre connexion.

## Conservation et suppression

Vos rencontres restent sur votre ordinateur jusqu'à ce que vous les supprimiez. Le bouton
« Supprimer » déplace une rencontre dans le dossier « corbeille » de l'application, d'où vous
pouvez la récupérer ou l'effacer définitivement. Désinstaller la version du Microsoft Store
supprime toutes ses données (rencontres, modèles et paramètres).

## Votre responsabilité lors de l'enregistrement

Enregistrer ou transcrire une conversation peut être encadré par la loi, notamment au Québec par
la *Loi sur la protection des renseignements personnels dans le secteur privé* et par la *Loi sur
l'accès aux documents des organismes publics et sur la protection des renseignements personnels*.
Informez les participants, y compris ceux à distance, et obtenez leur consentement avant
d'enregistrer une rencontre.

## Enfants

L'application ne s'adresse pas aux enfants et ne recueille sciemment aucune donnée les concernant.

## Changements

Toute modification de cette politique sera publiée à cette adresse, avec sa date de mise à jour.

## Nous joindre

Pour toute question : [courriel de contact à préciser]
