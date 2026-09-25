# Fluxo

Lecteur IPTV et multimédia pour macOS, avec un cœur de données en Rust et une interface sobre.

## Essayer l’application

Pré-requis pour construire : macOS 13 ou plus récent, Rust, Node.js et les outils Xcode. Depuis ce dossier :

```bash
npm install
npm run tauri dev
```

Pour créer l’application Mac locale :

```bash
./scripts/verify.sh --bundle
```

Ouvrez ensuite `Fluxo.app` dans ce dossier. Cette application est destinée à être utilisée hors App Store. Le paquet local n’est ni signé avec un certificat Developer ID ni notarisé.

## Utilisation

- Importez une playlist M3U locale ou distante. `fixtures/demo.m3u` contient un flux HLS de démonstration Apple.
- Dans **Sources & guide TV**, connectez un compte Xtream Codes avec son serveur, son utilisateur et son mot de passe. Fluxo importe les chaînes en direct, films et séries ; un clic sur une série affiche ses épisodes. Le mot de passe reste dans le trousseau macOS et n’est pas écrit dans la bibliothèque JSON. La lecture résout l’adresse du flux seulement au moment de l’ouverture.
- Les en-têtes exigés par certaines chaînes (`http-user-agent`, `http-referrer`, `#EXTVLCOPT`, `#EXTHTTP`) et le guide annoncé par la playlist (`x-tvg-url`) sont repris à l’import. Pour une playlist importée avec une version antérieure, actualisez-la (↻) dans **Sources & guide TV**.
- Le **Guide TV** combine votre guide XMLTV personnel, ceux des playlists (seulement les pays présents, en priorité ceux de vos favoris) et le `xmltv.php` des comptes Xtream ; il est actualisé toutes les 6 heures. Les chaînes sont associées par `tvg-id`, par identifiant sans suffixe (`M6.fr@HD` → `M6.fr`) ou par nom. La vue **Guide TV** affiche une grille de 6 heures, et la liste des chaînes montre le programme en cours.
- Les chaînes Xtream avec archive proposent le **replay** : depuis la grille ou la section Replay du programme TV.
- **Reprendre** liste les films, épisodes et vidéos commencés ; leur lecture reprend à la dernière position.
- **Catégories** : le bouton **Catégories** ouvre une liste avec recherche (plusieurs mots, dans n’importe quel ordre : « belg sport »), navigation au clavier (↑ ↓, Entrée, Échap) et tri par ordre de la playlist, alphabétique ou par taille. 📌 épingle une catégorie dans les raccourcis sous la barre, à côté des dernières utilisées. Recliquer sur la catégorie active (ou ×) réaffiche tout.
- **Renommer une playlist** : ✎ à côté de son titre, double-clic dans la barre latérale, ou ✎ dans **Sources & guide TV**.
- **Filtres** : pays, langue, chaînes hors ligne et groupes masqués. **Vérifier** teste la disponibilité des chaînes affichées ; les chaînes visibles sont aussi vérifiées automatiquement (désactivable). Les favoris se réordonnent avec ↑/↓.
- **Multivue** : jusqu’à 4 chaînes à la fois ; cliquez des chaînes dans la liste pour les ajouter et sur une vignette pour entendre son son.
- ⏺ enregistre le flux en cours dans `~/Movies/Fluxo` ; **Enregistrements** liste les fichiers, lisibles dans Fluxo.
- ⓘ (ou `I`) affiche le diagnostic du flux : moteur utilisé, codecs réels, erreurs du serveur, autres sources de la chaîne.
- Ouvrez une URL de flux, un fichier audio ou une vidéo locale. Si l’URL « Lire une URL » pointe vers une playlist M3U/M3U8, ses chaînes sont ajoutées au catalogue ; un véritable flux HLS est lu directement.
- Les chaînes dont l’adresse est une page YouTube se lisent dans la zone vidéo de la fenêtre principale avec le lecteur YouTube intégré. Si un autre flux échoue dans le lecteur principal, le bouton **Ouvrir dans le navigateur** permet de l’essayer directement.
- Glissez un ou plusieurs fichiers audio/vidéo, ou un dossier contenant des vidéos, dans la fenêtre. Fluxo les ajoute à **Médias locaux** et lance le premier fichier. La liste **À suivre** permet de passer au précédent ou au suivant ; la lecture continue automatiquement au fichier suivant. Le bouton **Ouvrir des médias** accepte aussi plusieurs fichiers.
- Passez en **Mode lecteur** pour agrandir l’image, ou utilisez le bouton plein écran. Les commandes incluent lecture/pause, saut de 10 secondes, position, volume, vitesse et image dans l’image quand macOS l’autorise. Les raccourcis `Espace`, `←`, `→`, `M` et `F` agissent lorsque le focus n’est pas dans un champ ; `↑`/`↓` changent de chaîne, les chiffres donnent accès à un numéro et `⌫` revient à la chaîne précédente.
- Le bouton **CC** de la barre vidéo ouvre directement les réglages des sous-titres : choix de piste, import SRT/VTT UTF-8, taille et hauteur. Il s’allume lorsqu’une piste est active. Ces réglages restent aussi accessibles dans les **Options du lecteur**, qui proposent le choix de la piste audio ; la taille et la hauteur sont mémorisées sur ce Mac.
- Sur un flux HLS qui annonce plusieurs variantes, le bouton **Auto** de la barre vidéo permet de choisir une résolution publiée par la source (720p, 1080p, 4K, etc.) ou de revenir à l’adaptation automatique. Les vidéos YouTube utilisent le menu de qualité du lecteur YouTube intégré.

