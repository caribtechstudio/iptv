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
- Ajoutez un guide XMLTV local ou distant pour afficher les programmes associés aux identifiants `tvg-id`.
- Ouvrez une URL de flux, un fichier audio ou une vidéo locale. Si l’URL « Lire une URL » pointe vers une playlist M3U/M3U8, ses chaînes sont ajoutées au catalogue ; un véritable flux HLS est lu directement.
- Les chaînes dont l’adresse est une page YouTube se lisent dans la zone vidéo de la fenêtre principale avec le lecteur YouTube intégré. Si un autre flux échoue dans le lecteur principal, le bouton **Ouvrir dans le navigateur** permet de l’essayer directement.
- Glissez un ou plusieurs fichiers audio/vidéo, ou un dossier contenant des vidéos, dans la fenêtre. Fluxo les ajoute à **Médias locaux** et lance le premier fichier. La liste **À suivre** permet de passer au précédent ou au suivant ; la lecture continue automatiquement au fichier suivant. Le bouton **Ouvrir des médias** accepte aussi plusieurs fichiers.
- Passez en **Mode lecteur** pour agrandir l’image, ou utilisez le bouton plein écran. Les commandes incluent lecture/pause, saut de 10 secondes, position, volume, vitesse et image dans l’image quand macOS l’autorise. Les raccourcis `Espace`, `←`, `→` et `F` agissent lorsque le focus n’est pas dans un champ.
- Le bouton **CC** de la barre vidéo ouvre directement les réglages des sous-titres : choix de piste, import SRT/VTT UTF-8, taille et hauteur. Il s’allume lorsqu’une piste est active. Ces réglages restent aussi accessibles dans les **Options du lecteur**, qui proposent le choix de la piste audio ; la taille et la hauteur sont mémorisées sur ce Mac.
- Sur un flux HLS qui annonce plusieurs variantes, le bouton **Auto** de la barre vidéo permet de choisir une résolution publiée par la source (720p, 1080p, 4K, etc.) ou de revenir à l’adaptation automatique. Les vidéos YouTube utilisent le menu de qualité du lecteur YouTube intégré.

Les comptes Xtream nécessitent un accès légitime au service. Pour les fournisseurs HTTP, les identifiants sont transmis en clair sur le réseau ; préférez HTTPS quand le fournisseur le propose. Fluxo ne fournit ni chaînes ni abonnement.

## Compatibilité et limites actuelles

Le lecteur intégré utilise le moteur multimédia de macOS. HLS, MP4/MOV, MP3, M4A/AAC, WAV et AIFF sont les formats principaux visés. Un fichier MKV, AVI, FLAC, OGG ou TS peut apparaître dans le sélecteur, mais sa lecture dépend du conteneur et des codecs disponibles sur ce Mac. Le plein écran et l’image dans l’image dépendent aussi des capacités de WebKit. Certains services Xtream n’exposent qu’un flux MPEG-TS qui peut échouer dans ce lecteur.

Une playlist peut contenir des pages web, des flux DASH, des liens expirés ou des chaînes restreintes à certains pays. L’import d’une chaîne ne garantit pas que son fournisseur autorise sa lecture sur ce Mac et à cet emplacement.

Les sous-titres SRT/VTT externes et les pistes annoncées par le média sont pris en charge dans le lecteur. La disponibilité des pistes HLS dépend de WebKit. Les réglages de position et de taille s’appliquent au rendu de sous-titres de Fluxo.

Le choix manuel de qualité HLS recharge brièvement le flux. Les variantes dont l’audio ou les sous-titres dépendent d’une playlist séparée restent en mode automatique pour conserver ces pistes. Les fichiers vidéo et les flux qui n’annoncent aucune variante n’affichent pas de choix de résolution.

Les flux DRM (FairPlay, Widevine, PlayReady) ne sont pas encore pris en charge. Leur intégration nécessite le type de DRM, le serveur de licences, les certificats et un flux de test autorisé du fournisseur. Le fait de distribuer l’application hors App Store ne supprime pas ces exigences. Une prise en charge plus large des codecs demande l’intégration et la distribution d’un moteur multimédia supplémentaire, par exemple libmpv/FFmpeg, puis des tests de codecs et de licences.

## Organisation et vérification

- `crates/iptv-core` : modèles, analyse M3U/XMLTV et tests Rust.
- `src-tauri` : intégration macOS, API Xtream, trousseau, import et persistance atomique.
- `src` : interface et commandes du lecteur.
- `scripts/verify.sh` : format Rust, tests Rust et sous-titres, Clippy, contrôle JavaScript et construction de l’interface.

La bibliothèque locale est enregistrée dans les données de l’application macOS. Les fichiers multimédias et sous-titres sélectionnés restent à leur emplacement d’origine. La vérification est aussi exécutée par GitHub Actions à chaque envoi de code.
Les dossiers déposés sont parcourus jusqu’à huit niveaux et l’import est limité à 500 médias par dépôt. Les fichiers ne sont pas copiés ; s’ils sont déplacés ou supprimés, leur entrée dans la bibliothèque devra être retirée ou réimportée.
