import { invoke, convertFileSrc } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { readText } from '@tauri-apps/plugin-clipboard-manager';
import { openUrl } from '@tauri-apps/plugin-opener';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { parseSubtitles } from './subtitles.js';
import { isWebUrl, isYouTubePage } from './media-sources.js';
import { isHlsMediaUrl, parseHlsQualities } from './hls-quality.js';
import './style.css';

const app = document.querySelector('#app');
app.innerHTML = `
  <div class="shell">
    <aside class="sidebar">
      <div class="brand"><span class="brand-icon">▶</span><span>Fluxo</span></div>
      <nav class="main-nav" aria-label="Navigation">
        <button data-view="all" class="active"><span class="nav-icon">▦</span> Toutes les chaînes</button>
        <button data-view="favorites"><span class="nav-icon">☆</span> Favoris</button>
        <button data-view="recent"><span class="nav-icon">◷</span> Récents</button>
      </nav>
      <div class="sidebar-heading"><span>PLAYLISTS</span><button id="addPlaylistShortcut" title="Ajouter une playlist" aria-label="Ajouter une playlist">＋</button></div>
      <div id="playlistNav" class="playlist-nav"></div>
      <div class="sidebar-bottom"><button id="settingsButton">⚙ <span>Sources & guide TV</span></button></div>
    </aside>
    <main class="main">
      <header class="topbar"><div><div class="eyebrow">VOTRE LECTEUR</div><h1 id="viewTitle">Toutes les chaînes</h1></div><div class="top-actions"><button id="playerMode" class="subtle" title="Agrandir le lecteur">Mode lecteur</button><button id="openMedia" class="subtle">Ouvrir des médias</button><button id="playUrl" class="primary">＋ Lire une URL</button></div></header>
      <div class="content">
        <section class="catalogue"><div class="search-wrap"><span>⌕</span><input id="search" type="search" placeholder="Rechercher une chaîne ou un groupe…" aria-label="Rechercher"></div><div class="filters"><div id="groupFilters" class="group-filters"></div><span id="channelCount"></span></div><div id="channels" class="channels"></div></section>
        <section class="player-pane"><div class="player-card"><div class="video-wrap" id="videoWrap"><video id="video" playsinline preload="metadata"></video><div id="subtitleOverlay" class="subtitle-overlay" aria-live="off"></div><div id="videoEmpty" class="video-empty"><div class="empty-glyph">▶</div><strong>Prêt à regarder</strong><span>Choisissez une chaîne ou glissez des vidéos ici.</span></div><div class="player-controls" id="playerControls"><input id="seek" type="range" min="0" max="1000" value="0" aria-label="Position de lecture"><div class="controls-row"><button id="togglePlay" title="Lecture / pause" aria-label="Lecture / pause">▶</button><button id="back10" title="Reculer de 10 secondes" aria-label="Reculer de 10 secondes">−10</button><button id="forward10" title="Avancer de 10 secondes" aria-label="Avancer de 10 secondes">+10</button><span id="timeLabel">00:00 / 00:00</span><div class="controls-spacer"></div><button id="mute" title="Couper le son" aria-label="Couper le son">◖))</button><input id="volume" type="range" min="0" max="1" step="0.01" value="1" aria-label="Volume"><button id="subtitleOptions" title="Gérer les sous-titres" aria-label="Gérer les sous-titres" aria-haspopup="dialog">CC</button><button id="qualityOptions" title="Qualité vidéo" aria-label="Qualité vidéo" aria-haspopup="dialog" hidden>Auto</button><button id="playerOptions" title="Options du lecteur" aria-label="Options du lecteur">⚙</button><button id="pip" title="Image dans l’image" aria-label="Image dans l’image">▣</button><button id="fullscreen" title="Plein écran" aria-label="Plein écran">⛶</button></div></div></div><div class="player-meta"><div class="live-indicator" id="liveIndicator">LECTEUR</div><div class="player-title-row"><h2 id="playingTitle">Aucune lecture</h2><button id="favoriteButton" title="Ajouter aux favoris" aria-label="Ajouter aux favoris" hidden>☆</button></div><p id="playingDetail">La vidéo et l’audio se lisent ici.</p><p id="playerError" role="alert"></p></div></div><div id="queueCard" class="queue-card" hidden><div class="queue-head"><div><h3>À suivre</h3><span id="queueCount"></span></div><div class="queue-actions"><button id="queuePrevious" aria-label="Média précédent" title="Média précédent">←</button><button id="queueNext" aria-label="Média suivant" title="Média suivant">→</button><button id="queueClear" title="Effacer la file">Effacer</button></div></div><div id="queueItems" class="queue-items"></div></div><div class="guide-card"><div class="guide-header"><h3>Programme TV</h3><span id="guideStatus">Ajoutez un guide XMLTV</span></div><div id="programs" class="programs"><p class="muted">Sélectionnez une chaîne pour voir son programme.</p></div></div></section>
      </div>
    </main>
  </div>
  <div id="dropOverlay" class="drop-overlay" hidden><div><strong>Déposez vos médias</strong><span>Vidéos, fichiers audio ou dossier de vidéos</span></div></div>
  <div id="modal" class="modal-backdrop" hidden><div class="modal" role="dialog" aria-modal="true" aria-labelledby="modalTitle"><div class="modal-head"><h2 id="modalTitle"></h2><button id="modalClose" class="close" aria-label="Fermer">×</button></div><div id="modalBody"></div></div></div>
  <div id="toast" role="status" aria-live="polite"></div>`;