Les comptes Xtream nécessitent un accès légitime au service. Pour les fournisseurs HTTP, les identifiants sont transmis en clair sur le réseau ; préférez HTTPS quand le fournisseur le propose. Fluxo ne fournit ni chaînes ni abonnement.

## Compatibilité et limites actuelles

Le lecteur utilise le moteur multimédia de macOS, avec une escalade automatique quand un flux échoue :

1. lecture directe ;
2. relais local (127.0.0.1, adresse protégée par un jeton) qui ajoute les en-têtes du fournisseur et réécrit les playlists HLS ;
3. reconditionnement MPEG-TS → HLS pour les flux TS bruts (sorties `.ts` Xtream, `/udp/`, fichiers `.ts` locaux et enregistrements) ;
4. conversion FFmpeg pour ce que macOS ne décode pas (MPEG-2, HEVC en TS, DASH, MKV, AVI…), si FFmpeg est installé (`brew install ffmpeg`). L’image est ramenée à 1080 lignes au plus pour tenir le temps réel ; si l’encodeur matériel refuse la source, l’encodeur logiciel prend le relais.

Avant de conclure à un échec, Fluxo sonde le flux (playlist principale → variante → segment) avec les mêmes en-têtes que le lecteur et lit les codecs réellement transmis. Si la source est morte ou protégée, les autres sources de la même chaîne (même `tvg-id` ou même nom) sont essayées. Un flux figé est reconnecté automatiquement. Une chaîne sans piste audio est signalée comme telle.

Le plein écran et l’image dans l’image dépendent des capacités de WebKit. AirPlay n’est proposé que si un récepteur est disponible ; les flux passant par le relais local ne sont pas transmissibles.

Pour diagnostiquer une adresse hors de l’application : `cargo run -p fluxo --example stream_check -- <url> [relay|segmenter|transcode]`.

Une playlist peut contenir des pages web, des flux DASH, des liens expirés ou des chaînes restreintes à certains pays. L’import d’une chaîne ne garantit pas que son fournisseur autorise sa lecture sur ce Mac et à cet emplacement.

Si une lecture échoue, Fluxo affiche le motif concret : accès refusé (HTTP 403, y compris la protection `deny_backend`), adresse absente (HTTP 404), segments refusés alors que la playlist répond, page web au lieu d’un flux, certificat invalide, DNS, délai dépassé, DRM ou codec non pris en charge.

Les sous-titres SRT/VTT externes et les pistes annoncées par le média sont pris en charge dans le lecteur. La disponibilité des pistes HLS dépend de WebKit. Les réglages de position et de taille s’appliquent au rendu de sous-titres de Fluxo.

Le choix manuel de qualité HLS recharge brièvement le flux. Les variantes dont l’audio ou les sous-titres dépendent d’une playlist séparée restent en mode automatique pour conserver ces pistes. Les fichiers vidéo et les flux qui n’annoncent aucune variante n’affichent pas de choix de résolution.

Les flux DRM (FairPlay, Widevine, PlayReady) ne sont pas pris en charge. Leur intégration nécessite le type de DRM, le serveur de licences, les certificats et un flux de test autorisé du fournisseur. Le fait de distribuer l’application hors App Store ne supprime pas ces exigences. FFmpeg n’est pas embarqué dans l’application : Fluxo utilise celui installé sur le Mac (Homebrew, MacPorts ou `PATH`).

## Organisation et vérification

- `crates/iptv-core` : modèles, analyse M3U/XMLTV, HLS et MPEG-TS, normalisation des noms, tests Rust.
- `src-tauri` : intégration macOS, API Xtream, trousseau, sonde de flux, relais local, segmenteur TS, FFmpeg, guide TV, vérifications de disponibilité, enregistrements et persistance (`state.json` pour l’état utilisateur, `catalog/` pour les chaînes de chaque playlist).
- `src` : interface ; `playback.js` (moteur de lecture et bascule), `catalog.js` (doublons, filtres, sources alternatives), `guide-grid.js`, `multiview.js`.
- `scripts/verify.sh` : format Rust, tests Rust et sous-titres, Clippy, contrôle JavaScript et construction de l’interface.

La bibliothèque locale est enregistrée dans les données de l’application macOS. Les fichiers multimédias et sous-titres sélectionnés restent à leur emplacement d’origine. La vérification est aussi exécutée par GitHub Actions à chaque envoi de code.
Les dossiers déposés sont parcourus jusqu’à huit niveaux et l’import est limité à 500 médias par dépôt. Les fichiers ne sont pas copiés ; s’ils sont déplacés ou supprimés, leur entrée dans la bibliothèque devra être retirée ou réimportée.
