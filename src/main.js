import { invoke, convertFileSrc } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open, save } from '@tauri-apps/plugin-dialog';
import { readText } from '@tauri-apps/plugin-clipboard-manager';
import { openUrl, revealItemInDir } from '@tauri-apps/plugin-opener';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { parseSubtitles } from './subtitles.js';
import { isWebUrl, isYouTubePage } from './media-sources.js';
import { isHlsMediaUrl, parseHlsQualities } from './hls-quality.js';
import { alternativesFor, countryLabel, dedupeByUrl, facet, filterChannels, flag, normalizeQuery } from './catalog.js';
import { ENGINE_PREFERENCES, MODE_LABELS, Player, engineOf } from './playback.js';
import { MpvMedia } from './mpv-media.js';
import { gridWindow, renderGuideGrid } from './guide-grid.js';
import { createMultiview } from './multiview.js';
import { hydrateIcons, icon, iconButton, setIcon } from './icons.js';
import { RateMeter, formatBitrate, formatBytes } from './bitrate.js';
import { createFilmstrip } from './filmstrip.js';
import './style.css';

const app = document.querySelector('#app');
app.innerHTML = `
  <div class="shell">
    <aside class="sidebar">
      <div class="brand"><span class="brand-icon"><i data-icon="play"></i></span><span>Fluxo</span></div>
      <nav class="main-nav" aria-label="Navigation">
        <button data-view="all" class="active"><i data-icon="grid" class="nav-icon"></i> Toutes les chaînes</button>
        <button data-view="guide"><i data-icon="guide" class="nav-icon"></i> Guide TV</button>
        <button data-view="favorites"><i data-icon="star" class="nav-icon"></i> Favoris</button>
        <button data-view="recent"><i data-icon="history" class="nav-icon"></i> Récents</button>
        <button data-view="continue"><i data-icon="play-circle" class="nav-icon"></i> Reprendre</button>
      </nav>
      <div class="sidebar-heading"><span>PLAYLISTS</span><button id="addPlaylistShortcut" title="Ajouter une playlist" aria-label="Ajouter une playlist"><i data-icon="plus"></i></button></div>
      <div id="playlistNav" class="playlist-nav"></div>
      <div class="sidebar-bottom"><button id="recordingsButton"><i data-icon="record"></i><span>Enregistrements</span></button><button id="converterButton"><i data-icon="convert"></i><span>Convertisseur</span><span id="converterBadge" class="sidebar-badge" hidden></span></button><button id="settingsButton"><i data-icon="settings"></i><span>Sources & guide TV</span></button></div>
    </aside>
    <main class="main">
      <header class="topbar"><div><div class="eyebrow">VOTRE LECTEUR</div><div class="title-row"><h1 id="viewTitle">Toutes les chaînes</h1><button id="renameView" class="title-edit" title="Renommer la playlist" aria-label="Renommer la playlist" hidden><i data-icon="edit"></i></button><button id="exportView" class="title-edit" title="Exporter la playlist en M3U" aria-label="Exporter la playlist en M3U" hidden><i data-icon="download"></i></button></div></div><div class="top-actions"><button id="multiviewMode" class="subtle" title="Regarder jusqu’à 4 chaînes à la fois"><i data-icon="multiview"></i><span>Multivue</span></button><button id="playerMode" class="subtle" title="Agrandir le lecteur"><i data-icon="player-mode"></i><span>Mode lecteur</span></button><button id="openMedia" class="subtle"><i data-icon="folder-open"></i><span>Ouvrir des médias</span></button><button id="playUrl" class="primary"><i data-icon="link"></i><span>Lire une URL</span></button></div></header>
      <div class="content">
        <section class="catalogue"><div class="search-wrap"><i data-icon="search"></i><input id="search" type="search" placeholder="Rechercher une chaîne ou un groupe…" aria-label="Rechercher"><span id="channelCount" class="channel-count" aria-live="polite"></span></div><div class="filters-wrap"><div class="filters"><button id="groupPicker" class="group-picker" aria-haspopup="listbox" aria-expanded="false" aria-controls="groupPanel"><span id="groupPickerLabel" class="group-picker-label">Catégories</span><span id="groupPickerCount" class="group-picker-count"></span><i data-icon="chevron-down" class="group-picker-chevron"></i></button><button id="groupClear" class="group-clear" title="Afficher toutes les catégories" aria-label="Afficher toutes les catégories" hidden><i data-icon="x"></i></button><button id="filtersButton" class="filter-button" title="Pays, langue, groupes masqués, chaînes hors ligne"><i data-icon="filters"></i><span>Filtres</span></button><button id="checkButton" class="filter-button" title="Vérifier la disponibilité des chaînes affichées"><i data-icon="list-checks"></i><span>Vérifier</span></button></div><div id="groupPanel" class="group-panel" hidden><div class="group-panel-head"><input id="groupSearch" type="search" placeholder="Rechercher une catégorie…" aria-label="Rechercher une catégorie" autocomplete="off"><div class="group-sort" role="group" aria-label="Ordre des catégories"><button data-sort="list" title="Ordre de la playlist">Liste</button><button data-sort="az" title="Ordre alphabétique">A–Z</button><button data-sort="count" title="Les plus grandes d’abord">Taille</button></div></div><div id="groupList" class="group-list-panel" role="listbox" aria-label="Catégories"></div><p class="group-panel-hint">↑ ↓ pour naviguer · Entrée pour choisir · épingle pour garder en raccourci</p></div></div><div id="groupFilters" class="group-filters" hidden></div><div id="channels" class="channels"></div><div id="guideGrid" class="guide-grid" hidden></div></section>
        <section class="player-pane"><div class="player-card"><div class="video-wrap" id="videoWrap"><video id="video" playsinline preload="metadata" x-webkit-airplay="allow"></video><div id="subtitleOverlay" class="subtitle-overlay" aria-live="off"></div><div id="zapOverlay" class="zap-overlay" hidden></div><div id="videoEmpty" class="video-empty"><div class="empty-glyph"><i data-icon="play"></i></div><strong>Prêt à regarder</strong><span>Choisissez une chaîne ou glissez des vidéos ici.</span></div><div class="player-controls" id="playerControls"><div id="seekPreview" class="seek-preview" hidden><div class="seek-preview-image"></div><span></span></div><input id="seek" type="range" min="0" max="1000" value="0" aria-label="Position de lecture"><div class="controls-row"><button id="togglePlay" title="Lecture / pause (Espace)" aria-label="Lecture / pause"><i data-icon="play"></i></button><button id="stopPlayback" title="Arrêter la lecture (S)" aria-label="Arrêter la lecture"><i data-icon="stop"></i></button><button id="back10" title="Reculer de 10 secondes (←)" aria-label="Reculer de 10 secondes"><i data-icon="back10"></i></button><button id="forward10" title="Avancer de 10 secondes (→)" aria-label="Avancer de 10 secondes"><i data-icon="forward10"></i></button><span id="timeLabel">00:00 / 00:00</span><span id="bitrateLabel" class="bitrate-label" hidden><i data-icon="activity"></i><span></span></span><div class="controls-spacer"></div><button id="mute" title="Couper le son (M)" aria-label="Couper le son"><i data-icon="volume-high"></i></button><input id="volume" type="range" min="0" max="1" step="0.01" value="1" aria-label="Volume"><button id="filmstripButton" title="Plan de la vidéo (P)" aria-label="Afficher le plan de la vidéo" aria-pressed="false" hidden><i data-icon="film"></i></button><button id="subtitleOptions" title="Gérer les sous-titres" aria-label="Gérer les sous-titres" aria-haspopup="dialog"><i data-icon="captions"></i></button><button id="qualityOptions" title="Qualité vidéo" aria-label="Qualité vidéo" aria-haspopup="dialog" hidden>Auto</button><button id="recordButton" title="Enregistrer ce direct" aria-label="Enregistrer"><i data-icon="record"></i></button><button id="airplay" title="AirPlay" aria-label="AirPlay" hidden><i data-icon="airplay"></i></button><button id="playerOptions" title="Options du lecteur" aria-label="Options du lecteur"><i data-icon="settings"></i></button><button id="pip" title="Image dans l’image" aria-label="Image dans l’image"><i data-icon="pip"></i></button><button id="fullscreen" title="Plein écran (F)" aria-label="Plein écran"><i data-icon="fullscreen"></i></button></div></div></div><div class="player-meta"><div class="meta-top"><div class="live-indicator" id="liveIndicator">LECTEUR</div><span id="engineBadge" class="engine-badge" hidden></span><span id="bitrateBadge" class="bitrate-badge" hidden><i data-icon="activity"></i><span></span></span></div><div class="player-title-row"><h2 id="playingTitle">Aucune lecture</h2><button id="streamInfo" class="meta-button" title="Informations sur le flux (I)" aria-label="Informations sur le flux" hidden><i data-icon="info"></i></button><button id="favoriteButton" class="meta-button" title="Ajouter aux favoris" aria-label="Ajouter aux favoris" hidden><i data-icon="star"></i></button></div><p id="playingDetail">La vidéo et l’audio se lisent ici.</p><p id="playerStatus" class="player-status" aria-live="polite"></p><p id="playerError" role="alert"></p></div></div><div id="queueCard" class="queue-card" hidden><div class="queue-head"><div><h3>À suivre</h3><span id="queueCount"></span></div><div class="queue-actions"><button id="queuePrevious" aria-label="Média précédent" title="Média précédent"><i data-icon="chevron-left"></i></button><button id="queueNext" aria-label="Média suivant" title="Média suivant"><i data-icon="chevron-right"></i></button><button id="queueClear" title="Effacer la file">Effacer</button></div></div><div id="queueItems" class="queue-items"></div></div><div class="guide-card"><div class="guide-header"><h3>Programme TV</h3><span id="guideStatus">Ajoutez un guide XMLTV</span></div><div id="programs" class="programs"><p class="muted">Sélectionnez une chaîne pour voir son programme.</p></div></div></section>
        <section id="multiview" class="multiview" hidden></section>
      </div>
    </main>
  </div>
  <div id="dropOverlay" class="drop-overlay" hidden><div class="drop-head"><strong id="dropTitle">Déposez vos médias</strong><span id="dropDetail">Vidéos, fichiers audio ou dossiers</span></div><div class="drop-zones"><div class="drop-zone" data-action="play"><i data-icon="play-circle"></i><strong>Lire maintenant</strong><span>Ajoutés aux médias locaux, le premier démarre et les suivants s’enchaînent</span></div><div class="drop-zone" data-action="queue"><i data-icon="list-plus"></i><strong>Ajouter à la suite</strong><span>Après le média en cours, sans l’interrompre</span></div><div class="drop-zone" data-action="convert"><i data-icon="convert"></i><strong>Convertir</strong><span>Dans un autre format vidéo ou audio</span></div></div></div>
  <div id="modal" class="modal-backdrop" hidden><div class="modal" role="dialog" aria-modal="true" aria-labelledby="modalTitle"><div class="modal-head"><h2 id="modalTitle"></h2><button id="modalClose" class="close" aria-label="Fermer" title="Fermer"><i data-icon="x"></i></button></div><div id="modalBody"></div></div></div>
  <div id="updateBanner" class="update-banner" role="status" aria-live="polite" hidden><div><strong id="updateTitle"></strong><span id="updateDetail"></span></div><button id="updateInstall" class="primary">Installer et redémarrer</button><button id="updateLater" class="subtle">Plus tard</button></div>
  <div id="toast" role="status" aria-live="polite"></div>`;
hydrateIcons(app);

const $ = (id) => document.getElementById(id);
const state = {
  library: { playlists: [], favorites: [], recent: [], epgSource: null, xtreamAccounts: [], hiddenGroups: [], progress: [], ignorePlaylistEpg: false },
  view: 'all', group: 'Tous', query: '', country: '', language: '', hideOffline: false,
  playing: null, previous: null, subtitleCues: [], subtitleTrack: 'off', queue: [], queueIndex: -1,
  health: {}, engine: { ffmpeg: null, recordingsDir: '', mpv: { available: false }, platform: 'macos', version: '' }, recordings: new Map(), epg: { programs: 0, loading: false },
};
let catalog = { all: [], byUrl: new Map(), byId: new Map() };
const video = $('video');
const quality = { source: null, variants: [], selected: 'auto', resumeTime: null, resumePlaying: false, request: 0, switch: 0 };
const youtubePlayback = make('div', 'youtube-playback'); youtubePlayback.hidden = true;
const youtubeStatus = make('strong', '', 'Recherche du direct YouTube…');
const youtubeDetail = make('p', '', 'La vidéo apparaîtra ici.');
const youtubeRetry = button('Réessayer', 'primary', openCurrentYouTube);
youtubePlayback.append(youtubeStatus, youtubeDetail, youtubeRetry);
$('videoWrap').append(youtubePlayback);
let youtubeSession = 0;
let youtubeBoundsFrame = 0;
const openSourceExternally = button('Ouvrir dans le navigateur', 'subtle external-action', openCurrentSourceExternally);
openSourceExternally.hidden = true; $('playerError').after(openSourceExternally);
let toastTimer;
const preferences = (() => {
  // mpv first by default; `engineChosen` keeps an engine picked in the settings.
  const defaults = { autoCheck: true, pinnedGroups: [], recentGroups: [], groupSort: 'list', engine: 'mpv', engineChosen: false, autoUpdate: true };
  try {
    const saved = JSON.parse(localStorage.getItem('fluxo-preferences') || '{}');
    return { ...defaults, ...saved, engine: saved.engineChosen ? saved.engine : defaults.engine };
  } catch { return defaults; }
})();
function savePreferences() { try { localStorage.setItem('fluxo-preferences', JSON.stringify(preferences)); } catch { /* private storage */ } }
const subtitleSettings = (() => {
  try { return { size: 100, position: 8, ...JSON.parse(localStorage.getItem('fluxo-subtitles') || '{}') }; }
  catch { return { size: 100, position: 8 }; }
})();
function applySubtitleSettings() {
  const size = Math.max(60, Math.min(200, Number(subtitleSettings.size) || 100));
  const position = Math.max(2, Math.min(40, Number(subtitleSettings.position) || 8));
  $('subtitleOverlay').style.fontSize = `${size}%`;
  $('subtitleOverlay').style.bottom = `${position}%`;
  try { localStorage.setItem('fluxo-subtitles', JSON.stringify({ size, position })); } catch { /* private storage */ }
}
applySubtitleSettings();