const $ = (id) => document.getElementById(id);
const state = { library: { playlists: [], favorites: [], recent: [], epgSource: null, xtreamAccounts: [] }, view: 'all', group: 'Tous', query: '', playing: null, subtitleCues: [], subtitleTrack: 'off', queue: [], queueIndex: -1 };
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
let playbackFailureId = 0;
const openSourceExternally = button('Ouvrir dans le navigateur', 'subtle external-action', openCurrentSourceExternally);
openSourceExternally.hidden = true; $('playerError').after(openSourceExternally);
let toastTimer;
const subtitleSettings = (() => {
  try { return { size: 100, position: 8, ...JSON.parse(localStorage.getItem('fluxo-subtitles') || '{}') }; }
  catch { return { size: 100, position: 8 }; }
})();
function applySubtitleSettings() {
  const size = Math.max(60, Math.min(200, Number(subtitleSettings.size) || 100));
  const position = Math.max(2, Math.min(40, Number(subtitleSettings.position) || 8));
  $('subtitleOverlay').style.fontSize = `${size}%`;
  $('subtitleOverlay').style.bottom = `${position}%`;
  localStorage.setItem('fluxo-subtitles', JSON.stringify({ size, position }));
}
applySubtitleSettings();

function toast(message, kind = '') {
  const el = $('toast'); el.textContent = message; el.className = kind; el.classList.add('show');
  clearTimeout(toastTimer); toastTimer = setTimeout(() => el.classList.remove('show'), 4200);
}
function errorMessage(error) { return typeof error === 'string' ? error : error?.message || String(error); }
function playbackErrorMessage(error) {
  switch (video.error?.code) {
    case 2: return 'Une erreur réseau a interrompu la lecture.';
    case 3: return 'Le flux reçu ne peut pas être décodé par macOS.';
    case 4: return 'Le format de ce flux n’est pas reconnu par macOS.';
    default: return error?.name === 'NotSupportedError'
      ? 'Ce format n’est pas pris en charge par macOS.'
      : 'La lecture de ce flux a échoué.';
  }
}
function showPlaybackFailure(error) {
  const requestId = ++playbackFailureId;
  const playing = state.playing;
  const source = video.currentSrc || video.src;
  const fallback = playbackErrorMessage(error);
  openSourceExternally.hidden = !isWebUrl(playing?.url);
  if (!isWebUrl(source)) { $('playerError').textContent = fallback; return; }
  $('playerError').textContent = 'Vérification de la réponse du serveur…';
  invoke('diagnose_stream', { source }).then((diagnosis) => {
    if (requestId === playbackFailureId && playing === state.playing) $('playerError').textContent = diagnosis || fallback;
  }).catch(() => {
    if (requestId === playbackFailureId && playing === state.playing) $('playerError').textContent = fallback;
  });
}
function youtubeBounds() {
  if (!$('modal').hidden || !$('dropOverlay').hidden) return null;
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
  const url = state.playing?.url;
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
    youtubeRetry.textContent = 'Réessayer';
  } catch (error) {
    if (state.playing !== playing) return;
    youtubeStatus.textContent = 'Direct YouTube indisponible';
    youtubeDetail.textContent = errorMessage(error);
    youtubeRetry.textContent = 'Réessayer';
  } finally { youtubeRetry.disabled = false; }
}
function resetQuality(source = null) {
  quality.request += 1;
  quality.switch += 1;
  quality.source = source;
  quality.variants = [];
  quality.selected = 'auto';
  quality.resumeTime = null;
  quality.resumePlaying = false;
  $('qualityOptions').hidden = true;
  $('qualityOptions').textContent = 'Auto';
}
function updateQualityButton() {
  const button = $('qualityOptions');
  button.hidden = quality.variants.length === 0;
  const selected = quality.variants.find((variant) => variant.url === quality.selected);
  button.textContent = selected ? (selected.height >= 2160 ? '4K' : selected.height ? `${selected.height}p` : 'Qualité') : 'Auto';
  button.title = selected ? `Qualité vidéo : ${selected.label}` : 'Qualité vidéo : automatique';
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
  quality.selected = selected;
  quality.resumeTime = time;
  quality.resumePlaying = wasPlaying;
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
  const resume = { time: quality.resumeTime, playing: quality.resumePlaying };
  switchQuality('auto', resume);
  toast('Cette qualité ne peut pas être lue. Retour au réglage automatique.', 'error');
}
function showQualityOptions() {
  if (!quality.variants.length) return;
  modal('Qualité vidéo', (body) => {
    const choices = [['auto', 'Automatique']];
    choices.push(...quality.variants.filter((variant) => variant.selectable).map((variant) => [variant.url, variant.label]));
    body.append(selectField('Résolution', choices, quality.selected, (selected) => { closeModal(); switchQuality(selected); }));
    const separate = quality.variants.some((variant) => !variant.selectable);
    body.append(make('p', 'muted', separate
      ? 'Les qualités avec pistes audio ou sous-titres séparés restent en mode automatique pour conserver ces pistes.'
      : 'Les qualités affichées sont celles annoncées par le flux. Le changement recharge brièvement la vidéo.'));
  });
}
async function refreshLibrary() { state.library = await invoke('get_library'); render(); }
function allChannels() { return state.library.playlists.flatMap((playlist) => playlist.channels); }
function sortedMedia(channels) { return [...channels].sort((a, b) => a.name.localeCompare(b.name, 'fr', { numeric: true, sensitivity: 'base' })); }
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
  await playChannel(state.queue[index], true);
}
function currentChannels() {
  if (state.view === 'recent') return state.library.recent.map((item) => allChannels().find((channel) => channel.streamUrl === item.url) || ({ id: `recent|${item.url}`, name: item.name, group: 'Récents', streamUrl: item.url, kind: item.url.includes('/episode/') ? 'episode' : 'live', containerExtension: item.containerExtension }));
  let channels = allChannels();
  if (state.view === 'favorites') channels = channels.filter((item) => state.library.favorites.includes(item.id));
  else if (state.view !== 'all') channels = state.library.playlists.find((item) => item.id === state.view)?.channels || [];
  return channels;
}
function filteredChannels() {
  const query = state.query.toLocaleLowerCase('fr');
  return currentChannels().filter((item) => (state.group === 'Tous' || item.group === state.group) && (!query || `${item.name} ${item.group}`.toLocaleLowerCase('fr').includes(query)));
}
function make(tag, className, text) { const el = document.createElement(tag); if (className) el.className = className; if (text !== undefined) el.textContent = text; return el; }
function render() {
  document.querySelectorAll('.main-nav button').forEach((button) => button.classList.toggle('active', button.dataset.view === state.view));
  const playlistNav = $('playlistNav'); playlistNav.replaceChildren();
  for (const playlist of state.library.playlists) {
    const button = make('button', state.view === playlist.id ? 'active' : '', playlist.name);
    button.title = playlist.name; button.onclick = () => setView(playlist.id); playlistNav.append(button);
  }
  $('viewTitle').textContent = state.view === 'all' ? 'Toutes les chaînes' : state.view === 'favorites' ? 'Favoris' : state.view === 'recent' ? 'Récents' : state.library.playlists.find((item) => item.id === state.view)?.name || 'Playlist';
  const groups = ['Tous', ...new Set(currentChannels().map((item) => item.group))];
  if (!groups.includes(state.group)) state.group = 'Tous';
  const filters = $('groupFilters'); filters.replaceChildren();
  for (const group of groups) { const button = make('button', state.group === group ? 'selected' : '', group); button.onclick = () => { state.group = group; $('channels').scrollTop = 0; render(); }; filters.append(button); }
  const channels = filteredChannels(); $('channelCount').textContent = `${channels.length} élément${channels.length > 1 ? 's' : ''}`;
  const list = $('channels'); const previousScroll = list.scrollTop; list.replaceChildren(); list.onscroll = null;
  if (!channels.length) {
    const empty = make('div', 'list-empty');
    empty.append(make('div', 'empty-icon', state.library.playlists.length ? '⌕' : '＋'), make('strong', '', state.library.playlists.length ? 'Aucun résultat' : 'Ajoutez votre première playlist'), make('p', '', state.library.playlists.length ? 'Essayez une autre recherche ou un autre groupe.' : 'Importez un fichier M3U ou collez son adresse pour afficher vos chaînes.'));
    if (!state.library.playlists.length) { const button = make('button', 'primary', 'Ajouter une playlist'); button.onclick = showAddPlaylist; empty.append(button); }
    list.append(empty);
  }
  if (channels.length) {
    let cursor = 0;
    const appendBatch = (count = 120) => {
      const fragment = document.createDocumentFragment();
      for (const channel of channels.slice(cursor, cursor + count)) {
        const row = make('div', `channel-row${state.playing?.url === channel.streamUrl ? ' is-playing' : ''}`);
        const play = make('button', 'channel-main');
        const logo = make('span', 'channel-logo', channel.name.slice(0, 1).toUpperCase());
        if (channel.logo?.startsWith('https://') || channel.logo?.startsWith('http://')) { const image = document.createElement('img'); image.src = channel.logo; image.alt = ''; image.onerror = () => image.remove(); logo.prepend(image); }
        const text = make('span', 'channel-text'); text.append(make('strong', '', channel.name), make('small', '', channel.group));
        play.append(logo, text); play.onclick = () => playChannel(channel);
        const favorite = make('button', 'star', state.library.favorites.includes(channel.id) ? '★' : '☆'); favorite.title = 'Favori'; favorite.setAttribute('aria-label', 'Favori'); favorite.onclick = () => toggleFavorite(channel.id);
        row.append(play); if (state.view !== 'recent') row.append(favorite); fragment.append(row);
      }
      cursor = Math.min(cursor + count, channels.length);
      list.append(fragment);
    };
    const initialCount = Math.max(120, Math.ceil((previousScroll + list.clientHeight + 300) / 60 / 120) * 120);
    appendBatch(initialCount);
    list.scrollTop = previousScroll;
    list.onscroll = () => { if (cursor < channels.length && list.scrollHeight - list.scrollTop - list.clientHeight < 300) appendBatch(); };
  }
  $('favoriteButton').hidden = !state.playing?.channelId;
  if (state.playing?.channelId) $('favoriteButton').textContent = state.library.favorites.includes(state.playing.channelId) ? '★' : '☆';
  $('guideStatus').textContent = state.library.epgSource ? 'XMLTV connecté' : 'Ajoutez un guide XMLTV';
}
function setView(view) { state.view = view; state.group = 'Tous'; state.query = ''; $('search').value = ''; $('channels').scrollTop = 0; render(); }
async function toggleFavorite(id) { try { await invoke('toggle_favorite', { id }); await refreshLibrary(); } catch (error) { toast(errorMessage(error), 'error'); } }
async function playChannel(channel, fromQueue = false) {
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
    let url = channel.streamUrl;
    if (url.startsWith('xtream://')) url = await invoke('resolve_stream', { reference: url, extension: channel.containerExtension || null });
    if (url.startsWith('file:')) {
      const path = decodeURIComponent(new URL(url).pathname);
      url = convertFileSrc(await invoke('allow_media_file', { path }));
    }
    await playSource(url, channel.name, channel.id.startsWith('recent|') ? null : channel.id, channel.tvgId, channel.streamUrl, channel.kind, channel.containerExtension);
    return true;
  } catch (error) { toast(errorMessage(error), 'error'); return false; }
}
async function playSource(url, name, channelId = null, tvgId = null, recentUrl = url, kind = 'movie', extension = null) {
  playbackFailureId += 1;
  resetQuality(isHlsMediaUrl(url) ? url : null);
  video.pause(); video.removeAttribute('src'); video.load();
  $('videoEmpty').hidden = true; $('playerError').textContent = ''; openSourceExternally.hidden = true;
  const onYouTube = isYouTubePage(url);
  if (!onYouTube) {
    youtubeSession = 0;
    await invoke('hide_youtube_player').catch((error) => { $('playerError').textContent = errorMessage(error); });
  }
  $('videoWrap').classList.toggle('youtube-source', onYouTube);
  youtubePlayback.hidden = !onYouTube;
  state.subtitleCues = []; selectSubtitleTrack('off');
  state.playing = { url: recentUrl, name, channelId, tvgId }; $('playingTitle').textContent = name;
  $('playingDetail').textContent = recentUrl.startsWith('file:') ? 'Fichier local' : recentUrl.startsWith('xtream:') ? 'Catalogue Xtream' : recentUrl;
  $('liveIndicator').textContent = kind === 'live' && /\.m3u8?(?:[?#]|$)/i.test(url) ? 'DIRECT / HLS' : kind === 'live' ? 'DIRECT' : 'LECTEUR';
  if (onYouTube) await openCurrentYouTube();
  else {
    video.src = url; video.load();
    if (quality.source) void discoverQualities(url, state.playing);
    const playing = state.playing;
    try { await video.play(); }
    catch (error) {
      if (state.playing === playing && !video.error && !['NotAllowedError', 'AbortError'].includes(error?.name)) showPlaybackFailure(error);
    }
  }
  try { await invoke('record_recent', { name, url: recentUrl, extension }); await refreshLibrary(); } catch (error) { toast(errorMessage(error), 'error'); }
  renderPrograms(tvgId);
}
async function showEpisodes(channel) {
  modal(channel.name, (body) => body.append(make('p', 'muted', 'Chargement des épisodes…')));
  try {
    const episodes = await invoke('get_series_episodes', { reference: channel.streamUrl });
    const body = $('modalBody'); body.replaceChildren();
    if (!episodes.length) { body.append(make('p', 'muted', 'Aucun épisode disponible.')); return; }
    for (const episode of episodes) {
      body.append(button(`Saison ${episode.season} · ${episode.name}`, 'episode-button', async () => {
        closeModal();
        await playChannel({ id: episode.streamUrl, name: episode.name, group: channel.group, streamUrl: episode.streamUrl, kind: 'episode', containerExtension: episode.extension });
      }));
    }
  } catch (error) { $('modalBody').replaceChildren(make('p', 'muted', errorMessage(error))); }
}
async function renderPrograms(tvgId) {
  const box = $('programs'); box.replaceChildren();
  if (!state.library.epgSource) { box.append(make('p', 'muted', 'Ajoutez un guide XMLTV pour afficher les programmes.')); return; }
  if (!tvgId) { box.append(make('p', 'muted', 'Aucun identifiant de guide pour cette chaîne.')); return; }
  try {
    const programs = await invoke('get_programs', { channelId: tvgId });
    if (!programs.length) { box.append(make('p', 'muted', 'Aucun programme disponible pour les prochaines 24 heures.')); return; }
    for (const program of programs) {
      const row = make('div', 'program');
      const time = new Date(program.start * 1000).toLocaleTimeString('fr-FR', { hour: '2-digit', minute: '2-digit' });
      row.append(make('span', 'program-time', time), make('div', 'program-text'));
      row.lastChild.append(make('strong', '', program.title), make('small', '', program.description)); box.append(row);
    }
  } catch (error) { box.append(make('p', 'muted', errorMessage(error))); }
}
function modal(title, build) { $('modalTitle').textContent = title; const body = $('modalBody'); body.replaceChildren(); build(body); $('modal').hidden = false; scheduleYoutubeBounds(); const first = body.querySelector('input'); first?.focus(); }
function closeModal() { $('modal').hidden = true; scheduleYoutubeBounds(); }
function field(labelText, placeholder, value = '') { const label = make('label', 'field'); label.append(make('span', '', labelText)); const input = document.createElement('input'); input.placeholder = placeholder; input.value = value; label.append(input); return { label, input }; }
function button(text, className, action) { const el = make('button', className, text); el.onclick = action; return el; }
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
function showAddPlaylist() {
  modal('Ajouter une playlist', (body) => {
    body.append(make('p', 'modal-intro', 'Collez l’adresse d’une playlist M3U ou choisissez un fichier local.'));
    const name = field('Nom', 'Ex. Mes chaînes'); const source = pasteField('Adresse M3U ou fichier', 'https://…/playlist.m3u');
    body.append(name.label, source.label);
    body.append(button('Choisir un fichier M3U', 'subtle full', async () => { const path = await open({ multiple: false, filters: [{ name: 'Playlist M3U', extensions: ['m3u', 'm3u8'] }] }); if (path) { source.input.value = path; if (!name.input.value) name.input.value = path.split('/').pop().replace(/\.m3u8?$/i, ''); } }));
    body.append(button('Importer la playlist', 'primary full', async (event) => { const target = event.currentTarget; target.disabled = true; target.textContent = 'Importation…'; try { const playlist = await invoke('add_playlist', { name: name.input.value, source: source.input.value }); closeModal(); await refreshLibrary(); setView(playlist.id); toast(`${playlist.channels.length} chaînes importées.`); } catch (error) { toast(errorMessage(error), 'error'); } finally { target.disabled = false; target.textContent = 'Importer la playlist'; } }));
  });
}
function showAddXtream() {
  modal('Connecter Xtream Codes', (body) => {
    body.append(make('p', 'modal-intro', 'Renseignez les identifiants fournis par votre service. Le mot de passe est conservé dans le trousseau macOS.'));
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
    body.append(make('p', 'modal-intro', 'Ouvrez un flux HLS, une vidéo, un fichier audio ou une playlist M3U en ligne.'));
    const source = pasteField('Adresse du média', 'https://…/video.m3u8'); body.append(source.label);
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
        closeModal(); clearQueue(); await playSource(value, url.pathname.split('/').pop() || 'Média en ligne');
      } catch (error) { toast(errorMessage(error), 'error'); }
      finally { target.disabled = false; target.textContent = 'Ouvrir l’adresse'; }
    }));
  });
}
function showSettings() {
  modal('Sources & guide TV', (body) => {
    body.append(make('p', 'modal-intro', 'Gérez vos playlists, comptes Xtream et le guide des programmes.'));
    body.append(button('＋ Ajouter une playlist M3U', 'subtle full', showAddPlaylist));
    body.append(button('＋ Connecter Xtream Codes', 'subtle full', showAddXtream));
    const epg = pasteField('Guide TV XMLTV', 'Adresse HTTPS ou fichier XML', state.library.epgSource || ''); body.append(epg.label);
    body.append(button('Choisir un fichier XMLTV', 'subtle full', async () => { const path = await open({ multiple: false, filters: [{ name: 'Guide XMLTV', extensions: ['xml'] }] }); if (path) epg.input.value = path; }));
    body.append(button('Enregistrer le guide TV', 'primary full', async (event) => { const target = event.currentTarget; target.disabled = true; try { const count = await invoke('set_epg_source', { source: epg.input.value }); await refreshLibrary(); toast(`${count} programmes chargés.`); if (state.playing?.tvgId) renderPrograms(state.playing.tvgId); } catch (error) { toast(errorMessage(error), 'error'); } finally { target.disabled = false; } }));
    const heading = make('h3', 'settings-heading', 'Playlists'); body.append(heading);
    for (const playlist of state.library.playlists) {
      const row = make('div', 'settings-row'); const info = make('div'); info.append(make('strong', '', playlist.name), make('small', '', `${playlist.channels.length} média${playlist.channels.length > 1 ? 's' : ''}`));
      row.append(info, button('↻', 'icon-button', async () => { try { const updated = await invoke('refresh_playlist', { id: playlist.id }); await refreshLibrary(); toast(`${updated.channels.length} médias actualisés.`); } catch (error) { toast(errorMessage(error), 'error'); } }), button('×', 'icon-button danger', async () => { if (!window.confirm(`Supprimer « ${playlist.name} » ?`)) return; await invoke('remove_playlist', { id: playlist.id }); if (playlist.id === 'local-media') { clearQueue(); if (state.playing?.url.startsWith('file:')) state.playing.channelId = null; } if (state.view === playlist.id) state.view = 'all'; await refreshLibrary(); showSettings(); })); body.append(row);
    }
  });
}
async function openMedia() {
  try { const paths = await open({ multiple: true, filters: [{ name: 'Audio et vidéo', extensions: ['mp4', 'm4v', 'mov', 'mp3', 'm4a', 'aac', 'wav', 'aiff', 'aif', 'webm', 'mkv', 'flac', 'ogg', 'opus', 'ts', 'avi', 'mpg', 'mpeg'] }] }); if (!paths) return; await importLocalMedia(Array.isArray(paths) ? paths : [paths]); }
  catch (error) { toast(errorMessage(error), 'error'); }
}
async function importLocalMedia(paths) {
  $('dropOverlay').hidden = true;
  if (!paths.length) return;
  const result = await invoke('import_local_media', { paths });
  await refreshLibrary(); setView(result.playlist.id);
  state.queue = sortedMedia(result.channels); state.queueIndex = 0; renderQueue();
  await playQueueIndex(0);
  const extra = result.truncated ? ' Limite de 500 médias atteinte.' : result.skipped ? ` ${result.skipped} fichier${result.skipped > 1 ? 's' : ''} ignoré${result.skipped > 1 ? 's' : ''}.` : '';
  toast(`${result.channels.length} média${result.channels.length > 1 ? 's' : ''} ajouté${result.channels.length > 1 ? 's' : ''}.${extra}`);
}
function timeText(value) {
  if (!Number.isFinite(value)) return '00:00';
  const hours = Math.floor(value / 3600); const minutes = Math.floor(value / 60) % 60; const seconds = Math.floor(value) % 60;
  return `${hours ? `${String(hours).padStart(2, '0')}:` : ''}${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
}
function updateControls() {
  const duration = video.duration;
  const seekable = Number.isFinite(duration) && duration > 0;
  $('seek').disabled = !seekable;
  $('back10').disabled = !seekable; $('forward10').disabled = !seekable;
  $('seek').value = seekable ? Math.round(video.currentTime / duration * 1000) : 0;
  $('timeLabel').textContent = seekable ? `${timeText(video.currentTime)} / ${timeText(duration)}` : video.src ? 'DIRECT' : '00:00 / 00:00';
  $('togglePlay').textContent = video.paused ? '▶' : 'Ⅱ';
  $('mute').textContent = video.muted || video.volume === 0 ? '◖×' : '◖))';
  $('volume').value = video.muted ? 0 : video.volume;
}
function renderSubtitles() {
  const overlay = $('subtitleOverlay');
  if (state.subtitleTrack === 'external') {
    overlay.textContent = state.subtitleCues.filter((cue) => cue.start <= video.currentTime && cue.end >= video.currentTime).map((cue) => cue.text).join('\n');
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
function appendSubtitleOptions(body, reopen) {
  const tracks = [['off', 'Désactivés']];
  if (state.subtitleCues.length) tracks.push(['external', 'Fichier externe']);
  Array.from(video.textTracks).forEach((track, index) => tracks.push([`native:${index}`, track.label || track.language || `Piste ${index + 1}`]));
  body.append(selectField('Sous-titres', tracks, state.subtitleTrack, selectSubtitleTrack));
  body.append(button('Importer un fichier SRT ou VTT', 'subtle full', () => loadSubtitleFile(reopen)));
  body.append(rangeField('Taille des sous-titres', 60, 200, subtitleSettings.size, '%', (value) => { subtitleSettings.size = value; applySubtitleSettings(); }));
  body.append(rangeField('Hauteur depuis le bas', 2, 40, subtitleSettings.position, '%', (value) => { subtitleSettings.position = value; applySubtitleSettings(); }));
}
function showSubtitleOptions() {
  modal('Sous-titres', (body) => appendSubtitleOptions(body, showSubtitleOptions));
}
function showPlayerOptions() {
  modal('Options du lecteur', (body) => {
    body.append(selectField('Vitesse', [['0.5', '0,5×'], ['0.75', '0,75×'], ['1', 'Normale'], ['1.25', '1,25×'], ['1.5', '1,5×'], ['2', '2×']], String(video.playbackRate), (value) => { video.playbackRate = Number(value); }));
    appendSubtitleOptions(body, showPlayerOptions);
    const audioTracks = video.audioTracks;
    if (audioTracks?.length > 1) {
      body.append(selectField('Piste audio', Array.from(audioTracks).map((track, index) => [String(index), track.label || track.language || `Piste ${index + 1}`]), String(Array.from(audioTracks).findIndex((track) => track.enabled)), (selected) => {
        Array.from(audioTracks).forEach((track, index) => { track.enabled = index === Number(selected); });
      }));
    }
    body.append(make('p', 'muted', 'Les pistes proposées dépendent du média et du moteur multimédia macOS.'));
  });
}
async function toggleFullscreen() {
  try {
    const window = getCurrentWindow();
    const next = !(await window.isFullscreen());
    await window.setFullscreen(next);
    document.body.classList.toggle('native-fullscreen', next);
    scheduleYoutubeBounds();
  } catch (error) { toast(`Plein écran indisponible : ${errorMessage(error)}`, 'error'); }
}
function togglePlayerMode() {
  const enabled = document.body.classList.toggle('player-mode');
  $('playerMode').textContent = enabled ? 'Afficher le catalogue' : 'Mode lecteur';
  scheduleYoutubeBounds();
}
getCurrentWindow().onResized(async () => {
  document.body.classList.toggle('native-fullscreen', await getCurrentWindow().isFullscreen());
  scheduleYoutubeBounds();
}).catch(() => {});
new ResizeObserver(scheduleYoutubeBounds).observe($('videoWrap'));
document.querySelector('.player-pane').addEventListener('scroll', scheduleYoutubeBounds, { passive: true });
window.addEventListener('resize', scheduleYoutubeBounds);
$('addPlaylistShortcut').onclick = showAddPlaylist; $('settingsButton').onclick = showSettings; $('playUrl').onclick = showPlayUrl; $('openMedia').onclick = openMedia;
$('playerMode').onclick = togglePlayerMode;
$('queuePrevious').onclick = () => playQueueIndex(state.queueIndex - 1);
$('queueNext').onclick = () => playQueueIndex(state.queueIndex + 1);
$('queueClear').onclick = clearQueue;
$('togglePlay').onclick = () => { if (video.paused) video.play(); else video.pause(); };
$('back10').onclick = () => { video.currentTime = Math.max(0, video.currentTime - 10); };
$('forward10').onclick = () => { video.currentTime = Math.min(video.duration, video.currentTime + 10); };
$('seek').oninput = (event) => { if (Number.isFinite(video.duration)) video.currentTime = Number(event.target.value) / 1000 * video.duration; };
$('mute').onclick = () => { video.muted = !video.muted; updateControls(); };
$('volume').oninput = (event) => { video.volume = Number(event.target.value); video.muted = video.volume === 0; updateControls(); };
$('subtitleOptions').onclick = showSubtitleOptions;
$('qualityOptions').onclick = showQualityOptions;
$('playerOptions').onclick = showPlayerOptions;
$('fullscreen').onclick = toggleFullscreen;
$('pip').onclick = async () => {
  try {
    if (!document.pictureInPictureEnabled || !video.requestPictureInPicture) throw new Error('Image dans l’image non disponible sur ce Mac.');
    if (document.pictureInPictureElement) await document.exitPictureInPicture(); else await video.requestPictureInPicture();
  } catch (error) { toast(errorMessage(error), 'error'); }
};
$('videoWrap').ondblclick = (event) => { if (event.target === video) toggleFullscreen(); };
$('favoriteButton').onclick = () => state.playing?.channelId && toggleFavorite(state.playing.channelId);
$('modalClose').onclick = closeModal; $('modal').onclick = (event) => { if (event.target === $('modal')) closeModal(); };
$('search').oninput = (event) => { state.query = event.target.value; $('channels').scrollTop = 0; render(); };
document.querySelectorAll('.main-nav button').forEach((button) => button.onclick = () => setView(button.dataset.view));
document.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') closeModal();
  if (event.target !== document.body || !video.src) return;
  if (event.key === ' ') { event.preventDefault(); if (video.paused) video.play(); else video.pause(); }
  if (event.key.toLowerCase() === 'f') toggleFullscreen();
  if (event.key === 'ArrowLeft' && Number.isFinite(video.duration)) video.currentTime = Math.max(0, video.currentTime - 10);
  if (event.key === 'ArrowRight' && Number.isFinite(video.duration)) video.currentTime = Math.min(video.duration, video.currentTime + 10);
});
for (const name of ['timeupdate', 'loadedmetadata', 'durationchange', 'play', 'pause', 'volumechange']) video.addEventListener(name, () => { updateControls(); renderSubtitles(); });
video.addEventListener('playing', () => { playbackFailureId += 1; $('playerError').textContent = ''; openSourceExternally.hidden = true; updateControls(); });
video.addEventListener('ended', () => { if (state.queueIndex >= 0 && state.queueIndex + 1 < state.queue.length) playQueueIndex(state.queueIndex + 1); });
video.textTracks?.addEventListener?.('addtrack', (event) => { event.track.mode = 'disabled'; event.track.addEventListener('cuechange', renderSubtitles); });
video.addEventListener('error', () => {
  if (!video.src || $('videoWrap').classList.contains('youtube-source')) return;
  if (quality.selected !== 'auto') { fallbackQuality(); return; }
  showPlaybackFailure();
});
getCurrentWindow().onDragDropEvent(async (event) => {
  const { type } = event.payload;
  if (type === 'enter') $('dropOverlay').hidden = !event.payload.paths?.length;
  if (type === 'leave') $('dropOverlay').hidden = true;
  if (type === 'drop') {
    $('dropOverlay').hidden = true;
    try { await importLocalMedia(event.payload.paths); }
    catch (error) { toast(errorMessage(error), 'error'); }
  }
  scheduleYoutubeBounds();
}).catch((error) => toast(`Glisser-déposer indisponible : ${errorMessage(error)}`, 'error'));
refreshLibrary().catch((error) => toast(errorMessage(error), 'error'));
