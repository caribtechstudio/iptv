# Fluxo

Lecteur IPTV et multimédia pour macOS, avec une interface simple et un cœur de données en Rust.

## Essayer l’application

Pré-requis : macOS 13 ou plus récent, Rust, Node.js et les outils Xcode. Depuis ce dossier :

```bash
npm install
npm run tauri dev
```

Pour créer l’application Mac :

```bash
./scripts/verify.sh --bundle
```

L’application se trouve ensuite directement dans ce dossier sous le nom `Fluxo.app`. Double-cliquez dessus pour l’ouvrir. Il s’agit d’une version locale non signée, utilisable pour les essais. Une distribution publique demanderait une signature Apple et une notarisation.

## Premiers pas

- Cliquez sur **＋** à côté de « Playlists » et choisissez `fixtures/demo.m3u` pour essayer un flux HLS de démonstration Apple.
- Importez vos propres fichiers ou URL M3U pour consulter les chaînes, groupes et favoris.
- Cliquez sur **Lire une URL** pour un flux HLS direct ou un média en ligne.
- Cliquez sur **Ouvrir un média** pour choisir un fichier audio ou vidéo local.
- Dans **Sources & guide TV**, ajoutez un fichier ou une URL XMLTV pour le guide des programmes. Le lien se fait grâce à `tvg-id` dans la playlist et `channel` dans le XMLTV.

Fluxo n’inclut aucune chaîne ni abonnement. Les formats lus dépendent des codecs pris en charge par le moteur multimédia de macOS. HLS, MP4, MOV, MP3, M4A, AAC et WAV sont les premiers formats visés. Certains fichiers MKV ou flux spécialisés peuvent être refusés. Les flux protégés par DRM ou nécessitant des en-têtes personnalisés ne sont pas pris en charge dans cette version.

## Organisation

- `crates/iptv-core` : modèles, analyse M3U/XMLTV et tests en Rust.
- `src-tauri` : intégration macOS, import réseau/local et persistance atomique.
- `src` : interface du lecteur.
- `fixtures` : playlist de démonstration.
- `scripts/verify.sh` : boucle de contrôle locale.

La bibliothèque locale est stockée dans le dossier de données de l’application macOS. Les fichiers multimédias sélectionnés restent à leur emplacement d’origine.

## Vérification continue

`./scripts/verify.sh` vérifie le format Rust, les tests du cœur, les avertissements du compilateur et la construction de l’interface. `./scripts/verify.sh --bundle` ajoute la construction du paquet macOS. Le même processus est exécuté sur macOS par GitHub Actions à chaque envoi de code.