function toast(message, kind = '') {
  const el = $('toast'); el.textContent = message; el.className = kind; el.classList.add('show');
  clearTimeout(toastTimer); toastTimer = setTimeout(() => el.classList.remove('show'), 4800);
}
function errorMessage(error) { return typeof error === 'string' ? error : error?.message || String(error); }
function make(tag, className, text) { const el = document.createElement(tag); if (className) el.className = className; if (text !== undefined) el.textContent = text; return el; }
function button(text, className, action) { const el = make('button', className, text); el.onclick = action; return el; }
function withIcon(el, name) { el.classList.add('with-icon'); el.prepend(icon(name)); return el; }
function emptyIcon(name) { const el = make('div', 'empty-icon'); el.append(icon(name)); return el; }
function clock(seconds) { return new Date(seconds * 1000).toLocaleTimeString('fr-FR', { hour: '2-digit', minute: '2-digit' }); }
function timeText(value) {
  if (!Number.isFinite(value)) return '00:00';
  const hours = Math.floor(value / 3600); const minutes = Math.floor(value / 60) % 60; const seconds = Math.floor(value) % 60;
  return `${hours ? `${String(hours).padStart(2, '0')}:` : ''}${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
}
function headersOf(channel) { return { userAgent: channel?.userAgent || null, referrer: channel?.referrer || null }; }
function epgRef(channel) { return { key: channel.id, tvgId: channel.tvgId || null, name: channel.name }; }

// ---------- Lecteur ----------

/** Engine that last played each address (`mpv` or `system`), used by the automatic choice. */
const engineMemory = (() => {
  const KEY = 'fluxo-engines';
  const LIMIT = 500;
  let entries;
  try { entries = new Map(JSON.parse(localStorage.getItem(KEY) || '[]')); } catch { entries = new Map(); }
  return {
    get: (url) => entries.get(url) || null,
    set(url, engine) {
      if (!url || entries.get(url) === engine) return;
      entries.delete(url); entries.set(url, engine);
      while (entries.size > LIMIT) entries.delete(entries.keys().next().value);
      try { localStorage.setItem(KEY, JSON.stringify([...entries])); } catch { /* private storage */ }
    },
    clear() { entries.clear(); try { localStorage.removeItem(KEY); } catch { /* private storage */ } },
  };
})();

const playerDeps = {
  invoke,
  ffmpeg: () => Boolean(state.engine.ffmpeg),
  mpv: () => Boolean(state.engine.mpv?.available),
  preference: () => (ENGINE_PREFERENCES.includes(preferences.engine) ? preferences.engine : 'mpv'),
  remembered: (url) => engineMemory.get(url),
  platform: () => state.engine.platform || 'macos',
  nativeSrc: async (url) => {
    if (!url.startsWith('file:')) return url;
    return convertFileSrc(await invoke('allow_media_file', { path: decodeURIComponent(new URL(url).pathname) }));
  },
  resolve: async (channel, ctx) => {
    let url = channel.streamUrl;
    if (ctx.catchup) url = await invoke('resolve_catchup', { reference: channel.streamUrl, start: ctx.catchup.start, stop: ctx.catchup.stop });
    else if (url.startsWith('xtream://')) url = await invoke('resolve_stream', { reference: url, extension: channel.containerExtension || null });
    return { url, headers: headersOf(channel), live: !ctx.catchup && channel.kind === 'live' };
  },
};

function reportHealth(url, ok, message = null) {
  if (!isWebUrl(url)) return;
  state.health[url] = { ok, message, checkedAt: Math.floor(Date.now() / 1000) };
  invoke('report_health', { url, ok, message }).catch(() => {});
}

const mainMpv = new MpvMedia('main', { invoke, listen });
mainMpv.track($('videoWrap'), { clip: () => document.querySelector('.player-pane').getBoundingClientRect() });
/** Whatever currently shows the video: the <video> element or the mpv engine. */
const media = () => player.media;
const player = new Player(video, playerDeps, {
  interceptError: () => {
    if (quality.selected === 'auto') return false;
    fallbackQuality();
    return true;
  },
  onSource: (ctx) => {
    filmstrip.close();
    const channel = ctx.channel;
    state.playing = { url: channel.streamUrl, name: channel.name, channelId: channel.id.startsWith('recent|') || channel.id.startsWith('progress|') ? null : channel.id, tvgId: channel.tvgId, channel, catchup: ctx.catchup || null, kind: channel.kind };
    $('playingTitle').textContent = ctx.catchup ? `${channel.name} · ${ctx.catchup.title}` : channel.name;
    const source = channel.streamUrl.startsWith('file:') ? 'Fichier local' : channel.streamUrl.startsWith('xtream:') ? 'Catalogue Xtream' : channel.streamUrl;
    $('playingDetail').textContent = ctx.alternatives.length > 1 ? `Source ${ctx.index + 1}/${ctx.alternatives.length} · ${source}` : source;
    $('liveIndicator').textContent = ctx.catchup ? 'REPLAY' : channel.kind === 'live' ? 'DIRECT' : 'LECTEUR';
    $('playerError').textContent = ''; $('playerStatus').textContent = ''; openSourceExternally.hidden = true;
    $('streamInfo').hidden = false;
    state.subtitleCues = []; selectSubtitleTrack('off');
    updateFavoriteButton(); markPlayingRows(); updateRecordButton();
    renderPrograms(channel);
  },
  onPlan: (plan, ctx) => {
    $('engineBadge').hidden = false;
    $('engineBadge').textContent = MODE_LABELS[plan.mode];
    if (!ctx.resumeChecked) {
      ctx.resumeChecked = true;
      const saved = !ctx.live && state.library.progress.find((item) => item.url === ctx.channel.streamUrl);
      if (saved) { ctx.resumeAt = saved.position; toast(`Reprise à ${timeText(saved.position)}.`); }
    }
  },
  onLoaded: (ctx, src) => {
    resetQuality(['native', 'relay'].includes(ctx.plan.mode) && isHlsMediaUrl(src) ? src : null);
    if (quality.source) void discoverQualities(src, state.playing);
  },
  onStatus: (text) => { $('playerStatus').textContent = text; },
  onStarted: (ctx) => {
    $('playerStatus').textContent = ''; $('playerError').textContent = ''; openSourceExternally.hidden = true;
    reportHealth(ctx.url, true);
    engineMemory.set(ctx.channel?.streamUrl, engineOf(ctx.plan?.mode));
    updateRecordButton();
  },
  onRetry: (next, ctx, reason) => {
    $('playerStatus').textContent = `Nouvel essai : ${MODE_LABELS[next.mode]}…`;
    if (reason === 'audio') toast('Son non décodé par macOS : conversion FFmpeg en cours…');
  },
  onSourceFailed: (ctx, message) => { if (ctx.url) reportHealth(ctx.url, false, message); },
  onFailover: (ctx, message) => {
    toast(`${message} Essai de la source ${ctx.index + 1}/${ctx.alternatives.length}…`, 'error');
  },
  onFailure: (message, ctx) => {
    $('playerStatus').textContent = '';
    $('playerError').textContent = ctx.alternatives.length > 1 ? `${message} (${ctx.alternatives.length} sources essayées)` : message;
    openSourceExternally.hidden = !isWebUrl(ctx.url || ctx.channel?.streamUrl);
    markPlayingRows();
  },
  onAudioIssue: (kind) => {
    $('playerStatus').textContent = kind === 'missing'
      ? 'Cette chaîne ne diffuse aucune piste audio : l’absence de son vient de la source.'
      : 'La piste audio n’est pas décodée par macOS. Installez FFmpeg (brew install ffmpeg) pour la convertir.';
  },
}, { watchdog: true, mpv: mainMpv });
for (const type of ['emptied', 'loadstart']) {
  mainMpv.addEventListener(type, () => $('videoWrap').classList.toggle('mpv-active', Boolean(mainMpv.getAttribute('src'))));
}

/** The video being played, when FFmpeg can take pictures of it (films, episodes, files). */
function filmstripSource() {
  const ctx = player.info;
  const url = String(ctx?.url || '');
  if (!url || ctx.live || !state.engine.ffmpeg || !player.media.getAttribute('src')) return null;
  const local = url.startsWith('file:') || url.startsWith('/');
  if (!local && !/^https?:\/\//i.test(url)) return null;
  return { source: url, headers: ctx.headers || {}, local };
}
const filmstrip = createFilmstrip($('videoWrap'), {
  invoke, listen, timeText,
  media: () => player.media,
  source: filmstripSource,
  onError: () => toast('Impossible de prendre des images de cette vidéo : le serveur ne permet peut-être pas de s’y déplacer.', 'error'),
  onToggle: (open) => {
    $('filmstripButton').classList.toggle('active', open);
    $('filmstripButton').setAttribute('aria-pressed', String(open));
    wakeControls();
  },
});

function youtubeBounds() {
  if (!$('modal').hidden || !$('dropOverlay').hidden || document.body.classList.contains('multiview-mode')) return null;
  const rect = $('videoWrap').getBoundingClientRect();
  const pane = document.querySelector('.player-pane').getBoundingClientRect();
  if (rect.width <= 0 || rect.height <= 0 || rect.left < pane.left - 1 || rect.right > pane.right + 1 || rect.top < pane.top - 1 || rect.bottom > pane.bottom + 1) return null;
  return { x: rect.left, y: rect.top, width: rect.width, height: rect.height };
}
function scheduleYoutubeBounds() {
  if (!youtubeSession || youtubeBoundsFrame) return;
  youtubeBoundsFrame = requestAnimationFrame(() => {
    youtubeBoundsFrame = 0;
    if (youtubeSession && isYouTubePage(state.playing?.url)) {
      invoke('set_youtube_player_bounds', { session: youtubeSession, bounds: youtubeBounds() }).catch((error) => {
        $('playerError').textContent = `Placement du lecteur YouTube impossible : ${errorMessage(error)}`;
      });
    }
  });
}
async function openCurrentSourceExternally() {
  const url = player.info?.url || state.playing?.url;
  if (!isWebUrl(url)) return;
  try { await openUrl(url); }
  catch (error) { $('playerError').textContent = `Impossible d’ouvrir le navigateur : ${errorMessage(error)}`; }
}
async function openCurrentYouTube() {
  const playing = state.playing;
  if (!isYouTubePage(playing?.url)) return;
  youtubeStatus.textContent = 'Recherche du direct YouTube…';
  youtubeDetail.textContent = 'La vidéo apparaîtra ici.';
  youtubeRetry.disabled = true;
  try {
    const session = await invoke('open_youtube_player', { url: playing.url, bounds: youtubeBounds() || { x: 0, y: 0, width: 1, height: 1 } });
    if (state.playing !== playing || !session) return;
    youtubeSession = session;
    scheduleYoutubeBounds();
    youtubeStatus.textContent = 'Lecture dans Fluxo';
    youtubeDetail.textContent = 'Réglez sa qualité avec ⚙ dans le lecteur YouTube.';
  } catch (error) {
    if (state.playing !== playing) return;
    youtubeStatus.textContent = 'Direct YouTube indisponible';
    youtubeDetail.textContent = errorMessage(error);
  } finally { youtubeRetry.disabled = false; }
}
async function hideYoutube() {
  youtubeSession = 0;
  $('videoWrap').classList.remove('youtube-source');
  youtubePlayback.hidden = true;
  await invoke('hide_youtube_player').catch((error) => { $('playerError').textContent = errorMessage(error); });
}

// ---------- Qualité HLS ----------

function resetQuality(source = null) {
  quality.request += 1; quality.switch += 1;
  Object.assign(quality, { source, variants: [], selected: 'auto', resumeTime: null, resumePlaying: false });
  $('qualityOptions').hidden = true;
  $('qualityOptions').textContent = 'Auto';
}
function updateQualityButton() {
  const el = $('qualityOptions');
  el.hidden = quality.variants.length === 0;
  const selected = quality.variants.find((variant) => variant.url === quality.selected);
  el.textContent = selected ? (selected.height >= 2160 ? '4K' : selected.height ? `${selected.height}p` : 'Qualité') : 'Auto';
  el.title = selected ? `Qualité vidéo : ${selected.label}` : 'Qualité vidéo : automatique';
}
async function discoverQualities(source, playing) {
  const request = quality.request;
  try {
    const manifest = await invoke('fetch_hls_manifest', { source });
    if (request !== quality.request || playing !== state.playing) return;
    quality.variants = parseHlsQualities(manifest.text, manifest.url);
    updateQualityButton();
  } catch {
    if (request === quality.request) quality.variants = [];
  }
}
function switchQuality(selected, resume = {}) {
  if (selected === quality.selected || !quality.source) return;
  const variant = quality.variants.find((item) => item.url === selected && item.selectable);
  if (selected !== 'auto' && !variant) return;
  const wasPlaying = 'playing' in resume ? resume.playing : !video.paused;
  const time = 'time' in resume ? resume.time : Number.isFinite(video.duration) ? video.currentTime : null;
  const source = selected === 'auto' ? quality.source : variant.url;
  Object.assign(quality, { selected, resumeTime: time, resumePlaying: wasPlaying });
  const switchId = ++quality.switch;
  updateQualityButton();
  if (Number.isFinite(time) && time > 0) {
    video.addEventListener('loadedmetadata', () => {
      if (switchId !== quality.switch || !Number.isFinite(video.duration) || video.duration <= 0) return;
      try { video.currentTime = Math.min(time, Math.max(0, video.duration - 0.2)); } catch { /* The stream may not be seekable. */ }
    }, { once: true });
  }
  video.src = source;
  video.load();
  if (wasPlaying) video.play().catch(() => { if (selected !== 'auto' && quality.selected === selected) fallbackQuality(); });
}
function fallbackQuality() {
  if (quality.selected === 'auto') return;
  switchQuality('auto', { time: quality.resumeTime, playing: quality.resumePlaying });
  toast('Cette qualité ne peut pas être lue. Retour au réglage automatique.', 'error');
}
function showQualityOptions() {
  if (!quality.variants.length) return;
  modal('Qualité vidéo', (body) => {
    const choices = [['auto', 'Automatique'], ...quality.variants.filter((variant) => variant.selectable).map((variant) => [variant.url, variant.label])];
    body.append(selectField('Résolution', choices, quality.selected, (selected) => { closeModal(); switchQuality(selected); }));
    const separate = quality.variants.some((variant) => !variant.selectable);
    body.append(make('p', 'muted', separate
      ? 'Les qualités avec pistes audio ou sous-titres séparés restent en mode automatique pour conserver ces pistes.'
      : 'Les qualités affichées sont celles annoncées par le flux. Le changement recharge brièvement la vidéo.'));
  });
}

// ---------- Débit ----------
// Sampled every second: mpv gives its network speed, the local relay counts the bytes it
// serves, and WebKit the bytes it decodes. Without any of them, the rate announced by the
// chosen HLS variant is shown as an estimate.

const BITRATE_SOURCES = {
  network: 'Débit réseau',
  stream: 'Débit du fichier lu',
  decoded: 'Débit reçu par le lecteur',
  converted: 'Débit après conversion FFmpeg',
  declared: 'Débit annoncé par la source',
};
const bitrate = { meter: new RateMeter(), key: '', busy: false, source: '', detail: null, declared: 0 };
/** The rate to show and where it comes from. When a film is fully buffered the network rests:
 * the bitrate of the stream being decoded is shown instead. */
function shownBitrate() {
  const measured = bitrate.meter.bitsPerSecond;
  if (Number.isFinite(measured) && measured > 0) return { value: measured, source: bitrate.source };
  const stream = (bitrate.detail?.video || 0) + (bitrate.detail?.audio || 0);
  if (stream > 0) return { value: stream, source: 'stream' };
  if (bitrate.source === 'declared' && bitrate.declared) return { value: bitrate.declared, source: 'declared' };
  return null;
}
function bitrateTitle(source) {
  const lines = [`${BITRATE_SOURCES[source] || 'Débit'}${source === 'declared' ? '' : ' · moyenne sur quelques secondes'}`];
  if (source === 'stream' && bitrate.source === 'network') lines.push('Le cache est plein : le réseau est au repos.');
  if (bitrate.detail?.video) lines.push(`Vidéo : ${formatBitrate(bitrate.detail.video)}`);
  if (bitrate.detail?.audio) lines.push(`Audio : ${formatBitrate(bitrate.detail.audio)}`);
  if (bitrate.meter.total > 0) lines.push(`Données reçues : ${formatBytes(bitrate.meter.total)}`);
  return lines.join('\n');
}
function renderBitrate() {
  const shown = shownBitrate();
  const show = Boolean(bitrate.key) && Boolean(shown);
  const source = shown?.source;
  const text = show ? `${source === 'declared' ? '≈ ' : ''}${formatBitrate(shown.value)}` : '';
  const title = show ? bitrateTitle(source) : '';
  for (const id of ['bitrateLabel', 'bitrateBadge']) {
    const el = $(id);
    el.hidden = !show;
    if (!show) continue;
    el.lastChild.textContent = text;
    el.title = title;
    el.setAttribute('aria-label', `${BITRATE_SOURCES[source] || 'Débit'} : ${text}`);
  }
}
async function sampleBitrate() {
  const ctx = player.info;
  const current = media();
  if (!ctx?.plan || !current.getAttribute('src') || document.body.classList.contains('multiview-mode')) {
    if (bitrate.key) { bitrate.key = ''; bitrate.meter.reset(); renderBitrate(); }
    return;
  }
  if (bitrate.busy) return;
  const key = `${ctx.token}|${ctx.index}|${ctx.plan.mode}|${player.session || ''}|${current.getAttribute('src')}`;
  if (key !== bitrate.key) Object.assign(bitrate, { key, source: '', detail: null, declared: 0 }, bitrate.meter.reset());
  const local = String(ctx.url || '').startsWith('file:') || String(ctx.url || '').startsWith('/');
  bitrate.busy = true;
  try {
    const now = performance.now();
    if (player.engine === 'mpv') {
      const stats = await invoke('mpv_stats', { surface: 'main' });
      if (key !== bitrate.key) return;
      const stream = (stats.videoBitrate || 0) + (stats.audioBitrate || 0);
      bitrate.detail = { video: stats.videoBitrate, audio: stats.audioBitrate, dropped: stats.droppedFrames };
      if (!local && Number.isFinite(stats.cacheSpeed)) { bitrate.source = 'network'; bitrate.meter.rate(stats.cacheSpeed * 8, now); }
      else if (stream > 0) { bitrate.source = 'stream'; bitrate.meter.rate(stream, now); }
    } else if (player.session) {
      const status = await invoke('stream_status', { session: player.session });
      if (key !== bitrate.key) return;
      bitrate.source = ctx.plan.mode === 'transcode' ? 'converted' : 'network';
      bitrate.meter.counter(status.bytes, now);
    } else {
      const decoded = (video.webkitVideoDecodedByteCount || 0) + (video.webkitAudioDecodedByteCount || 0);
      const variant = quality.variants.find((item) => item.url === quality.selected);
      if (decoded > 0) { bitrate.source = local ? 'stream' : 'decoded'; bitrate.meter.counter(decoded, now); }
      else if (variant?.bandwidth) { bitrate.source = 'declared'; bitrate.declared = variant.bandwidth; }
    }
  } catch { /* engine switched or session closed: the next sample starts over */ } finally { bitrate.busy = false; }
  renderBitrate();
}
setInterval(sampleBitrate, 1000);

// ---------- Catalogue ----------

function rebuildCatalog() {
  const every = state.library.playlists.flatMap((playlist) => playlist.channels || []);
  const all = dedupeByUrl(every);
  catalog = { all, byUrl: new Map(all.map((channel) => [channel.streamUrl, channel])), byId: new Map(every.map((channel) => [channel.id, channel])) };
  catalogVersion += 1;
}
async function refreshLibrary() {
  state.library = await invoke('get_library');
  rebuildCatalog();
  render();
}
const VIDEO_FILE = /\.(mp4|m4v|mov|mkv|avi|webm|wmv|flv|mpe?g)(?:[?#]|$)/i;
/** Kind of a recent address that is no longer in a playlist: a file or a film can be sought. */
function recentKind(item) {
  if (item.url.includes('/episode/')) return 'episode';
  if (item.url.startsWith('file:') || (VIDEO_FILE.test(item.url) && !/\.m3u8?(?:[?#]|$)/i.test(item.url))) return 'movie';
  return 'live';
}
function sortedMedia(channels) { return [...channels].sort((a, b) => a.name.localeCompare(b.name, 'fr', { numeric: true, sensitivity: 'base' })); }
function currentChannels() {
  const { library } = state;
  switch (state.view) {
    case 'recent':
      return library.recent.map((item) => catalog.byUrl.get(item.url) || ({ id: `recent|${item.url}`, name: item.name, group: 'Récents', streamUrl: item.url, kind: recentKind(item), containerExtension: item.containerExtension }));
    case 'favorites':
      return library.favorites.map((id) => catalog.byId.get(id)).filter(Boolean);
    case 'continue':
      return library.progress.map((item) => ({ ...(catalog.byUrl.get(item.url) || { id: `progress|${item.url}`, name: item.name, group: 'À reprendre', streamUrl: item.url, kind: item.kind, containerExtension: item.containerExtension }), progress: item }));
    case 'all':
    case 'guide':
      return catalog.all;
    default:
      return library.playlists.find((item) => item.id === state.view)?.channels || [];
  }
}
function viewKeepsHidden() { return ['favorites', 'recent', 'continue'].includes(state.view); }
function filterOptions(overrides = {}) {
  return { group: state.group, query: state.query, country: state.country, language: state.language, hiddenGroups: state.library.hiddenGroups, hideOffline: state.hideOffline, health: state.health, keepHidden: viewKeepsHidden(), ...overrides };
}
function filteredChannels() { return filterChannels(currentChannels(), filterOptions()); }
function currentPlaylist() { return state.library.playlists.find((item) => item.id === state.view) || null; }
function viewTitle() {
  const titles = { all: 'Toutes les chaînes', guide: 'Guide TV', favorites: 'Favoris', recent: 'Récents', continue: 'Reprendre' };
  return titles[state.view] || state.library.playlists.find((item) => item.id === state.view)?.name || 'Playlist';
}
function activeFilterCount() { return [state.country, state.language, state.hideOffline].filter(Boolean).length + (state.library.hiddenGroups.length ? 1 : 0); }

// Channels of the view before the category and search filters: they give the category counts.
// Typing in the search box re-renders the list but leaves this list unchanged.
let catalogVersion = 0;
let healthVersion = 0;
const groupBaseCache = { key: '', list: [] };
function groupBase() {
  const compute = () => filterChannels(currentChannels(), filterOptions({ group: 'Tous', query: '' }));
  if (viewKeepsHidden()) return compute();
  const key = [state.view, catalogVersion, state.country, state.language, state.library.hiddenGroups.join('\u0001'), state.hideOffline ? healthVersion : 0].join('|');
  if (groupBaseCache.key !== key) Object.assign(groupBaseCache, { key, list: compute() });
  return groupBaseCache.list;
}

let lastList = [];
function render() {
  document.querySelectorAll('.main-nav button').forEach((el) => el.classList.toggle('active', el.dataset.view === state.view));
  const playlistNav = $('playlistNav'); playlistNav.replaceChildren();
  for (const playlist of state.library.playlists) {
    const row = make('div', `playlist-item${state.view === playlist.id ? ' active' : ''}`);
    const el = make('button', 'playlist-name', playlist.name);
    el.title = `${playlist.name}\nDouble-cliquez pour renommer · clic droit pour plus d’actions`; el.onclick = () => setView(playlist.id); el.ondblclick = () => renamePlaylist(playlist);
    const more = iconButton('ellipsis', `Actions pour ${playlist.name}`, 'playlist-more', (event) => { event.stopPropagation(); showPlaylistMenu(playlist, event.currentTarget); });
    more.setAttribute('aria-haspopup', 'menu');
    row.oncontextmenu = (event) => { event.preventDefault(); showPlaylistMenu(playlist, { x: event.clientX, y: event.clientY }); };
    row.append(el, more);
    playlistNav.append(row);
  }
  $('viewTitle').textContent = viewTitle();
  $('renameView').hidden = !currentPlaylist();
  $('exportView').hidden = !currentPlaylist();
  renderGroups(groupBase());
  const count = activeFilterCount();
  $('filtersButton').lastChild.textContent = count ? `Filtres · ${count}` : 'Filtres';
  $('filtersButton').classList.toggle('selected', count > 0);
  const channels = filteredChannels();
  lastList = channels;
  $('channelCount').textContent = channels.length.toLocaleString('fr-FR');
  $('channelCount').title = `${channels.length.toLocaleString('fr-FR')} élément${channels.length > 1 ? 's' : ''} affiché${channels.length > 1 ? 's' : ''}`;
  const guideView = state.view === 'guide';
  document.querySelector('.content').classList.toggle('guide-mode', guideView);
  $('channels').hidden = guideView;
  $('guideGrid').hidden = !guideView;
  if (guideView) renderGuide(channels);
  else renderList(channels);
  updateFavoriteButton();
  $('guideStatus').textContent = state.epg.loading ? 'Chargement du guide…' : state.epg.programs ? `${state.epg.programs.toLocaleString('fr-FR')} programmes` : 'Ajoutez un guide XMLTV';
}

function renderList(channels) {
  const list = $('channels'); const previousScroll = list.scrollTop; list.replaceChildren(); list.onscroll = null;
  if (!channels.length) {
    const empty = make('div', 'list-empty');
    const hasPlaylists = state.library.playlists.length > 0;
    const emptyText = state.view === 'continue' ? ['play-circle', 'Rien à reprendre', 'Les films, épisodes et vidéos commencés apparaîtront ici.']
      : hasPlaylists ? ['search', 'Aucun résultat', 'Essayez une autre recherche, un autre groupe ou retirez des filtres.']
        : ['plus', 'Ajoutez votre première playlist', 'Importez un fichier M3U ou collez son adresse pour afficher vos chaînes.'];
    empty.append(emptyIcon(emptyText[0]), make('strong', '', emptyText[1]), make('p', '', emptyText[2]));
    if (!hasPlaylists) empty.append(button('Ajouter une playlist', 'primary', showAddPlaylist));
    list.append(empty);
    return;
  }
  let cursor = 0;
  const appendBatch = (size = 120) => {
    const fragment = document.createDocumentFragment();
    const batch = channels.slice(cursor, cursor + size);
    batch.forEach((channel, offset) => fragment.append(channelRow(channel, cursor + offset)));
    cursor = Math.min(cursor + size, channels.length);
    list.append(fragment);
    requestNowNext(batch);
    autoCheck(batch);
  };
  appendBatch(Math.max(120, Math.ceil((previousScroll + list.clientHeight + 300) / 64 / 120) * 120));
  list.scrollTop = previousScroll;
  list.onscroll = () => { if (cursor < channels.length && list.scrollHeight - list.scrollTop - list.clientHeight < 300) appendBatch(); };
}

function channelRow(channel, index) {
  const row = make('div', `channel-row${state.playing?.url === channel.streamUrl ? ' is-playing' : ''}`);
  row.dataset.url = channel.streamUrl;
  row.dataset.id = channel.id;
  const play = make('button', 'channel-main');
  const logo = make('span', 'channel-logo', channel.name.slice(0, 1).toUpperCase());
  if (channel.logo?.startsWith('https://') || channel.logo?.startsWith('http://')) {
    const image = document.createElement('img'); image.src = channel.logo; image.alt = ''; image.loading = 'lazy'; image.onerror = () => image.remove(); logo.prepend(image);
  }
  const text = make('span', 'channel-text');
  const detail = [channel.group, channel.country ? flag(channel.country) : '', channel.catchupDays ? `replay ${channel.catchupDays} j` : ''].filter(Boolean).join(' · ');
  text.append(make('strong', '', channel.name), make('small', '', detail));
  if (channel.progress) {
    const percent = channel.progress.duration ? Math.round(channel.progress.position / channel.progress.duration * 100) : 0;
    text.append(make('small', 'channel-now', `Reprendre à ${timeText(channel.progress.position)} · ${percent} %`));
  } else text.append(make('small', 'channel-now'));
  play.append(make('span', 'channel-number', String(index + 1)), logo, text);
  play.onclick = () => (document.body.classList.contains('multiview-mode') ? multiview.add(channel) : playChannel(channel));
  row.append(play);
  const health = state.health[channel.streamUrl];
  if (health) {
    const dot = make('span', `health-dot ${health.ok ? 'ok' : 'bad'}`);
    dot.title = health.ok ? 'Chaîne disponible lors de la dernière vérification' : health.message || 'Chaîne indisponible';
    row.append(dot);
  }
  if (state.view === 'favorites') {
    row.append(iconButton('arrow-up', 'Monter', 'row-action', () => moveFavorite(channel.id, -1)), iconButton('arrow-down', 'Descendre', 'row-action', () => moveFavorite(channel.id, 1)));
  }
  if (state.view === 'continue') {
    row.append(iconButton('x', 'Retirer de la liste', 'row-action', async () => { state.library.progress = await invoke('clear_progress', { url: channel.streamUrl }); render(); }));
  }
  if (!['recent', 'continue'].includes(state.view)) {
    const favorite = iconButton('star', 'Favori', 'star', () => toggleFavorite(channel.id));
    favorite.classList.toggle('on', state.library.favorites.includes(channel.id));
    favorite.setAttribute('aria-pressed', String(favorite.classList.contains('on')));
    row.append(favorite);
  }
  const cached = nowNextCache.get(channel.id);
  if (cached?.value) fillNowNext(row, cached.value);
  return row;
}

function markPlayingRows() {
  document.querySelectorAll('.channel-row').forEach((row) => row.classList.toggle('is-playing', row.dataset.url === state.playing?.url));
}

function setView(view) {
  closeGroupPanel();
  state.view = view; state.group = 'Tous'; state.query = ''; $('search').value = ''; $('channels').scrollTop = 0; render();
}

// ---------- Catégories ----------
// A searchable picker replaces the endless row of chips; pinned and recent categories stay
// one click away. Clicking the active category again returns to « Toutes ».

const groupPanel = { open: false, active: -1, items: [], counts: new Map(), total: 0 };
function selectGroup(group) {
  const next = group === state.group ? 'Tous' : group;
  state.group = next;
  if (next !== 'Tous') {
    preferences.recentGroups = [next, ...preferences.recentGroups.filter((item) => item !== next)].slice(0, 6);
    savePreferences();
  }
  closeGroupPanel();
  $('channels').scrollTop = 0; render();
}
function togglePinnedGroup(group) {
  const pinned = preferences.pinnedGroups;
  preferences.pinnedGroups = pinned.includes(group) ? pinned.filter((item) => item !== group) : [...pinned, group];
  savePreferences();
  render();
}
function renderGroups(base) {
  const counts = new Map();
  for (const channel of base) counts.set(channel.group, (counts.get(channel.group) || 0) + 1);
  Object.assign(groupPanel, { counts, total: base.length });
  if (state.group !== 'Tous' && !counts.has(state.group)) state.group = 'Tous';
  const all = state.group === 'Tous';
  const useful = counts.size > 1;
  $('groupPicker').hidden = !useful;
  $('groupPicker').classList.toggle('selected', !all);
  $('groupPickerLabel').textContent = all ? 'Catégories' : state.group;
  $('groupPickerCount').textContent = all ? `${counts.size}` : `${counts.get(state.group)}`;
  $('groupPicker').title = all ? `${counts.size} catégories · cliquez pour choisir` : `${state.group} · cliquez pour changer`;
  $('groupClear').hidden = all;
  const pinned = preferences.pinnedGroups.filter((group) => counts.has(group));
  const recent = preferences.recentGroups.filter((group) => counts.has(group) && !pinned.includes(group));
  const quick = [...pinned, ...recent.slice(0, Math.max(0, 8 - pinned.length))];
  if (!all && !quick.includes(state.group)) quick.unshift(state.group);
  const chips = $('groupFilters'); chips.replaceChildren();
  chips.hidden = !useful || !quick.length;
  for (const group of quick) {
    const active = state.group === group;
    const chip = make('button', `${active ? 'selected' : ''}${pinned.includes(group) ? ' pinned' : ''}`, group);
    chip.title = active ? `${group} · cliquez à nouveau pour tout afficher` : `${group} · ${counts.get(group)} élément${counts.get(group) > 1 ? 's' : ''}`;
    chip.setAttribute('aria-pressed', String(active));
    chip.onclick = () => selectGroup(group);
    chips.append(chip);
  }
  if (groupPanel.open) drawGroupPanel();
}
function groupOption(group, count, label = group) {
  const option = make('div', `group-option${state.group === group ? ' selected' : ''}`);
  option.setAttribute('role', 'option');
  option.setAttribute('aria-selected', String(state.group === group));
  const main = make('button', 'group-option-main');
  main.tabIndex = -1;
  main.append(make('span', 'group-option-name', label), make('span', 'group-option-count', count.toLocaleString('fr-FR')));
  main.onclick = () => selectGroup(group);
  option.append(main);
  if (group !== 'Tous') {
    const isPinned = preferences.pinnedGroups.includes(group);
    const pin = iconButton(isPinned ? 'pin-off' : 'pin', isPinned ? 'Retirer des raccourcis' : 'Épingler dans les raccourcis', `group-pin${isPinned ? ' on' : ''}`, (event) => { event.stopPropagation(); togglePinnedGroup(group); $('groupSearch').focus(); });
    pin.tabIndex = -1;
    option.append(pin);
  }
  groupPanel.items.push({ group, el: option });
  return option;
}
function drawGroupPanel() {
  const list = $('groupList'); list.replaceChildren(); groupPanel.items = [];
  document.querySelectorAll('.group-sort button').forEach((el) => el.classList.toggle('selected', el.dataset.sort === preferences.groupSort));
  const needle = normalizeQuery($('groupSearch').value);
  let groups = [...groupPanel.counts];
  if (preferences.groupSort === 'az') groups.sort((a, b) => a[0].localeCompare(b[0], 'fr', { numeric: true, sensitivity: 'base' }));
  else if (preferences.groupSort === 'count') groups.sort((a, b) => b[1] - a[1]);
  // Every word must appear, in any order: « belg sport » finds « Belgique | Sport ».
  const words = needle.split(/\s+/).filter(Boolean);
  if (words.length) groups = groups.filter(([group]) => { const name = normalizeQuery(group); return words.every((word) => name.includes(word)); });
  if (!needle) list.append(groupOption('Tous', groupPanel.total, 'Toutes les catégories'));
  const pinned = needle ? [] : groups.filter(([group]) => preferences.pinnedGroups.includes(group));
  if (pinned.length) {
    list.append(make('div', 'group-section', 'Épinglées'));
    for (const [group, count] of pinned) list.append(groupOption(group, count));
    list.append(make('div', 'group-section', `Toutes · ${groups.length}`));
  }
  for (const [group, count] of groups) list.append(groupOption(group, count));
  if (!groups.length) list.append(make('p', 'group-empty', 'Aucune catégorie ne correspond.'));
  const selected = groupPanel.items.findIndex((item) => item.group === state.group);
  setGroupActive(needle ? 0 : Math.max(0, selected), !needle);
}
function setGroupActive(index, center = false) {
  const { items } = groupPanel;
  if (!items.length) { groupPanel.active = -1; return; }
  groupPanel.active = Math.max(0, Math.min(items.length - 1, index));
  items.forEach((item, i) => item.el.classList.toggle('active', i === groupPanel.active));
  items[groupPanel.active].el.scrollIntoView({ block: center ? 'center' : 'nearest' });
}
function openGroupPanel() {
  if (groupPanel.open) { closeGroupPanel(true); return; }
  groupPanel.open = true;
  $('groupPanel').hidden = false;
  $('groupPicker').setAttribute('aria-expanded', 'true');
  $('groupSearch').value = '';
  drawGroupPanel();
  $('groupSearch').focus();
}
function closeGroupPanel(focusPicker = false) {
  if (!groupPanel.open) return;
  groupPanel.open = false;
  $('groupPanel').hidden = true;
  $('groupPicker').setAttribute('aria-expanded', 'false');
  if (focusPicker) $('groupPicker').focus();
}
$('groupPicker').onclick = openGroupPanel;
$('groupClear').onclick = () => selectGroup(state.group);
$('groupSearch').oninput = drawGroupPanel;
$('groupSearch').onkeydown = (event) => {
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') { event.preventDefault(); setGroupActive(groupPanel.active + (event.key === 'ArrowDown' ? 1 : -1)); }
  else if (event.key === 'PageDown' || event.key === 'PageUp') { event.preventDefault(); setGroupActive(groupPanel.active + (event.key === 'PageDown' ? 10 : -10)); }
  else if (event.key === 'Enter') { event.preventDefault(); const item = groupPanel.items[groupPanel.active]; if (item) selectGroup(item.group); }
  else if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); closeGroupPanel(true); }
};
document.querySelectorAll('.group-sort button').forEach((el) => {
  el.onclick = () => { preferences.groupSort = el.dataset.sort; savePreferences(); drawGroupPanel(); $('groupSearch').focus(); };
});
document.addEventListener('pointerdown', (event) => {
  if (groupPanel.open && !$('groupPanel').contains(event.target) && !$('groupPicker').contains(event.target)) closeGroupPanel();
});

// ---------- Maintenant / à suivre ----------

const nowNextCache = new Map();
let nowNextPending = new Map();
let nowNextTimer = 0;
function fillNowNext(row, value) {
  const el = row.querySelector('.channel-now');
  if (!el || !value) return;
  const now = value.now ? `▶ ${value.now.title} · jusqu’à ${clock(value.now.stop)}` : '';
  const next = value.next ? `${now ? ' · ' : 'À suivre : '}${clock(value.next.start)} ${value.next.title}` : '';
  el.textContent = `${now}${!now ? next : ''}`;
  el.title = [value.now && `Maintenant : ${value.now.title}`, value.next && `À ${clock(value.next.start)} : ${value.next.title}`].filter(Boolean).join('\n');
}
function requestNowNext(channels) {
  if (!state.epg.programs) return;
  const now = Date.now();
  for (const channel of channels) {
    if (channel.kind && channel.kind !== 'live') continue;
    const cached = nowNextCache.get(channel.id);
    if (cached && now - cached.at < 60_000 && (!cached.value?.now || cached.value.now.stop * 1000 > now)) continue;
    nowNextPending.set(channel.id, epgRef(channel));
  }
  if (!nowNextPending.size || nowNextTimer) return;
  nowNextTimer = setTimeout(async () => {
    nowNextTimer = 0;
    const references = [...nowNextPending.values()];
    nowNextPending = new Map();
    try {
      const result = await invoke('get_now_next', { references });
      const at = Date.now();
      // Long browsing sessions would otherwise keep an entry for every channel ever shown.
      if (nowNextCache.size > 6000) [...nowNextCache.keys()].slice(0, 2000).forEach((key) => nowNextCache.delete(key));
      for (const reference of references) {
        const value = result[reference.key] || null;
        nowNextCache.set(reference.key, { at, value });
        if (!value) continue;
        const row = document.querySelector(`.channel-row[data-id="${CSS.escape(reference.key)}"]`);
        if (row) fillNowNext(row, value);
      }
    } catch { /* guide not ready */ }
  }, 120);
}

// ---------- Disponibilité ----------

const checkedThisSession = new Set();
function checkItems(channels, max) {
  const stale = Math.floor(Date.now() / 1000) - 12 * 3600;
  const items = [];
  for (const channel of channels) {
    if (items.length >= max) break;
    if (!isWebUrl(channel.streamUrl) || isYouTubePage(channel.streamUrl) || checkedThisSession.has(channel.streamUrl)) continue;
    if ((state.health[channel.streamUrl]?.checkedAt || 0) > stale) continue;
    checkedThisSession.add(channel.streamUrl);
    items.push({ url: channel.streamUrl, userAgent: channel.userAgent || null, referrer: channel.referrer || null });
  }
  return items;
}
function autoCheck(channels) {
  if (!preferences.autoCheck) return;
  const items = checkItems(channels, 40);
  if (items.length) invoke('check_channels', { items }).catch(() => {});
}
async function checkVisibleChannels() {
  const items = lastList.filter((channel) => isWebUrl(channel.streamUrl) && !isYouTubePage(channel.streamUrl)).slice(0, 1000)
    .map((channel) => { checkedThisSession.add(channel.streamUrl); return { url: channel.streamUrl, userAgent: channel.userAgent || null, referrer: channel.referrer || null }; });
  if (!items.length) { toast('Aucune chaîne en ligne à vérifier dans cette liste.'); return; }
  const count = await invoke('check_channels', { items });
  toast(`Vérification de ${count || items.length} chaîne${items.length > 1 ? 's' : ''} en arrière-plan…`);
}
let healthRenderTimer = 0;
listen('health', (event) => {
  for (const item of event.payload) state.health[item.url] = { ok: item.ok, message: item.message, checkedAt: item.checkedAt };
  healthVersion += 1;
  if (!healthRenderTimer) {
    healthRenderTimer = setTimeout(() => {
      healthRenderTimer = 0;
      if (state.hideOffline) { render(); return; }
      document.querySelectorAll('.channel-row').forEach((row) => {
        const entry = state.health[row.dataset.url];
        if (!entry) return;
        let dot = row.querySelector('.health-dot');
        if (!dot) { dot = make('span', 'health-dot'); row.querySelector('.channel-main').after(dot); }
        dot.className = `health-dot ${entry.ok ? 'ok' : 'bad'}`;
        dot.title = entry.ok ? 'Chaîne disponible lors de la dernière vérification' : entry.message || 'Chaîne indisponible';
      });
    }, 1200);
  }
}).catch(() => {});
listen('health-done', () => { if (state.hideOffline) render(); }).catch(() => {});

// ---------- Favoris ----------

function updateFavoriteButton() {
  $('favoriteButton').hidden = !state.playing?.channelId;
  if (state.playing?.channelId) {
    const on = state.library.favorites.includes(state.playing.channelId);
    $('favoriteButton').classList.toggle('on', on);
    $('favoriteButton').title = on ? 'Retirer des favoris' : 'Ajouter aux favoris';
    $('favoriteButton').setAttribute('aria-label', $('favoriteButton').title);
  }
}
async function toggleFavorite(id) {
  try {
    state.library.favorites = await invoke('toggle_favorite', { id });
    if (state.view === 'favorites') { render(); return; }
    const on = state.library.favorites.includes(id);
    document.querySelectorAll(`.channel-row[data-id="${CSS.escape(id)}"] .star`).forEach((el) => { el.classList.toggle('on', on); el.setAttribute('aria-pressed', String(on)); });
    updateFavoriteButton();
  } catch (error) { toast(errorMessage(error), 'error'); }
}
async function moveFavorite(id, delta) {
  const favorites = [...state.library.favorites];
  const index = favorites.indexOf(id);
  const target = index + delta;
  if (index < 0 || target < 0 || target >= favorites.length) return;
  [favorites[index], favorites[target]] = [favorites[target], favorites[index]];
  try { state.library.favorites = await invoke('reorder_favorites', { ids: favorites }); render(); }
  catch (error) { toast(errorMessage(error), 'error'); }
}

// ---------- Lecture d’une chaîne ----------

function renderQueue() {
  $('queueCard').hidden = state.queue.length === 0;
  $('queueCount').textContent = `${state.queue.length} média${state.queue.length > 1 ? 's' : ''}`;
  $('queuePrevious').disabled = state.queueIndex <= 0;
  $('queueNext').disabled = state.queueIndex < 0 || state.queueIndex >= state.queue.length - 1;
  const list = $('queueItems'); list.replaceChildren();
  state.queue.forEach((item, index) => {
    const row = make('button', `queue-item${index === state.queueIndex ? ' active' : ''}`);
    row.append(make('span', 'queue-number', String(index + 1)), make('span', 'queue-name', item.name));
    row.title = item.name; row.onclick = () => playQueueIndex(index); list.append(row);
  });
}
function clearQueue() { state.queue = []; state.queueIndex = -1; renderQueue(); }
async function playQueueIndex(index) {
  if (index < 0 || index >= state.queue.length) return;
  state.queueIndex = index; renderQueue();
  await playChannel(state.queue[index], { fromQueue: true });
}

async function playChannel(channel, { fromQueue = false, catchup = null } = {}) {
  try {
    if (!fromQueue) {
      if (channel.streamUrl.startsWith('file:')) {
        const local = state.library.playlists.find((playlist) => playlist.id === 'local-media')?.channels || [];
        state.queue = sortedMedia(local.filter((item) => item.group === channel.group));
        state.queueIndex = state.queue.findIndex((item) => item.streamUrl === channel.streamUrl);
        renderQueue();
      } else clearQueue();
    }
    if (channel.kind === 'series') { await showEpisodes(channel); return; }
    saveProgressNow();
    if (state.playing?.channel && state.playing.url !== channel.streamUrl && !state.playing.catchup) state.previous = state.playing.channel;
    $('videoEmpty').hidden = true;
    if (isYouTubePage(channel.streamUrl)) {
      player.stop(); resetQuality();
      state.playing = { url: channel.streamUrl, name: channel.name, channelId: channel.id, tvgId: channel.tvgId, channel, kind: channel.kind };
      $('playingTitle').textContent = channel.name; $('playingDetail').textContent = channel.streamUrl;
      $('liveIndicator').textContent = 'DIRECT'; $('engineBadge').hidden = true; $('streamInfo').hidden = true;
      $('playerError').textContent = ''; $('playerStatus').textContent = '';
      $('videoWrap').classList.add('youtube-source'); youtubePlayback.hidden = false;
      updateFavoriteButton(); markPlayingRows(); renderPrograms(channel);
      await openCurrentYouTube();
    } else {
      if (youtubeSession || !youtubePlayback.hidden) await hideYoutube();
      const alternatives = catchup ? [channel] : alternativesFor(channel, catalog.all, state.health);
      await player.play({ alternatives, catchup });
    }
    if (!catchup) {
      invoke('record_recent', { name: channel.name, url: channel.streamUrl, extension: channel.containerExtension || null })
        .then((recent) => { state.library.recent = recent; if (state.view === 'recent') render(); })
        .catch((error) => toast(errorMessage(error), 'error'));
    }
  } catch (error) { toast(errorMessage(error), 'error'); }
}

async function playUrl(url, name) {
  clearQueue();
  const channel = catalog.byUrl.get(url) || { id: `recent|${url}`, name, group: 'Adresse', streamUrl: url, kind: /\.m3u8(?:[?#]|$)/i.test(url) ? 'live' : 'movie' };
  await playChannel(channel, { fromQueue: true });
}

function zap(delta) {
  const list = lastList.filter((channel) => channel.kind !== 'series');
  if (!list.length) return;
  const index = list.findIndex((channel) => channel.streamUrl === state.playing?.url);
  const next = index < 0 ? (delta > 0 ? 0 : list.length - 1) : (index + delta + list.length) % list.length;
  showZap(`${next + 1}  ${list[next].name}`);
  playChannel(list[next]);
}
let zapDigits = '';
let zapTimer = 0;
let zapHideTimer = 0;
function showZap(text) {
  $('zapOverlay').textContent = text; $('zapOverlay').hidden = false;
  clearTimeout(zapHideTimer); zapHideTimer = setTimeout(() => { $('zapOverlay').hidden = true; }, 1800);
}
function zapDigit(digit) {
  zapDigits = `${zapDigits}${digit}`.slice(-4);
  showZap(zapDigits);
  clearTimeout(zapTimer);
  zapTimer = setTimeout(() => {
    const number = Number(zapDigits); zapDigits = '';
    const channel = lastList[number - 1];
    if (channel) { showZap(`${number}  ${channel.name}`); playChannel(channel); }
    else showZap(`${number} : aucune chaîne`);
  }, 1300);
}

// ---------- Reprise de lecture ----------

let progressTimer = 0;
function saveProgressNow() {
  const playing = state.playing;
  const ctx = player.info;
  const current = media();
  if (!playing || !ctx || ctx.live || playing.catchup || !Number.isFinite(current.duration) || current.duration < 120 || current.currentTime < 1) return;
  invoke('save_progress', { item: { url: playing.url, name: playing.name, position: current.currentTime, duration: current.duration, updatedAt: '', kind: playing.kind || 'movie', containerExtension: playing.channel?.containerExtension || null } })
    .then((progress) => { state.library.progress = progress; if (state.view === 'continue') render(); })
    .catch(() => {});
}
for (const target of [video, mainMpv]) {
  target.addEventListener('timeupdate', () => {
    const now = Date.now();
    if (target === media() && now - progressTimer > 10_000) { progressTimer = now; saveProgressNow(); }
  });
  target.addEventListener('pause', () => { if (target === media()) saveProgressNow(); });
  target.addEventListener('ended', () => { if (target === media()) saveProgressNow(); });
}

// ---------- Guide TV ----------

async function renderPrograms(channel) {
  const box = $('programs'); box.replaceChildren();
  if (!state.epg.programs) {
    box.append(make('p', 'muted', state.epg.loading ? 'Chargement du guide TV…' : 'Ajoutez un guide XMLTV ou une playlist qui en fournit un.'));
    return;
  }
  if (!channel || (channel.kind && channel.kind !== 'live')) { box.append(make('p', 'muted', 'Pas de programme pour ce média.')); return; }
  try {
    const now = Math.floor(Date.now() / 1000);
    const programs = await invoke('get_programs', { reference: epgRef(channel), from: now, to: now + 86_400 });
    if (state.playing?.channel !== channel && state.playing?.url !== channel.streamUrl) return;
    if (!programs.length) box.append(make('p', 'muted', 'Aucun programme trouvé pour cette chaîne dans le guide.'));
    for (const program of programs.slice(0, 20)) {
      const row = make('div', `program${program.start <= now ? ' current' : ''}`);
      row.append(make('span', 'program-time', clock(program.start)), make('div', 'program-text'));
      row.lastChild.append(make('strong', '', program.title), make('small', '', program.description));
      box.append(row);
    }
    if (channel.catchupDays && channel.streamUrl.startsWith('xtream://')) {
      const past = await invoke('get_programs', { reference: epgRef(channel), from: now - Math.min(channel.catchupDays, 2) * 86_400, to: now });
      const finished = past.filter((program) => program.stop <= now).slice(-15).reverse();
      if (finished.length) {
        box.append(make('h4', 'programs-heading', 'Replay'));
        for (const program of finished) {
          const row = make('div', 'program replayable');
          const day = new Date(program.start * 1000).toLocaleDateString('fr-FR', { weekday: 'short' });
          row.append(make('span', 'program-time', `${day} ${clock(program.start)}`), make('div', 'program-text'), withIcon(button('Revoir', 'subtle small', () => playCatchup(channel, program)), 'play'));
          row.children[1].append(make('strong', '', program.title), make('small', '', program.description));
          box.append(row);
        }
      }
    }
  } catch (error) { box.append(make('p', 'muted', errorMessage(error))); }
}

function canReplay(channel, program) {
  const now = Date.now() / 1000;
  return Boolean(channel.catchupDays && channel.streamUrl.startsWith('xtream://') && now - program.start < channel.catchupDays * 86_400);
}
function playCatchup(channel, program) {
  clearQueue();
  playChannel(channel, { fromQueue: true, catchup: { start: program.start, stop: program.stop, title: program.title } });
}

let guideRequest = 0;
async function renderGuide(channels) {
  const container = $('guideGrid');
  const request = ++guideRequest;
  if (!state.epg.programs) {
    container.replaceChildren(make('div', 'list-empty'));
    container.firstChild.append(emptyIcon('guide'), make('strong', '', state.epg.loading ? 'Chargement du guide…' : 'Aucun guide TV'), make('p', '', 'Ajoutez un guide XMLTV dans Sources & guide TV, ou importez une playlist qui en annonce un.'));
    return;
  }
  const now = Math.floor(Date.now() / 1000);
  const { from, to } = gridWindow(now, 6);
  const live = channels.filter((channel) => !channel.kind || channel.kind === 'live').slice(0, 300);
  const result = await invoke('get_guide', { references: live.map(epgRef), from, to }).catch(() => ({}));
  if (request !== guideRequest) return;
  const rows = live.filter((channel) => result[channel.id]).slice(0, 150).map((channel) => ({ channel, programs: result[channel.id] }));
  if (!rows.length) {
    container.replaceChildren(make('div', 'list-empty'));
    container.firstChild.append(emptyIcon('guide'), make('strong', '', 'Aucun programme pour ces chaînes'), make('p', '', 'Le guide chargé ne couvre pas les chaînes de cette sélection. Essayez un autre groupe ou pays.'));
    return;
  }
  renderGuideGrid(container, {
    rows, from, to, now, canReplay,
    onChannel: (channel) => playChannel(channel),
    onProgram: (channel, program, when) => {
      if (when === 'live') playChannel(channel);
      else if (when === 'past' && canReplay(channel, program)) playCatchup(channel, program);
      else if (when === 'past') toast('Ce programme est terminé et la chaîne ne propose pas de replay.');
      else toast(`« ${program.title} » commence à ${clock(program.start)}.`);
    },
  });
}

async function refreshEpgStatus() {
  try {
    const status = await invoke('epg_status');
    state.epg = status;
    nowNextCache.clear();
    render();
    if (state.playing?.channel) renderPrograms(state.playing.channel);
  } catch { /* backend not ready */ }
}
listen('epg-loading', () => { state.epg.loading = true; $('guideStatus').textContent = 'Chargement du guide…'; }).catch(() => {});
listen('epg-updated', () => refreshEpgStatus()).catch(() => {});

// ---------- Séries ----------

async function showEpisodes(channel) {
  modal(channel.name, (body) => body.append(make('p', 'muted', 'Chargement des épisodes…')));
  try {
    const episodes = await invoke('get_series_episodes', { reference: channel.streamUrl });
    const body = $('modalBody'); body.replaceChildren();
    if (!episodes.length) { body.append(make('p', 'muted', 'Aucun épisode disponible.')); return; }
    for (const episode of episodes) {
      const saved = state.library.progress.find((item) => item.url === episode.streamUrl);
      const label = `Saison ${episode.season} · ${episode.name}${saved ? ` · reprendre à ${timeText(saved.position)}` : ''}`;
      body.append(button(label, 'episode-button', async () => {
        closeModal();
        await playChannel({ id: episode.streamUrl, name: episode.name, group: channel.group, streamUrl: episode.streamUrl, kind: 'episode', containerExtension: episode.extension });
      }));
    }
  } catch (error) { $('modalBody').replaceChildren(make('p', 'muted', errorMessage(error))); }
}

// ---------- Fenêtres modales ----------

function modal(title, build, { wide = false } = {}) {
  closePopupMenu();
  $('modalTitle').textContent = title;
  document.querySelector('.modal').classList.toggle('wide', wide);
  const body = $('modalBody'); body.replaceChildren(); build(body);
  $('modal').hidden = false; scheduleYoutubeBounds();
  body.querySelector('input:not([type=checkbox])')?.focus();
}
function closeModal() { $('modal').hidden = true; scheduleYoutubeBounds(); }
function field(labelText, placeholder, value = '') { const label = make('label', 'field'); label.append(make('span', '', labelText)); const input = document.createElement('input'); input.placeholder = placeholder; input.value = value; label.append(input); return { label, input }; }
let pasteFieldCount = 0;
function pasteField(labelText, placeholder, value = '') {
  const container = make('div', 'field');
  const label = make('label', '', labelText);
  const input = document.createElement('input');
  input.id = `pasteField${++pasteFieldCount}`;
  input.placeholder = placeholder;
  input.value = value;
  label.htmlFor = input.id;
  const paste = button('Coller', 'subtle', async () => {
    try {
      const text = (await readText()).trim();
      if (!text) { toast('Le presse-papiers ne contient pas de texte.', 'error'); return; }
      input.value = text;
      input.dispatchEvent(new Event('input', { bubbles: true }));
      input.focus();
    } catch (error) { toast(`Impossible de coller : ${errorMessage(error)}`, 'error'); }
  });
  paste.setAttribute('aria-label', `Coller dans ${labelText}`);
  const row = make('div', 'paste-field-row'); row.append(input, paste);
  container.append(label, row);
  return { label: container, input };
}
function checkboxField(labelText, checked, onChange) {
  const label = make('label', 'check-field');
  const input = document.createElement('input'); input.type = 'checkbox'; input.checked = checked;
  input.onchange = () => onChange(input.checked);
  label.append(input, make('span', '', labelText));
  return label;
}
function selectField(labelText, options, selected, onChange) {
  const label = make('label', 'field'); label.append(make('span', '', labelText));
  const select = document.createElement('select');
  for (const [value, text] of options) { const option = document.createElement('option'); option.value = value; option.textContent = text; select.append(option); }
  select.value = selected; select.onchange = () => onChange(select.value); label.append(select); return label;
}
function rangeField(labelText, min, max, value, suffix, onChange) {
  const label = make('label', 'field'); const caption = make('span', '', `${labelText} · ${value}${suffix}`);
  const input = document.createElement('input'); input.type = 'range'; input.min = min; input.max = max; input.value = value;
  input.oninput = () => { caption.textContent = `${labelText} · ${input.value}${suffix}`; onChange(Number(input.value)); };
  label.append(caption, input); return label;
}

function showAddPlaylist() {
  modal('Ajouter une playlist', (body) => {
    body.append(make('p', 'modal-intro', 'Collez l’adresse d’une playlist M3U ou choisissez un fichier local. Les en-têtes HTTP (User-Agent, Referer) et le guide annoncés par la playlist sont repris automatiquement.'));
    const name = field('Nom', 'Ex. Mes chaînes'); const source = pasteField('Adresse M3U ou fichier', 'https://…/playlist.m3u');
    body.append(name.label, source.label);
    body.append(button('Choisir un fichier M3U', 'subtle full', async () => { const path = await open({ multiple: false, filters: [{ name: 'Playlist M3U', extensions: ['m3u', 'm3u8'] }] }); if (path) { source.input.value = path; if (!name.input.value) name.input.value = path.split('/').pop().replace(/\.m3u8?$/i, ''); } }));
    body.append(button('Importer la playlist', 'primary full', async (event) => {
      const target = event.currentTarget; target.disabled = true; target.textContent = 'Importation…';
      try {
        const playlist = await invoke('add_playlist', { name: name.input.value, source: source.input.value });
        closeModal(); await refreshLibrary(); setView(playlist.id);
        toast(`${playlist.channels.length} chaînes importées.${playlist.epgUrl ? ' Guide TV de la playlist en cours de chargement.' : ''}`);
      } catch (error) { toast(errorMessage(error), 'error'); }
      finally { target.disabled = false; target.textContent = 'Importer la playlist'; }
    }));
  });
}
function showAddXtream() {
  modal('Connecter Xtream Codes', (body) => {
    body.append(make('p', 'modal-intro', 'Renseignez les identifiants fournis par votre service. Le mot de passe est conservé dans le trousseau macOS. Le guide TV et le replay du service sont activés automatiquement.'));
    const name = field('Nom du compte', 'Ex. Mon service');
    const server = pasteField('Serveur', 'https://exemple.com:8080');
    const username = field('Utilisateur', 'Identifiant');
    const password = field('Mot de passe', 'Mot de passe'); password.input.type = 'password';
    body.append(name.label, server.label, username.label, password.label);
    body.append(button('Connecter le compte', 'primary full', async (event) => {
      const target = event.currentTarget; target.disabled = true; target.textContent = 'Connexion…';
      try {
        const playlist = await invoke('add_xtream_account', { name: name.input.value, server: server.input.value, username: username.input.value, password: password.input.value });
        password.input.value = ''; closeModal(); await refreshLibrary(); setView(playlist.id);
        toast(`${playlist.channels.length} médias importés.`);
      } catch (error) { toast(errorMessage(error), 'error'); }
      finally { target.disabled = false; target.textContent = 'Connecter le compte'; }
    }));
  });
}
function showPlayUrl() {
  modal('Lire une URL', (body) => {
    body.append(make('p', 'modal-intro', 'Ouvrez un flux HLS, MPEG-TS, une vidéo, un fichier audio ou une playlist M3U en ligne.'));
    const source = pasteField('Adresse du média', 'https://…/video.m3u8'); body.append(source.label);
    source.input.onkeydown = (event) => { if (event.key === 'Enter') { event.preventDefault(); body.querySelector('.primary')?.click(); } };
    body.append(button('Ouvrir l’adresse', 'primary full', async (event) => {
      const value = source.input.value.trim();
      if (!/^https?:\/\//i.test(value)) { toast('Entrez une adresse HTTP ou HTTPS.', 'error'); return; }
      const target = event.currentTarget; target.disabled = true; target.textContent = 'Ouverture…';
      try {
        const url = new URL(value);
        if (/\.m3u8?$/i.test(url.pathname)) {
          const filename = url.pathname.split('/').pop();
          const name = /^playlist\.m3u8?$/i.test(filename) ? 'Playlist IPTV' : filename.replace(/\.m3u8?$/i, '');
          const playlist = await invoke('add_playlist_if_catalog', { name, source: value });
          if (playlist) {
            closeModal(); await refreshLibrary(); setView(playlist.id);
            toast(`${playlist.channels.length} chaînes importées.`);
            return;
          }
        }
        closeModal(); await playUrl(value, url.pathname.split('/').pop() || 'Média en ligne');
      } catch (error) { toast(errorMessage(error), 'error'); }
      finally { target.disabled = false; target.textContent = 'Ouvrir l’adresse'; }
    }));
  });
}

function renamePlaylist(playlist, after) {
  modal('Renommer la playlist', (body) => {
    const name = field('Nom', playlist.name, playlist.name); body.append(name.label);
    const save = async () => {
      const value = name.input.value.trim();
      if (!value) { toast('Indiquez un nom.', 'error'); return; }
      if (value === playlist.name) { closeModal(); after?.(); return; }
      try {
        await invoke('rename_playlist', { id: playlist.id, name: value });
        playlist.name = value; closeModal(); render(); toast('Playlist renommée.'); after?.();
      } catch (error) { toast(errorMessage(error), 'error'); }
    };
    name.input.onkeydown = (event) => { if (event.key === 'Enter') { event.preventDefault(); save(); } };
    body.append(button('Renommer', 'primary full', save));
  });
  $('modalBody').querySelector('input')?.select();
}

// ---------- Menu des playlists ----------
// « … » à côté de chaque playlist (ou clic droit) : renommer, exporter, actualiser, supprimer.

let popupMenu = null;
function closePopupMenu() { popupMenu?.remove(); popupMenu = null; }
/** Small menu next to `anchor` (an element or a point); items are [icon, label, action, danger]. */
function showMenu(items, anchor) {
  closePopupMenu();
  const menu = make('div', 'popup-menu');
  menu.setAttribute('role', 'menu');
  for (const [name, label, action, danger] of items) {
    const item = withIcon(button(label, `popup-item${danger ? ' danger' : ''}`, () => { closePopupMenu(); action(); }), name);
    item.setAttribute('role', 'menuitem');
    menu.append(item);
  }
  menu.onkeydown = (event) => {
    const entries = [...menu.querySelectorAll('button')];
    const index = entries.indexOf(document.activeElement);
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      entries[(index + (event.key === 'ArrowDown' ? 1 : -1) + entries.length) % entries.length]?.focus();
    } else if (event.key === 'Escape') { event.stopPropagation(); closePopupMenu(); }
  };
  document.body.append(menu);
  const rect = anchor instanceof Element ? anchor.getBoundingClientRect() : { left: anchor.x, top: anchor.y, bottom: anchor.y };
  const { offsetWidth: width, offsetHeight: height } = menu;
  menu.style.left = `${Math.max(8, Math.min(rect.left, window.innerWidth - width - 8))}px`;
  menu.style.top = `${rect.bottom + height + 8 > window.innerHeight ? Math.max(8, rect.top - height - 4) : rect.bottom + 4}px`;
  popupMenu = menu;
  menu.querySelector('button')?.focus();
}
document.addEventListener('pointerdown', (event) => { if (popupMenu && !popupMenu.contains(event.target)) closePopupMenu(); }, true);
window.addEventListener('blur', closePopupMenu);
window.addEventListener('resize', closePopupMenu);

function showPlaylistMenu(playlist, anchor) {
  showMenu([
    ['edit', 'Renommer…', () => renamePlaylist(playlist)],
    ['download', 'Exporter en M3U…', () => showExport([playlist.id])],
    ['refresh', 'Actualiser', () => refreshPlaylist(playlist)],
    ['trash', 'Supprimer…', () => removePlaylist(playlist), true],
  ], anchor);
}
async function refreshPlaylist(playlist) {
  toast(`Actualisation de « ${playlist.name} »…`);
  try {
    const updated = await invoke('refresh_playlist', { id: playlist.id });
    await refreshLibrary();
    toast(`${updated.channels.length} médias actualisés.`);
  } catch (error) { toast(errorMessage(error), 'error'); }
}
async function removePlaylist(playlist, after) {
  if (!window.confirm(`Supprimer « ${playlist.name} » ?`)) return;
  try {
    await invoke('remove_playlist', { id: playlist.id });
    if (playlist.id === 'local-media') { clearQueue(); if (state.playing?.url.startsWith('file:')) state.playing.channelId = null; }
    if (state.view === playlist.id) state.view = 'all';
    await refreshLibrary();
    after?.();
  } catch (error) { toast(errorMessage(error), 'error'); }
}

// ---------- Export M3U ----------

function fileNameOf(path = '') { return String(path).split(/[\\/]/).pop(); }
function safeFileName(name) { return name.replace(/[\\/:*?"<>|]+/g, '-').replace(/^[.\s]+/, '').trim() || 'Playlist'; }

function showExport(preselected = []) {
  const playlists = state.library.playlists;
  if (!playlists.length) { toast('Ajoutez d’abord une playlist.', 'error'); return; }
  const chosen = new Set(preselected.length ? preselected : playlists.map((playlist) => playlist.id));
  const xtream = new Set(state.library.xtreamAccounts.map((account) => account.id));
  let separate = false;
  modal('Exporter en M3U', (body) => {
    body.append(make('p', 'modal-intro', 'Créez une playlist M3U lisible par VLC, Kodi, TiviMate ou une autre application. Groupes, logos, identifiants du guide TV et en-têtes HTTP sont conservés.'));
    const list = make('div', 'check-list');
    const note = make('p', 'muted');
    const mode = selectField('Fichiers créés', [['merge', 'Un seul fichier (adresses en double retirées)'], ['separate', 'Un fichier par playlist, dans un dossier']], 'merge', (value) => { separate = value === 'separate'; });
    const run = withIcon(button('Exporter…', 'primary full', () => runExport(run)), 'download');
    const result = make('div', 'export-result');
    const refresh = () => {
      list.replaceChildren(checkboxField(`Toutes les playlists (${playlists.length})`, chosen.size === playlists.length, (checked) => {
        chosen.clear();
        if (checked) playlists.forEach((playlist) => chosen.add(playlist.id));
        refresh();
      }));
      list.firstChild.classList.add('check-all');
      for (const playlist of playlists) {
        const count = playlist.channels.length;
        list.append(checkboxField(`${playlist.name} · ${count.toLocaleString('fr-FR')} média${count > 1 ? 's' : ''}`, chosen.has(playlist.id), (checked) => {
          if (checked) chosen.add(playlist.id); else chosen.delete(playlist.id);
          refresh();
        }));
      }
      mode.hidden = chosen.size < 2;
      run.disabled = chosen.size === 0;
      const warnings = [];
      if ([...chosen].some((id) => xtream.has(id))) warnings.push('Les adresses Xtream exportées contiennent votre identifiant et votre mot de passe : ne partagez pas ce fichier. Les séries n’ont pas d’adresse propre et ne sont pas exportées.');
      if (chosen.has('local-media')) warnings.push('Les médias locaux sont exportés avec leur chemin sur ce Mac.');
      note.textContent = warnings.join(' ');
      note.hidden = !warnings.length;
    };
    const runExport = async (target) => {
      const ids = playlists.filter((playlist) => chosen.has(playlist.id)).map((playlist) => playlist.id);
      if (!ids.length) return;
      const split = separate && ids.length > 1;
      const first = playlists.find((playlist) => playlist.id === ids[0]);
      let destination;
      try {
        destination = split
          ? await open({ directory: true, multiple: false, title: 'Dossier des fichiers M3U' })
          : await save({ title: 'Exporter en M3U', defaultPath: `${safeFileName(ids.length === 1 ? first.name : 'Fluxo')}.m3u`, filters: [{ name: 'Playlist M3U', extensions: ['m3u', 'm3u8'] }] });
      } catch (error) { toast(errorMessage(error), 'error'); return; }
      if (!destination) return;
      target.disabled = true; target.lastChild.textContent = 'Exportation…';
      try {
        const outcome = await invoke('export_m3u', { request: { playlists: ids, destination, separate: split } });
        const summary = `${outcome.channels.toLocaleString('fr-FR')} média${outcome.channels > 1 ? 's' : ''} exporté${outcome.channels > 1 ? 's' : ''}${outcome.skipped ? ` · ${outcome.skipped} ignoré${outcome.skipped > 1 ? 's' : ''} (séries, adresses introuvables)` : ''}.`;
        result.replaceChildren(make('strong', '', summary));
        for (const file of outcome.files) {
          const row = make('div', 'settings-row'); const info = make('div');
          info.append(make('strong', '', fileNameOf(file)), make('small', '', file));
          row.append(info, iconButton('folder-open', 'Afficher dans le Finder', 'icon-button', () => revealItemInDir(file).catch((error) => toast(errorMessage(error), 'error'))));
          result.append(row);
        }
        toast(summary);
      } catch (error) { toast(errorMessage(error), 'error'); }
      finally { target.disabled = chosen.size === 0; target.lastChild.textContent = 'Exporter…'; }
    };
    body.append(list, mode, note, run, result);
    refresh();
  });
}

// ---------- Convertisseur ----------
// Les conversions tournent dans Fluxo (FFmpeg, une à la fois) : la fenêtre peut être fermée.

const MEDIA_EXTENSIONS = ['mp4', 'm4v', 'mov', 'mkv', 'avi', 'wmv', 'asf', 'flv', 'f4v', 'webm', 'ts', 'm2ts', 'mts', 'mpg', 'mpeg', 'vob', '3gp', '3g2', 'ogv', 'divx', 'rm', 'rmvb', 'mxf', 'dv', 'mp3', 'm4a', 'aac', 'wav', 'aiff', 'aif', 'flac', 'ogg', 'opus', 'wma', 'ac3', 'mka'];
const CONVERT_HEIGHTS = [['', 'Taille d’origine'], ['2160', '4K (2160p) au plus'], ['1440', '1440p au plus'], ['1080', '1080p au plus'], ['720', '720p au plus'], ['480', '480p au plus']];
const CONVERT_QUALITIES = [['high', 'Haute (fichiers plus gros)'], ['standard', 'Standard'], ['small', 'Compacte (fichiers plus petits)']];
const converter = (() => {
  const defaults = { preset: 'mp4-h264', height: '', quality: 'standard', hardware: true };
  let saved = {};
  try { saved = JSON.parse(localStorage.getItem('fluxo-converter') || '{}'); } catch { /* private storage */ }
  return { settings: { ...defaults, ...saved }, jobs: new Map(), catalog: null, files: [], outputDir: null, view: null };
})();
function saveConverterSettings() { try { localStorage.setItem('fluxo-converter', JSON.stringify(converter.settings)); } catch { /* private storage */ } }
function converterVisible() { return Boolean(converter.view?.root.isConnected && !$('modal').hidden); }
function updateConverterBadge() {
  const active = [...converter.jobs.values()].filter((job) => job.status === 'queued' || job.status === 'running').length;
  $('converterBadge').hidden = active === 0;
  $('converterBadge').textContent = String(active);
  $('converterButton').title = active ? `${active} conversion${active > 1 ? 's' : ''} en cours` : 'Convertir des vidéos et fichiers audio';
}
listen('convert', (event) => {
  const job = event.payload;
  const previous = converter.jobs.get(job.id);
  converter.jobs.set(job.id, job);
  updateConverterBadge();
  if (converterVisible()) { converter.view.update(job); return; }
  if (previous && previous.status !== job.status) {
    if (job.status === 'done') toast(`Conversion terminée : ${fileNameOf(job.output)}`);
    else if (job.status === 'failed') toast(`Conversion de ${job.name} impossible. ${job.error || ''}`, 'error');
  }
}).catch(() => {});

function jobStatusText(job) {
  switch (job.status) {
    case 'queued': return 'En attente';
    case 'running': {
      if (!job.progress) return job.bytes ? `Conversion… ${formatBytes(job.bytes)} écrits` : 'Analyse et démarrage…';
      const parts = [`${Math.floor(job.progress * 100)} %`];
      if (job.speed) parts.push(`${job.speed.toLocaleString('fr-FR', { maximumFractionDigits: 1 })}× le temps réel`);
      if (Number.isFinite(job.eta)) parts.push(`reste ${timeText(job.eta)}`);
      return parts.join(' · ');
    }
    case 'done': return `Terminé · ${formatBytes(job.bytes)}`;
    case 'cancelled': return 'Annulée';
    default: return job.error || 'La conversion a échoué.';
  }
}
function jobRow(job) {
  const row = make('div', `convert-job ${job.status}`);
  row.dataset.id = String(job.id);
  const info = make('div', 'convert-job-main');
  info.append(make('strong', '', job.name), make('small', '', `${job.label.split(' — ')[0]} → ${fileNameOf(job.output)}`));
  if (job.status === 'running' || job.status === 'queued') {
    const bar = make('div', `progress-bar${job.status === 'running' && !job.progress ? ' indeterminate' : ''}`);
    const fill = make('span'); fill.style.width = `${Math.round((job.progress || 0) * 1000) / 10}%`;
    bar.append(fill); info.append(bar);
  }
  const status = make('small', 'convert-status', jobStatusText(job));
  status.title = status.textContent;
  info.append(status);
  row.append(info);
  if (job.status === 'queued' || job.status === 'running') {
    row.append(iconButton('x', 'Annuler cette conversion', 'icon-button', () => invoke('convert_cancel', { id: job.id }).catch((error) => toast(errorMessage(error), 'error'))));
  } else if (job.status === 'done') {
    row.append(
      iconButton('play', 'Lire dans Fluxo', 'icon-button', async () => { closeModal(); try { await importLocalMedia([job.output]); } catch (error) { toast(errorMessage(error), 'error'); } }),
      iconButton('folder-open', 'Afficher dans le Finder', 'icon-button', () => revealItemInDir(job.output).catch((error) => toast(errorMessage(error), 'error'))),
    );
  }
  return row;
}

async function showConverter(paths = []) {
  converter.files = [...new Set([...converter.files, ...paths])];
  if (!converter.catalog?.ffmpeg) converter.catalog = await invoke('convert_catalog').catch(() => ({ ffmpeg: false, presets: [] }));
  const jobs = await invoke('convert_jobs').catch(() => null);
  if (jobs) converter.jobs = new Map(jobs.map((job) => [job.id, job]));
  updateConverterBadge();
  const { settings, catalog } = converter;
  modal('Convertisseur vidéo', (body) => {
    const root = make('div', 'converter');
    body.append(root);
    if (!catalog.ffmpeg) {
      root.append(make('p', 'info-message', 'FFmpeg est introuvable. Installez-le avec « brew install ffmpeg », puis relancez Fluxo.'));
      converter.view = null;
      return;
    }
    root.append(make('p', 'modal-intro', 'Convertissez vidéos et fichiers audio dans un autre format. Les conversions continuent en arrière-plan, une à la fois : vous pouvez fermer cette fenêtre. Le fichier d’origine n’est jamais modifié.'));

    // Fichiers à convertir
    const files = make('div', 'convert-files');
    const drawFiles = () => {
      files.replaceChildren();
      if (!converter.files.length) files.append(make('p', 'muted', 'Aucun fichier choisi. Choisissez des fichiers ou glissez-les dans la fenêtre.'));
      for (const path of converter.files) {
        const row = make('div', 'convert-file');
        const name = make('span', '', fileNameOf(path)); name.title = path;
        row.append(icon('file-video'), name, iconButton('x', `Retirer ${fileNameOf(path)}`, 'row-action', () => { converter.files = converter.files.filter((item) => item !== path); drawFiles(); }));
        files.append(row);
      }
      start.disabled = !converter.files.length;
      start.lastChild.textContent = converter.files.length > 1 ? `Convertir ${converter.files.length} fichiers` : 'Convertir';
    };
    const pick = withIcon(button('Choisir des fichiers…', 'subtle full', async () => {
      const picked = await open({ multiple: true, filters: [{ name: 'Vidéo et audio', extensions: MEDIA_EXTENSIONS }] }).catch(() => null);
      if (!picked) return;
      converter.view.addFiles(Array.isArray(picked) ? picked : [picked]);
    }), 'folder-open');

    // Réglages
    const format = make('label', 'field'); format.append(make('span', '', 'Format de sortie'));
    const select = document.createElement('select');
    for (const [group, title] of [['video', 'Réencoder la vidéo'], ['copy', 'Sans réencodage (rapide)'], ['audio', 'Extraire le son']]) {
      const optgroup = document.createElement('optgroup'); optgroup.label = title;
      for (const preset of catalog.presets.filter((item) => item.group === group)) {
        const option = document.createElement('option');
        option.value = preset.id; option.disabled = !preset.available;
        option.textContent = preset.available ? preset.label : `${preset.label} (indisponible)`;
        optgroup.append(option);
      }
      select.append(optgroup);
    }
    if (!catalog.presets.some((preset) => preset.id === settings.preset && preset.available)) settings.preset = catalog.presets.find((preset) => preset.available)?.id || '';
    select.value = settings.preset;
    format.append(select);
    const height = selectField('Définition', CONVERT_HEIGHTS, settings.height, (value) => { settings.height = value; saveConverterSettings(); });
    const qualityField = selectField('Qualité', CONVERT_QUALITIES, settings.quality, (value) => { settings.quality = value; saveConverterSettings(); });
    const hardware = checkboxField('Encodage matériel (VideoToolbox) : beaucoup plus rapide. L’encodeur logiciel prend le relais si la source est refusée.', settings.hardware, (checked) => { settings.hardware = checked; saveConverterSettings(); });
    const settingsRow = make('div', 'field-row'); settingsRow.append(height, qualityField);
    const describe = make('p', 'muted');
    const applyPreset = () => {
      const preset = catalog.presets.find((item) => item.id === settings.preset);
      const reencode = preset?.group === 'video';
      const lossless = ['flac', 'wav'].includes(preset?.id);
      height.hidden = !reencode;
      qualityField.hidden = !(reencode || (preset?.group === 'audio' && !lossless));
      settingsRow.hidden = height.hidden && qualityField.hidden;
      hardware.hidden = !(reencode && preset?.hardware);
      describe.textContent = preset?.group === 'copy'
        ? 'Les pistes sont copiées telles quelles : conversion en quelques secondes, sans perte. Si le conteneur refuse le son, il est converti en AAC.'
        : preset?.id === 'gif' ? 'Le GIF est limité à 480 lignes par défaut et à 12 images par seconde : réservez-le aux courts extraits.'
          : preset?.group === 'audio' ? 'Seule la première piste audio est conservée.' : '';
      describe.hidden = !describe.textContent;
    };
    select.onchange = () => { settings.preset = select.value; saveConverterSettings(); applyPreset(); };

    // Destination
    const destination = make('div', 'settings-row convert-destination');
    const drawDestination = () => {
      const info = make('div');
      info.append(make('strong', '', 'Destination'), make('small', '', converter.outputDir || 'À côté de chaque fichier d’origine'));
      destination.replaceChildren(info, button('Choisir…', 'icon-button', async () => {
        const dir = await open({ directory: true, multiple: false, title: 'Dossier des fichiers convertis' }).catch(() => null);
        if (dir) { converter.outputDir = dir; drawDestination(); }
      }));
      if (converter.outputDir) destination.append(iconButton('x', 'Enregistrer à côté des fichiers d’origine', 'icon-button', () => { converter.outputDir = null; drawDestination(); }));
    };

    const start = withIcon(button('Convertir', 'primary full', async () => {
      if (!converter.files.length || !settings.preset) return;
      start.disabled = true;
      try {
        const added = await invoke('convert_add', { paths: converter.files, options: { preset: settings.preset, height: settings.height ? Number(settings.height) : null, quality: settings.quality, hardware: settings.hardware, outputDir: converter.outputDir } });
        for (const job of added) converter.view?.update(job);
        converter.files = [];
        drawFiles();
        toast(`${added.length} conversion${added.length > 1 ? 's' : ''} ajoutée${added.length > 1 ? 's' : ''}.`);
      } catch (error) { toast(errorMessage(error), 'error'); start.disabled = !converter.files.length; }
    }), 'convert');

    // Conversions
    const jobsHead = make('div', 'convert-jobs-head');
    const clear = button('Effacer les terminées', 'subtle small', async () => {
      const remaining = await invoke('convert_clear').catch(() => null);
      if (!remaining) return;
      converter.jobs = new Map(remaining.map((job) => [job.id, job]));
      drawJobs();
    });
    jobsHead.append(make('h3', 'settings-heading', 'Conversions'), clear);
    const jobList = make('div', 'convert-jobs');
    const drawJobs = () => {
      jobList.replaceChildren();
      const all = [...converter.jobs.values()];
      if (!all.length) jobList.append(make('p', 'muted', 'Aucune conversion pour le moment.'));
      for (const job of all) jobList.append(jobRow(job));
      clear.hidden = !all.some((job) => ['done', 'failed', 'cancelled'].includes(job.status));
    };

    root.append(make('h3', 'settings-heading first', 'Fichiers'), files, pick, format, describe, settingsRow, hardware, destination, start, jobsHead, jobList);
    converter.view = {
      root,
      addFiles(paths) { converter.files = [...new Set([...converter.files, ...paths])]; drawFiles(); },
      update(job) {
        const existing = jobList.querySelector(`.convert-job[data-id="${job.id}"]`);
        if (existing) existing.replaceWith(jobRow(job)); else drawJobs();
        clear.hidden = ![...converter.jobs.values()].some((item) => ['done', 'failed', 'cancelled'].includes(item.status));
      },
    };
    drawFiles(); applyPreset(); drawDestination(); drawJobs();
  }, { wide: true });
}

async function showSettings() {
  const [engine, epg] = await Promise.all([invoke('engine_info').catch(() => state.engine), invoke('epg_status').catch(() => state.epg)]);
  state.engine = engine;
  modal('Sources & guide TV', (body) => {
    body.append(make('p', 'modal-intro', 'Gérez vos playlists, comptes Xtream, le guide des programmes et le moteur de lecture.'));
    body.append(withIcon(button('Ajouter une playlist M3U', 'subtle full', showAddPlaylist), 'plus'));
    body.append(withIcon(button('Connecter Xtream Codes', 'subtle full', showAddXtream), 'plus'));

    body.append(make('h3', 'settings-heading', 'Guide TV'));
    const epgField = pasteField('Guide XMLTV personnel', 'Adresse HTTPS ou fichier XML (.xml, .xml.gz)', state.library.epgSource || ''); body.append(epgField.label);
    body.append(button('Choisir un fichier XMLTV', 'subtle full', async () => { const path = await open({ multiple: false, filters: [{ name: 'Guide XMLTV', extensions: ['xml', 'gz'] }] }); if (path) epgField.input.value = path; }));
    body.append(button('Enregistrer le guide TV', 'primary full', async (event) => {
      const target = event.currentTarget; target.disabled = true; target.textContent = 'Chargement du guide…';
      try { const count = await invoke('set_epg_source', { source: epgField.input.value }); state.library.epgSource = epgField.input.value.trim() || null; toast(`${count.toLocaleString('fr-FR')} programmes chargés.`); await refreshEpgStatus(); }
      catch (error) { toast(errorMessage(error), 'error'); }
      finally { target.disabled = false; target.textContent = 'Enregistrer le guide TV'; }
    }));
    body.append(checkboxField('Utiliser les guides annoncés par les playlists et les comptes Xtream', !state.library.ignorePlaylistEpg, async (enabled) => {
      try { await invoke('set_playlist_epg', { enabled }); state.library.ignorePlaylistEpg = !enabled; toast('Rechargement du guide…'); }
      catch (error) { toast(errorMessage(error), 'error'); }
    }));
    const sources = make('div', 'status-list');
    if (!epg.sources?.length) sources.append(make('small', 'muted', 'Aucune source de guide active.'));
    for (const source of epg.sources || []) {
      const row = make('div', `status-row${source.error ? ' bad' : ''}`);
      row.append(make('strong', '', source.label), make('small', '', source.error || `${source.programs.toLocaleString('fr-FR')} programmes`));
      sources.append(row);
    }
    body.append(sources);
    body.append(button(epg.loading ? 'Chargement en cours…' : 'Actualiser le guide maintenant', 'subtle full', () => { invoke('reload_epg'); toast('Actualisation du guide lancée.'); closeModal(); }));

    body.append(make('h3', 'settings-heading', 'Moteur de lecture'));
    const engineChoices = [
      ['mpv', 'mpv en priorité (par défaut, formats les plus variés)'],
      ['auto', 'Automatique (lecteur système pour les flux compatibles)'],
      ['system', 'Lecteur système en priorité (AirPlay, image dans l’image)'],
    ];
    body.append(selectField('Moteur préféré', engineChoices, preferences.engine, (value) => { preferences.engine = value; preferences.engineChosen = true; savePreferences(); }));
    body.append(make('p', 'muted', 'mpv lit presque tous les formats (HEVC en direct, MPEG-2, MKV, DASH, sons AC3/DTS…) avec le décodage matériel du Mac. AirPlay et l’image dans l’image rebasculent la chaîne sur le lecteur système. Automatique garde le lecteur système pour les flux compatibles et retient le moteur qui a fonctionné pour chaque chaîne. Dans tous les cas, l’autre moteur sert de secours.'));
    const mpvRow = make('div', `status-row${engine.mpv?.available ? '' : ' bad'}`);
    mpvRow.append(make('strong', '', 'Moteur mpv'), make('small', '', engine.mpv?.available ? `Actif : ${engine.mpv.library}` : `Indisponible${engine.mpv?.error ? ` : ${engine.mpv.error}` : ''}. Installez-le avec brew install mpv, puis relancez Fluxo.`));
    body.append(mpvRow);
    body.append(button('Oublier le moteur retenu pour chaque chaîne', 'subtle full', () => { engineMemory.clear(); toast('Choix automatique réinitialisé.'); }));
    const ffmpeg = make('div', `status-row${engine.ffmpeg ? '' : ' bad'}`);
    ffmpeg.append(make('strong', '', 'Moteur de compatibilité FFmpeg'), make('small', '', engine.ffmpeg ? `Actif : ${engine.ffmpeg}` : 'Non installé. Pour lire MPEG-2, HEVC en TS, DASH, MKV ou AVI : brew install ffmpeg, puis relancez Fluxo.'));
    body.append(ffmpeg);
    body.append(make('p', 'muted', 'Fluxo lit d’abord le flux directement, puis via un relais local qui ajoute les en-têtes exigés, un reconditionnement MPEG-TS → HLS, et enfin la conversion FFmpeg. Si une source reste indisponible, les autres sources de la même chaîne sont essayées.'));
    body.append(checkboxField('Vérifier automatiquement la disponibilité des chaînes affichées', preferences.autoCheck, (enabled) => { preferences.autoCheck = enabled; savePreferences(); }));

    body.append(make('h3', 'settings-heading', 'Playlists'));
    if (state.library.playlists.length) body.append(withIcon(button('Exporter en M3U…', 'subtle full', () => showExport()), 'download'));
    for (const playlist of state.library.playlists) {
      const row = make('div', 'settings-row'); const info = make('div');
      info.append(make('strong', '', playlist.name), make('small', '', `${playlist.channels.length} média${playlist.channels.length > 1 ? 's' : ''}${playlist.epgUrl ? ' · guide TV inclus' : ''}`));
      row.append(info,
        iconButton('edit', `Renommer ${playlist.name}`, 'icon-button', () => renamePlaylist(playlist, showSettings)),
        iconButton('download', `Exporter ${playlist.name} en M3U`, 'icon-button', () => showExport([playlist.id])),
        iconButton('refresh', `Actualiser ${playlist.name}`, 'icon-button', () => refreshPlaylist(playlist)),
        iconButton('trash', `Supprimer ${playlist.name}`, 'icon-button danger', () => removePlaylist(playlist, showSettings)));
      body.append(row);
    }

    body.append(make('h3', 'settings-heading', 'Mises à jour'));
    body.append(make('p', 'muted', `Version installée : ${state.engine.version || '—'}. Les nouvelles versions sont publiées sur GitHub et vérifiées (signature) avant installation.`));
    body.append(button('Rechercher une mise à jour', 'subtle full', async (event) => {
      const target = event.currentTarget; target.disabled = true; target.textContent = 'Recherche…';
      const info = await checkForUpdates();
      target.disabled = false; target.textContent = 'Rechercher une mise à jour';
      if (info) closeModal();
    }));
    body.append(checkboxField('Rechercher les mises à jour au démarrage', preferences.autoUpdate, (enabled) => { preferences.autoUpdate = enabled; savePreferences(); }));

    body.append(make('h3', 'settings-heading', 'Données'));
    const historyCount = state.library.recent.length + state.library.progress.length;
    body.append(button('Effacer l’historique', 'subtle full', async () => {
      if (!window.confirm('Effacer les chaînes récentes et les positions de reprise ?')) return;
      try { await invoke('clear_history'); state.library.recent = []; state.library.progress = []; render(); toast('Historique effacé.'); showSettings(); }
      catch (error) { toast(errorMessage(error), 'error'); }
    }));
    body.append(make('p', 'muted', historyCount ? `${state.library.recent.length} récent${state.library.recent.length > 1 ? 's' : ''} · ${state.library.progress.length} reprise${state.library.progress.length > 1 ? 's' : ''}` : 'L’historique est vide.'));
    body.append(button('Réinitialiser Fluxo…', 'subtle danger full', resetApp));
    body.append(make('p', 'muted', 'Supprime playlists, comptes Xtream (et leurs mots de passe du trousseau), favoris, historique, guide TV et préférences. Les fichiers enregistrés sont conservés.'));
  });
}

async function resetApp() {
  if (!window.confirm('Réinitialiser Fluxo ? Toutes les playlists, comptes, favoris, historique et réglages seront supprimés. Cette action est irréversible.')) return;
  try {
    await invoke('reset_app');
    await invoke('hide_youtube_player').catch(() => {});
    try { localStorage.clear(); } catch { /* private storage */ }
    window.location.reload();
  } catch (error) { toast(errorMessage(error), 'error'); }
}

function showFilters() {
  modal('Filtres', (body) => {
    const channels = currentChannels();
    const countries = facet(channels, 'country');
    const languages = facet(channels, 'language');
    const apply = () => { $('channels').scrollTop = 0; render(); };
    if (countries.length) body.append(selectField('Pays', [['', 'Tous les pays'], ...countries.map(([code, count]) => [code, `${countryLabel(code)} (${count})`])], state.country, (value) => { state.country = value; apply(); }));
    if (languages.length) body.append(selectField('Langue', [['', 'Toutes les langues'], ...languages.map(([code, count]) => [code, `${code} (${count})`])], state.language, (value) => { state.language = value; apply(); }));
    if (!countries.length && !languages.length) body.append(make('p', 'muted', 'Cette liste n’indique ni pays ni langue.'));
    body.append(checkboxField('Masquer les chaînes hors ligne (dernière vérification)', state.hideOffline, (value) => { state.hideOffline = value; apply(); }));
    body.append(make('h3', 'settings-heading', 'Groupes affichés'));
    body.append(make('p', 'muted', 'Décochez un groupe pour le masquer de toutes les listes (hors favoris et récents).'));
    const search = field('Filtrer les groupes', 'Nom du groupe'); body.append(search.label);
    const list = make('div', 'group-list');
    const hidden = new Set(state.library.hiddenGroups);
    const groups = facet(catalog.all, 'group');
    const save = async () => {
      try { state.library.hiddenGroups = await invoke('set_hidden_groups', { groups: [...hidden] }); apply(); }
      catch (error) { toast(errorMessage(error), 'error'); }
    };
    const draw = () => {
      list.replaceChildren();
      const needle = search.input.value.trim().toLocaleLowerCase('fr');
      for (const [group, count] of groups.filter(([group]) => !needle || group.toLocaleLowerCase('fr').includes(needle)).slice(0, 500)) {
        list.append(checkboxField(`${group} (${count})`, !hidden.has(group), (visible) => { if (visible) hidden.delete(group); else hidden.add(group); save(); }));
      }
    };
    search.input.oninput = draw; draw();
    body.append(list);
    body.append(button('Tout afficher et réinitialiser les filtres', 'subtle full', () => { hidden.clear(); state.country = ''; state.language = ''; state.hideOffline = false; save(); closeModal(); }));
  });
}

async function showStreamInfo() {
  const ctx = player.info;
  if (!ctx) return;
  const status = player.session ? await invoke('stream_status', { session: player.session }).catch(() => null) : null;
  const probe = ctx.probe || await ctx.probePromise;
  modal('Informations sur le flux', (body) => {
    const rows = [
      ['Chaîne', ctx.channel?.name],
      ['Source', `${ctx.index + 1} sur ${ctx.alternatives.length}`],
      ['Moteur', ctx.plan ? MODE_LABELS[ctx.plan.mode] : '—'],
      ['En-têtes', ctx.headers?.userAgent || ctx.headers?.referrer ? [ctx.headers.userAgent && `User-Agent : ${ctx.headers.userAgent}`, ctx.headers.referrer && `Referer : ${ctx.headers.referrer}`].filter(Boolean).join(' · ') : 'Aucun en-tête requis'],
      ['Type', probe?.kind ? probe.kind.toUpperCase() : 'Local / non analysé'],
      ['Vidéo', probe?.video?.join(', ') || '—'],
      ['Audio', probe?.audioMissing ? 'Aucune piste audio dans le flux' : probe?.audio?.join(', ') || '—'],
      ['Pistes audio décodées', media().audioTracks ? String(media().audioTracks.length) : '—'],
      ['Résolution', media().videoWidth ? `${media().videoWidth}×${media().videoHeight}` : '—'],
    ];
    if (player.engine === 'mpv') {
      rows.push(['Décodage', mainMpv.info.hwdec && mainMpv.info.hwdec !== 'no' ? `Matériel (${mainMpv.info.hwdec})` : 'Logiciel']);
      rows.push(['Codecs lus par mpv', [mainMpv.info.videoCodec, mainMpv.info.audioCodec].filter(Boolean).join(' · ') || '—']);
    }
    const shown = shownBitrate();
    if (shown) rows.push(['Débit', `${shown.source === 'declared' ? '≈ ' : ''}${formatBitrate(shown.value)} · ${(BITRATE_SOURCES[shown.source] || '').toLowerCase()}`]);
    if (bitrate.detail?.video || bitrate.detail?.audio) rows.push(['Débit vidéo · audio', `${formatBitrate(bitrate.detail.video)} · ${formatBitrate(bitrate.detail.audio)}`]);
    if (bitrate.meter.total > 0) rows.push(['Données reçues', formatBytes(bitrate.meter.total)]);
    if (bitrate.detail?.dropped) rows.push(['Images perdues', bitrate.detail.dropped.toLocaleString('fr-FR')]);
    if (probe?.encryption) rows.push(['Chiffrement', probe.encryption]);
    if (probe?.unsupported?.length) rows.push(['Non pris en charge par macOS', probe.unsupported.join(', ')]);
    const table = make('div', 'info-table');
    for (const [key, value] of rows) table.append(make('span', '', key), make('strong', '', value || '—'));
    body.append(table);
    if (probe?.message) body.append(make('p', 'info-message', probe.message));
    if (probe?.steps?.length) body.append(make('p', 'muted', probe.steps.join(' → ')));
    const errors = [status?.failure, ...(status?.errors || []), ...(ctx.lastErrors || [])].filter(Boolean);
    if (errors.length) { body.append(make('h3', 'settings-heading', 'Erreurs du serveur')); for (const error of [...new Set(errors)]) body.append(make('p', 'info-message', error)); }
    body.append(make('h3', 'settings-heading', 'Actions'));
    body.append(button('Recharger le flux', 'subtle full', () => { closeModal(); player.reload(); }));
    if (ctx.url && isWebUrl(ctx.url) && ctx.plan?.mode !== 'relay') body.append(button('Essayer via le relais local', 'subtle full', () => { closeModal(); player.force('relay'); }));
    if (state.engine.mpv?.available && ctx.plan?.mode !== 'mpv') body.append(button('Lire avec le moteur mpv', 'subtle full', () => { closeModal(); player.force('mpv'); }));
    if (ctx.plan?.mode === 'mpv') body.append(button('Revenir au lecteur système (AirPlay, image dans l’image)', 'subtle full', () => { closeModal(); player.force('native'); }));
    if (state.engine.ffmpeg && ctx.plan?.mode !== 'transcode') body.append(button('Forcer la conversion FFmpeg (corrige souvent l’absence de son)', 'subtle full', () => { closeModal(); player.force('transcode'); }));
    if (ctx.alternatives.length > 1) {
      body.append(make('h3', 'settings-heading', 'Autres sources de cette chaîne'));
      ctx.alternatives.forEach((channel, index) => {
        const health = state.health[channel.streamUrl];
        const label = `${index === ctx.index ? '● ' : ''}${channel.name}${health ? (health.ok ? ' · disponible' : ` · ${health.message || 'indisponible'}`) : ''}`;
        body.append(button(label, 'episode-button', () => { closeModal(); player.useSource(index); }));
      });
    }
  });
}

// ---------- Enregistrements ----------

function currentRecording() {
  const url = player.info?.url;
  return [...state.recordings.values()].find((item) => item.active && item.url === url);
}
function updateRecordButton() {
  const ctx = player.info;
  const recordable = Boolean(ctx?.url && isWebUrl(ctx.url));
  $('recordButton').hidden = !recordable;
  const active = currentRecording();
  $('recordButton').classList.toggle('recording', Boolean(active));
  $('recordButton').title = active ? 'Arrêter l’enregistrement' : 'Enregistrer ce flux';
}
async function toggleRecording() {
  const ctx = player.info;
  if (!ctx?.url) return;
  const active = currentRecording();
  if (active) { await invoke('stop_recording', { id: active.id }); toast('Arrêt de l’enregistrement…'); return; }
  try {
    const name = state.playing?.catchup ? `${ctx.channel.name} ${state.playing.catchup.title}` : ctx.channel.name;
    const info = await invoke('start_recording', { url: ctx.url, headers: ctx.headers, name });
    state.recordings.set(info.id, { ...info, url: ctx.url });
    updateRecordButton();
    toast(`Enregistrement démarré dans ${info.path.split('/').slice(-2, -1)[0] || 'Fluxo'}.`);
  } catch (error) { toast(errorMessage(error), 'error'); }
}
listen('recording', (event) => {
  const info = event.payload;
  const known = state.recordings.get(info.id);
  state.recordings.set(info.id, { ...known, ...info });
  if (!info.active) toast(info.error ? `Enregistrement interrompu : ${info.error}` : `Enregistrement terminé : ${info.path.split('/').pop()}`, info.error ? 'error' : '');
  updateRecordButton();
}).catch(() => {});
function sizeText(bytes) {
  if (bytes > 1e9) return `${(bytes / 1e9).toFixed(1)} Go`;
  if (bytes > 1e6) return `${Math.round(bytes / 1e6)} Mo`;
  return `${Math.round(bytes / 1e3)} Ko`;
}
async function showRecordings() {
  const data = await invoke('list_recordings').catch((error) => { toast(errorMessage(error), 'error'); return null; });
  if (!data) return;
  modal('Enregistrements', (body) => {
    body.append(make('p', 'modal-intro', `Les enregistrements sont placés dans ${data.dir}. Lancez-en un avec ⏺ dans la barre du lecteur.`));
    const active = data.active.filter((item) => item.active);
    if (active.length) body.append(make('h3', 'settings-heading', 'En cours'));
    for (const item of active) {
      const row = make('div', 'settings-row'); const info = make('div');
      info.append(make('strong', '', item.name), make('small', '', `${sizeText(item.bytes)} · depuis ${clock(item.startedAt)}`));
      row.append(info, button('Arrêter', 'icon-button danger', async () => { await invoke('stop_recording', { id: item.id }); closeModal(); }));
      body.append(row);
    }
    body.append(make('h3', 'settings-heading', 'Fichiers'));
    if (!data.files.length) body.append(make('p', 'muted', 'Aucun enregistrement pour le moment.'));
    for (const file of data.files) {
      const row = make('div', 'settings-row'); const info = make('div');
      info.append(make('strong', '', file.name), make('small', '', `${sizeText(file.bytes)} · ${new Date(file.modified * 1000).toLocaleString('fr-FR')}`));
      row.append(info,
        iconButton('play', 'Lire', 'icon-button', async () => { closeModal(); try { await importLocalMedia([file.path]); } catch (error) { toast(errorMessage(error), 'error'); } }),
        iconButton('convert', 'Convertir dans un autre format', 'icon-button', () => showConverter([file.path])),
        iconButton('folder-open', 'Afficher dans le Finder', 'icon-button', () => revealItemInDir(file.path).catch((error) => toast(errorMessage(error), 'error'))));
      body.append(row);
    }
  });
}

// ---------- Médias locaux ----------

async function openMedia() {
  try { const paths = await open({ multiple: true, filters: [{ name: 'Audio et vidéo', extensions: ['mp4', 'm4v', 'mov', 'mp3', 'm4a', 'aac', 'wav', 'aiff', 'aif', 'webm', 'mkv', 'flac', 'ogg', 'opus', 'ts', 'm2ts', 'mts', 'avi', 'mpg', 'mpeg', 'wmv', 'flv'] }] }); if (!paths) return; await importLocalMedia(Array.isArray(paths) ? paths : [paths]); }
  catch (error) { toast(errorMessage(error), 'error'); }
}
/** Adds files and folders to « Médias locaux »; plays the first one, or with `append`, adds
 * them after the media being played. */
async function importLocalMedia(paths, { append = false } = {}) {
  $('dropOverlay').hidden = true;
  if (!paths.length) return;
  const result = await invoke('import_local_media', { paths });
  await refreshLibrary();
  if (append && state.queue.length) {
    const known = new Set(state.queue.map((item) => item.streamUrl));
    const added = sortedMedia(result.channels).filter((item) => !known.has(item.streamUrl));
    state.queue.push(...added);
    renderQueue();
    toast(added.length ? `${added.length} média${added.length > 1 ? 's' : ''} ajouté${added.length > 1 ? 's' : ''} à la suite.` : 'Ces médias sont déjà dans la file.');
    return;
  }
  setView(result.playlist.id);
  state.queue = sortedMedia(result.channels); state.queueIndex = 0; renderQueue();
  await playQueueIndex(0);
  const extra = result.truncated ? ' Limite de 500 médias atteinte.' : result.skipped ? ` ${result.skipped} fichier${result.skipped > 1 ? 's' : ''} ignoré${result.skipped > 1 ? 's' : ''}.` : '';
  toast(`${result.channels.length} média${result.channels.length > 1 ? 's' : ''} ajouté${result.channels.length > 1 ? 's' : ''}.${extra}`);
}

// ---------- Commandes du lecteur ----------

/** Stops the stream or the video and frees the connection; the queue is kept. */
async function stopPlayback() {
  if (!state.playing && !player.info) return;
  saveProgressNow();
  filmstrip.close();
  player.stop();
  resetQuality();
  if (youtubeSession || !youtubePlayback.hidden) await hideYoutube();
  state.playing = null;
  state.subtitleCues = []; selectSubtitleTrack('off');
  $('videoEmpty').hidden = false;
  $('playingTitle').textContent = 'Aucune lecture';
  $('playingDetail').textContent = 'La vidéo et l’audio se lisent ici.';
  $('liveIndicator').textContent = 'LECTEUR';
  for (const id of ['engineBadge', 'streamInfo', 'favoriteButton']) $(id).hidden = true;
  $('playerStatus').textContent = ''; $('playerError').textContent = ''; openSourceExternally.hidden = true;
  $('programs').replaceChildren(make('p', 'muted', 'Sélectionnez une chaîne pour voir son programme.'));
  markPlayingRows(); updateRecordButton(); updateControls(); renderQueue();
}

function toggleFilmstrip() {
  if (filmstrip.isOpen) { filmstrip.close(); return; }
  if (!filmstripSource()) { toast('Le plan est disponible pour les films, épisodes et fichiers vidéo.'); return; }
  if (!filmstrip.open()) toast('Le plan de cette vidéo n’est pas disponible.', 'error');
  else if (!filmstripSource().local) toast('Les images sont prises sur le serveur du film : elles peuvent mettre quelques secondes à apparaître.');
}

// Preview above the seek bar: the time under the pointer and, once loaded, its picture.
function seekPreviewAt(event) {
  const current = media();
  const duration = current.duration;
  const seek = $('seek');
  if (!Number.isFinite(duration) || duration <= 0 || seek.disabled) { $('seekPreview').hidden = true; return; }
  const rect = seek.getBoundingClientRect();
  const ratio = Math.max(0, Math.min(1, (event.clientX - rect.left) / rect.width));
  const time = ratio * duration;
  const info = filmstripSource();
  const frame = info ? filmstrip.preview(info.source, time) : null;
  const preview = $('seekPreview');
  const image = preview.firstChild;
  image.hidden = !frame;
  if (frame) image.style.backgroundImage = `url("${frame.image}")`;
  preview.lastChild.textContent = timeText(time);
  preview.hidden = false;
  const host = $('playerControls').getBoundingClientRect();
  const half = preview.offsetWidth / 2;
  preview.style.left = `${Math.max(half + 4, Math.min(host.width - half - 4, event.clientX - host.left))}px`;
}
$('seek').addEventListener('pointerenter', () => filmstrip.prefetch());
$('seek').addEventListener('pointermove', seekPreviewAt);
$('seek').addEventListener('pointerleave', () => { $('seekPreview').hidden = true; });

// In full screen and player mode, the controls and the pointer disappear after a few seconds
// of playback without movement; any movement, key or pause brings them back.
const IDLE_DELAY = 3000;
let idleTimer = 0;
function wakeControls() {
  document.body.classList.remove('controls-idle');
  clearTimeout(idleTimer);
  idleTimer = setTimeout(() => {
    const current = media();
    const immersive = document.body.classList.contains('native-fullscreen') || document.body.classList.contains('player-mode');
    if (!immersive || current.paused || !current.getAttribute('src') || filmstrip.isOpen || !$('modal').hidden || popupMenu) return;
    if ($('playerControls').matches(':hover') || $('playerControls').contains(document.activeElement) && document.activeElement !== document.body && document.activeElement.matches(':focus-visible')) return;
    document.body.classList.add('controls-idle');
  }, IDLE_DELAY);
}
for (const type of ['pointermove', 'pointerdown', 'keydown', 'wheel']) document.addEventListener(type, wakeControls, { passive: true });

function updateControls() {
  const current = media();
  const duration = current.duration;
  const seekable = Number.isFinite(duration) && duration > 0;
  $('seek').disabled = !seekable;
  $('back10').disabled = !seekable; $('forward10').disabled = !seekable;
  $('seek').value = seekable ? Math.round(current.currentTime / duration * 1000) : 0;
  $('timeLabel').textContent = seekable ? `${timeText(current.currentTime)} / ${timeText(duration)}` : current.getAttribute('src') ? 'DIRECT' : '00:00 / 00:00';
  setIcon($('togglePlay'), current.paused ? 'play' : 'pause');
  $('filmstripButton').hidden = !seekable || !filmstripSource();
  if (filmstrip.isOpen) { if ($('filmstripButton').hidden) filmstrip.close(); else filmstrip.update(); }
  setIcon($('mute'), current.muted || current.volume === 0 ? 'volume-off' : current.volume < 0.5 ? 'volume-low' : 'volume-high');
  $('volume').value = current.muted ? 0 : current.volume;
}
function togglePause() { const current = media(); if (current.paused) current.play()?.catch?.(() => {}); else current.pause(); }
function toggleMute() { media().muted = !media().muted; updateControls(); }
function seekBy(delta) {
  const current = media();
  if (Number.isFinite(current.duration)) current.currentTime = Math.max(0, Math.min(current.duration, current.currentTime + delta));
}
function renderSubtitles() {
  const overlay = $('subtitleOverlay');
  if (state.subtitleTrack === 'external') {
    const time = media().currentTime;
    overlay.textContent = state.subtitleCues.filter((cue) => cue.start <= time && cue.end >= time).map((cue) => cue.text).join('\n');
    return;
  }
  if (state.subtitleTrack.startsWith('native:')) {
    const track = video.textTracks[Number(state.subtitleTrack.split(':')[1])];
    overlay.textContent = track?.activeCues ? Array.from(track.activeCues).map((cue) => cue.text).join('\n') : '';
    return;
  }
  overlay.textContent = '';
}
function selectSubtitleTrack(value) {
  state.subtitleTrack = value;
  Array.from(video.textTracks).forEach((track, index) => { track.mode = value === `native:${index}` ? 'hidden' : 'disabled'; });
  // mpv draws the subtitles of the stream itself; external files use the HTML overlay.
  if (player.engine === 'mpv') mainMpv.selectSubtitle(value.startsWith('mpv:') ? value.slice(4) : null);
  $('subtitleOptions').classList.toggle('subtitle-active', value !== 'off');
  renderSubtitles();
}
async function loadSubtitleFile(reopen) {
  const path = await open({ multiple: false, filters: [{ name: 'Sous-titres', extensions: ['srt', 'vtt'] }] });
  if (!path) return;
  try {
    const text = await invoke('read_subtitle', { path });
    const cues = parseSubtitles(text);
    if (!cues.length) throw new Error('Aucun sous-titre valide dans ce fichier.');
    state.subtitleCues = cues; selectSubtitleTrack('external'); toast(`${cues.length} sous-titres chargés.`);
    reopen();
  } catch (error) { toast(errorMessage(error), 'error'); }
}
function appendSubtitleOptions(body, reopen) {
  const tracks = [['off', 'Désactivés']];
  if (state.subtitleCues.length) tracks.push(['external', 'Fichier externe']);
  if (player.engine === 'mpv') {
    for (const track of mainMpv.subtitleTracks()) {
      tracks.push([`mpv:${track.id}`, track.label]);
      if (track.selected && state.subtitleTrack === 'off') state.subtitleTrack = `mpv:${track.id}`;
    }
  } else Array.from(video.textTracks).forEach((track, index) => tracks.push([`native:${index}`, track.label || track.language || `Piste ${index + 1}`]));
  body.append(selectField('Sous-titres', tracks, state.subtitleTrack, selectSubtitleTrack));
  body.append(button('Importer un fichier SRT ou VTT', 'subtle full', () => loadSubtitleFile(reopen)));
  body.append(rangeField('Taille des sous-titres', 60, 200, subtitleSettings.size, '%', (value) => { subtitleSettings.size = value; applySubtitleSettings(); }));
  body.append(rangeField('Hauteur depuis le bas', 2, 40, subtitleSettings.position, '%', (value) => { subtitleSettings.position = value; applySubtitleSettings(); }));
}
function showSubtitleOptions() { modal('Sous-titres', (body) => appendSubtitleOptions(body, showSubtitleOptions)); }
function showPlayerOptions() {
  modal('Options du lecteur', (body) => {
    body.append(selectField('Vitesse', [['0.5', '0,5×'], ['0.75', '0,75×'], ['1', 'Normale'], ['1.25', '1,25×'], ['1.5', '1,5×'], ['2', '2×']], String(media().playbackRate), (value) => { media().playbackRate = Number(value); }));
    appendSubtitleOptions(body, showPlayerOptions);
    const audioTracks = media().audioTracks;
    if (audioTracks?.length > 1) {
      body.append(selectField('Piste audio', Array.from(audioTracks).map((track, index) => [String(index), track.label || track.language || `Piste ${index + 1}`]), String(Array.from(audioTracks).findIndex((track) => track.enabled)), (selected) => {
        Array.from(audioTracks).forEach((track, index) => { track.enabled = index === Number(selected); });
      }));
    }
    body.append(make('p', 'muted', 'Raccourcis : ↑/↓ chaîne précédente/suivante, chiffres pour aller à un numéro, ⌫ dernière chaîne, Espace lecture/pause, ←/→ ±10 s, M son, F plein écran, I infos du flux.'));
  });
}
function syncFullscreen(enabled) {
  document.body.classList.toggle('native-fullscreen', enabled);
  wakeControls();
  setIcon($('fullscreen'), enabled ? 'fullscreen-exit' : 'fullscreen');
  $('fullscreen').title = enabled ? 'Quitter le plein écran (F)' : 'Plein écran (F)';
}
async function toggleFullscreen() {
  try {
    const window = getCurrentWindow();
    const next = !(await window.isFullscreen());
    await window.setFullscreen(next);
    syncFullscreen(next);
    scheduleYoutubeBounds();
  } catch (error) { toast(`Plein écran indisponible : ${errorMessage(error)}`, 'error'); }
}
function togglePlayerMode() {
  if (document.body.classList.contains('multiview-mode')) toggleMultiview();
  const enabled = document.body.classList.toggle('player-mode');
  $('playerMode').lastChild.textContent = enabled ? 'Afficher le catalogue' : 'Mode lecteur';
  wakeControls();
  setIcon($('playerMode'), enabled ? 'player-mode-exit' : 'player-mode');
  scheduleYoutubeBounds();
}

// ---------- Multivue ----------

const multiview = createMultiview($('multiview'), {
  deps: playerDeps,
  alternatives: (channel) => alternativesFor(channel, catalog.all, state.health),
  toast,
  listen,
  onPromote: (channel) => { toggleMultiview(); playChannel(channel); },
});
let multiviewResume = null;
function toggleMultiview() {
  const enabled = !document.body.classList.contains('multiview-mode');
  if (enabled && document.body.classList.contains('player-mode')) togglePlayerMode();
  document.body.classList.toggle('multiview-mode', enabled);
  $('multiview').hidden = !enabled;
  $('multiviewMode').lastChild.textContent = enabled ? 'Quitter la multivue' : 'Multivue';
  if (enabled) {
    const current = player.info?.channel;
    multiviewResume = current && !player.info.catchup ? current : null;
    player.stop();
    if (youtubeSession) hideYoutube();
    if (multiviewResume && multiviewResume.kind === 'live' && !isYouTubePage(multiviewResume.streamUrl)) multiview.add(multiviewResume);
    toast('Multivue : cliquez sur des chaînes de la liste pour les ajouter.');
  } else {
    multiview.clear();
    if (multiviewResume) playChannel(multiviewResume);
    multiviewResume = null;
  }
  scheduleYoutubeBounds();
}

// ---------- AirPlay ----------

/** AirPlay and picture in picture belong to the system player: switch to it first. */
function needsSystemPlayer(feature) {
  if (player.engine !== 'mpv') return false;
  player.force('native');
  toast(`${feature} : passage au lecteur système… Touchez à nouveau le bouton quand l’image apparaît.`);
  return true;
}
if (window.WebKitPlaybackTargetAvailabilityEvent) {
  video.addEventListener('webkitplaybacktargetavailabilitychanged', (event) => { $('airplay').hidden = event.availability !== 'available'; });
  $('airplay').onclick = () => { if (!needsSystemPlayer('AirPlay')) video.webkitShowPlaybackTargetPicker?.(); };
  $('airplay').title = 'AirPlay (les flux passant par le relais local ne sont pas transmissibles)';
}

// ---------- Mises à jour ----------
// Releases are published on GitHub with a signed `latest.json`; the check runs at start-up
// at most every 12 hours, and on demand from the settings.

const UPDATE_INTERVAL = 12 * 3600 * 1000;
function showUpdateBanner(info) {
  $('updateTitle').textContent = `Fluxo ${info.version} est disponible`;
  $('updateDetail').textContent = info.notes ? info.notes.split('\n').find((line) => line.trim())?.replace(/^[#*\-\s]+/, '') || '' : `Version installée : ${info.currentVersion}`;
  $('updateInstall').disabled = false; $('updateLater').disabled = false;
  $('updateBanner').hidden = false;
}
async function checkForUpdates({ silent = false } = {}) {
  try {
    const info = await invoke('check_update');
    try { localStorage.setItem('fluxo-update-check', String(Date.now())); } catch { /* private storage */ }
    if (info) showUpdateBanner(info);
    else if (!silent) toast(`Fluxo ${state.engine.version} est à jour.`);
    return info;
  } catch (error) {
    if (!silent) toast(errorMessage(error), 'error');
    return null;
  }
}
async function installUpdate() {
  $('updateInstall').disabled = true; $('updateLater').disabled = true;
  $('updateDetail').textContent = 'Téléchargement…';
  saveProgressNow();
  const unlisten = await listen('update-progress', (event) => {
    const [received, total] = event.payload;
    $('updateDetail').textContent = total ? `Téléchargement… ${Math.min(100, Math.round(received / total * 100))} %` : `Téléchargement… ${(received / 1_048_576).toFixed(1)} Mo`;
  }).catch(() => null);
  try {
    await invoke('install_update');
    $('updateDetail').textContent = 'Redémarrage…';
  } catch (error) {
    toast(errorMessage(error), 'error');
    $('updateDetail').textContent = 'L’installation a échoué. Réessayez plus tard.';
    $('updateInstall').disabled = false; $('updateLater').disabled = false;
  } finally { unlisten?.(); }
}
function lastUpdateCheck() { try { return Number(localStorage.getItem('fluxo-update-check')) || 0; } catch { return 0; } }
$('updateInstall').onclick = installUpdate;
$('updateLater').onclick = () => { $('updateBanner').hidden = true; };

// ---------- Événements ----------

getCurrentWindow().onResized(async () => {
  syncFullscreen(await getCurrentWindow().isFullscreen());
  scheduleYoutubeBounds();
}).catch(() => {});
new ResizeObserver(scheduleYoutubeBounds).observe($('videoWrap'));
document.querySelector('.player-pane').addEventListener('scroll', scheduleYoutubeBounds, { passive: true });
window.addEventListener('resize', scheduleYoutubeBounds);
$('addPlaylistShortcut').onclick = showAddPlaylist; $('settingsButton').onclick = showSettings; $('recordingsButton').onclick = showRecordings;
$('converterButton').onclick = () => showConverter();
$('playUrl').onclick = showPlayUrl; $('openMedia').onclick = openMedia;
$('playerMode').onclick = togglePlayerMode; $('multiviewMode').onclick = toggleMultiview;
$('renameView').onclick = () => { const playlist = currentPlaylist(); if (playlist) renamePlaylist(playlist); };
$('exportView').onclick = () => { const playlist = currentPlaylist(); if (playlist) showExport([playlist.id]); };
$('filtersButton').onclick = showFilters; $('checkButton').onclick = () => checkVisibleChannels().catch((error) => toast(errorMessage(error), 'error'));
$('queuePrevious').onclick = () => playQueueIndex(state.queueIndex - 1);
$('queueNext').onclick = () => playQueueIndex(state.queueIndex + 1);
$('queueClear').onclick = clearQueue;
$('togglePlay').onclick = togglePause;
$('stopPlayback').onclick = () => stopPlayback();
$('filmstripButton').onclick = toggleFilmstrip;
$('back10').onclick = () => seekBy(-10);
$('forward10').onclick = () => seekBy(10);
$('seek').oninput = (event) => { const current = media(); if (Number.isFinite(current.duration)) current.currentTime = Number(event.target.value) / 1000 * current.duration; };
$('mute').onclick = toggleMute;
$('volume').oninput = (event) => { const current = media(); current.volume = Number(event.target.value); current.muted = current.volume === 0; updateControls(); };
$('subtitleOptions').onclick = showSubtitleOptions;
$('qualityOptions').onclick = showQualityOptions;
$('recordButton').onclick = toggleRecording;
$('playerOptions').onclick = showPlayerOptions;
$('streamInfo').onclick = showStreamInfo;
$('fullscreen').onclick = toggleFullscreen;
$('pip').onclick = async () => {
  try {
    if (needsSystemPlayer('Image dans l’image')) return;
    if (!document.pictureInPictureEnabled || !video.requestPictureInPicture) throw new Error('Image dans l’image non disponible sur ce Mac.');
    if (document.pictureInPictureElement) await document.exitPictureInPicture(); else await video.requestPictureInPicture();
  } catch (error) { toast(errorMessage(error), 'error'); }
};
$('videoWrap').ondblclick = (event) => { if (event.target === video || event.target === $('videoWrap')) toggleFullscreen(); };
// A click on the picture (not on the controls) pauses or resumes, as in most players.
let pictureClickTimer = 0;
$('videoWrap').addEventListener('click', (event) => {
  if ((event.target === video || event.target === $('videoWrap')) && media().getAttribute('src') && event.detail === 1) {
    clearTimeout(pictureClickTimer);
    pictureClickTimer = setTimeout(togglePause, 220);
  }
});
$('videoWrap').addEventListener('dblclick', () => clearTimeout(pictureClickTimer));
$('favoriteButton').onclick = () => state.playing?.channelId && toggleFavorite(state.playing.channelId);
$('modalClose').onclick = closeModal; $('modal').onclick = (event) => { if (event.target === $('modal')) closeModal(); };
let searchTimer = 0;
$('search').oninput = (event) => {
  clearTimeout(searchTimer);
  searchTimer = setTimeout(() => { state.query = event.target.value; $('channels').scrollTop = 0; render(); }, 180);
};
document.querySelectorAll('.main-nav button').forEach((el) => { el.onclick = () => setView(el.dataset.view); });
document.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') {
    if (popupMenu) { closePopupMenu(); return; }
    if (filmstrip.isOpen && $('modal').hidden) { filmstrip.close(); return; }
    closeGroupPanel(); closeModal(); return;
  }
  const target = event.target;
  if (!$('modal').hidden || event.metaKey || event.ctrlKey || event.altKey) return;
  if (target instanceof HTMLElement && (target.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName))) return;
  const hasMedia = Boolean(media().getAttribute('src'));
  const key = event.key;
  if (key === 'ArrowUp' || key === 'PageUp') { event.preventDefault(); zap(-1); return; }
  if (key === 'ArrowDown' || key === 'PageDown') { event.preventDefault(); zap(1); return; }
  if (/^[0-9]$/.test(key)) { event.preventDefault(); zapDigit(key); return; }
  if (key === 'Backspace' && state.previous) { event.preventDefault(); showZap(state.previous.name); playChannel(state.previous); return; }
  if (key.toLowerCase() === 'i' && player.info) { showStreamInfo(); return; }
  if (key.toLowerCase() === 'm') { toggleMute(); return; }
  if (key.toLowerCase() === 's' && (state.playing || player.info)) { event.preventDefault(); stopPlayback(); return; }
  if (key.toLowerCase() === 'p' && player.info) { event.preventDefault(); toggleFilmstrip(); return; }
  if (!hasMedia) return;
  if (key === ' ') { event.preventDefault(); togglePause(); }
  if (key.toLowerCase() === 'f') toggleFullscreen();
  if (key === 'ArrowLeft') seekBy(-10);
  if (key === 'ArrowRight') seekBy(10);
});
for (const target of [video, mainMpv]) {
  for (const name of ['timeupdate', 'loadedmetadata', 'durationchange', 'play', 'pause', 'volumechange', 'emptied']) {
    target.addEventListener(name, () => { if (target === media()) { updateControls(); renderSubtitles(); } });
  }
  for (const name of ['play', 'pause']) {
    target.addEventListener(name, () => { if (target === media()) wakeControls(); });
  }
  target.addEventListener('ended', () => { if (target === media() && state.queueIndex >= 0 && state.queueIndex + 1 < state.queue.length) playQueueIndex(state.queueIndex + 1); });
}
video.textTracks?.addEventListener?.('addtrack', (event) => { event.track.mode = 'disabled'; event.track.addEventListener('cuechange', renderSubtitles); });
/** Marks the zone under the pointer (physical pixels) and returns its action. */
function highlightDropZone(position) {
  const scale = window.devicePixelRatio || 1;
  const x = (position?.x ?? -1) / scale;
  const y = (position?.y ?? -1) / scale;
  let action = null;
  document.querySelectorAll('.drop-zone').forEach((zone) => {
    const rect = zone.getBoundingClientRect();
    const inside = !zone.hidden && x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
    zone.classList.toggle('active', inside);
    if (inside) action = zone.dataset.action;
  });
  return action;
}
getCurrentWindow().onDragDropEvent(async (event) => {
  const { type } = event.payload;
  const converting = converterVisible();
  if (type === 'enter') {
    const count = event.payload.paths?.length || 0;
    $('dropTitle').textContent = converting ? `Convertir ${count > 1 ? `ces ${count} éléments` : 'cet élément'}` : count > 1 ? `${count} éléments à déposer` : 'Déposez votre média';
    $('dropDetail').textContent = converting ? 'Ils rejoignent la liste des fichiers à convertir.' : 'Choisissez l’action en déposant sur l’une des zones.';
    document.querySelectorAll('.drop-zone').forEach((zone) => {
      const action = zone.dataset.action;
      zone.hidden = converting ? action !== 'convert' : action === 'queue' && !state.queue.length;
    });
    $('dropOverlay').classList.toggle('single', converting);
    $('dropOverlay').hidden = !count;
  }
  if (type === 'enter' || type === 'over') highlightDropZone(event.payload.position);
  if (type === 'leave') $('dropOverlay').hidden = true;
  if (type === 'drop') {
    const action = converting ? 'convert' : highlightDropZone(event.payload.position) || 'play';
    $('dropOverlay').hidden = true;
    const paths = event.payload.paths || [];
    if (action === 'convert') { if (converting) converter.view.addFiles(paths); else showConverter(paths); return; }
    try { await importLocalMedia(paths, { append: action === 'queue' }); }
    catch (error) { toast(errorMessage(error), 'error'); }
  }
  scheduleYoutubeBounds();
}).catch((error) => toast(`Glisser-déposer indisponible : ${errorMessage(error)}`, 'error'));
setInterval(() => { if (state.view === 'guide') render(); else if (state.epg.programs) requestNowNext(lastList.slice(0, 200)); }, 60_000);

(async () => {
  try {
    // Video views left by a previous page load (development reloads) are removed.
    await invoke('mpv_destroy_all').catch(() => {});
    const [engine, health] = await Promise.all([invoke('engine_info').catch(() => state.engine), invoke('get_health').catch(() => ({}))]);
    state.engine = engine;
    state.health = health;
    await refreshLibrary();
    await refreshEpgStatus();
    invoke('convert_jobs').then((jobs) => { converter.jobs = new Map(jobs.map((job) => [job.id, job])); updateConverterBadge(); }).catch(() => {});
    if (engine.debugPlay) await playUrl(engine.debugPlay, 'Diagnostic');
    if (preferences.autoUpdate && Date.now() - lastUpdateCheck() > UPDATE_INTERVAL) checkForUpdates({ silent: true });
  } catch (error) { toast(errorMessage(error), 'error'); }
})();
