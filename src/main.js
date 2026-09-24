import { invoke, convertFileSrc } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
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
      <header class="topbar"><div><div class="eyebrow">VOTRE LECTEUR</div><h1 id="viewTitle">Toutes les chaînes</h1></div><div class="top-actions"><button id="openMedia" class="subtle">Ouvrir un média</button><button id="playUrl" class="primary">＋ Lire une URL</button></div></header>
      <div class="content">
        <section class="catalogue"><div class="search-wrap"><span>⌕</span><input id="search" type="search" placeholder="Rechercher une chaîne ou un groupe…" aria-label="Rechercher"></div><div class="filters"><div id="groupFilters" class="group-filters"></div><span id="channelCount"></span></div><div id="channels" class="channels"></div></section>
        <section class="player-pane"><div class="player-card"><div class="video-wrap"><video id="video" controls playsinline preload="metadata"></video><div id="videoEmpty" class="video-empty"><div class="empty-glyph">▶</div><strong>Prêt à regarder</strong><span>Choisissez une chaîne ou ouvrez un média.</span></div></div><div class="player-meta"><div class="live-indicator" id="liveIndicator">LECTEUR</div><div class="player-title-row"><h2 id="playingTitle">Aucune lecture</h2><button id="favoriteButton" title="Ajouter aux favoris" aria-label="Ajouter aux favoris" hidden>☆</button></div><p id="playingDetail">La vidéo et l’audio se lisent ici.</p><p id="playerError" role="alert"></p></div></div><div class="guide-card"><div class="guide-header"><h3>Programme TV</h3><span id="guideStatus">Ajoutez un guide XMLTV</span></div><div id="programs" class="programs"><p class="muted">Sélectionnez une chaîne pour voir son programme.</p></div></div></section>
      </div>
    </main>
  </div>
  <div id="modal" class="modal-backdrop" hidden><div class="modal" role="dialog" aria-modal="true" aria-labelledby="modalTitle"><div class="modal-head"><h2 id="modalTitle"></h2><button id="modalClose" class="close" aria-label="Fermer">×</button></div><div id="modalBody"></div></div></div>
  <div id="toast" role="status" aria-live="polite"></div>`;

const $ = (id) => document.getElementById(id);
const state = { library: { playlists: [], favorites: [], recent: [], epgSource: null }, view: 'all', group: 'Tous', query: '', playing: null };
const video = $('video');
let toastTimer;

function toast(message, kind = '') {
  const el = $('toast'); el.textContent = message; el.className = kind; el.classList.add('show');
  clearTimeout(toastTimer); toastTimer = setTimeout(() => el.classList.remove('show'), 4200);
}
function errorMessage(error) { return typeof error === 'string' ? error : error?.message || String(error); }
async function refreshLibrary() { state.library = await invoke('get_library'); render(); }
function allChannels() { return state.library.playlists.flatMap((playlist) => playlist.channels); }
function currentChannels() {
  if (state.view === 'recent') return state.library.recent.map((item) => ({ id: `recent|${item.url}`, name: item.name, group: 'Récents', streamUrl: item.url }));
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
  const channels = filteredChannels(); $('channelCount').textContent = `${channels.length} chaîne${channels.length > 1 ? 's' : ''}`;
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
        row.append(play, favorite); fragment.append(row);
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
async function playChannel(channel) {
  try {
    let url = channel.streamUrl;
    if (url.startsWith('file:')) {
      const path = decodeURIComponent(new URL(url).pathname);
      url = convertFileSrc(await invoke('allow_media_file', { path }));
    }
    await playSource(url, channel.name, channel.id, channel.tvgId);
  } catch (error) { toast(errorMessage(error), 'error'); }
}
async function playSource(url, name, channelId = null, tvgId = null) {
  video.pause(); video.removeAttribute('src'); video.load();
  $('videoEmpty').hidden = true; $('playerError').textContent = '';
  state.playing = { url, name, channelId, tvgId }; $('playingTitle').textContent = name;
  $('playingDetail').textContent = url.startsWith('asset:') || url.includes('asset.localhost') ? 'Fichier local' : url;
  $('liveIndicator').textContent = /\.m3u8?(?:[?#]|$)/i.test(url) ? 'DIRECT / HLS' : 'LECTEUR';
  video.src = url; video.load();
  try { await video.play(); } catch (error) { if (error?.name !== 'NotAllowedError') $('playerError').textContent = 'Lecture impossible. Vérifiez le format ou l’adresse du flux.'; }
  try { await invoke('record_recent', { name, url }); await refreshLibrary(); } catch (error) { toast(errorMessage(error), 'error'); }
  renderPrograms(tvgId);
}
async function renderPrograms(tvgId) {
  const box = $('programs'); box.replaceChildren();
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
function modal(title, build) { $('modalTitle').textContent = title; const body = $('modalBody'); body.replaceChildren(); build(body); $('modal').hidden = false; const first = body.querySelector('input'); first?.focus(); }
function closeModal() { $('modal').hidden = true; }
function field(labelText, placeholder, value = '') { const label = make('label', 'field'); label.append(make('span', '', labelText)); const input = document.createElement('input'); input.placeholder = placeholder; input.value = value; label.append(input); return { label, input }; }
function button(text, className, action) { const el = make('button', className, text); el.onclick = action; return el; }
function showAddPlaylist() {
  modal('Ajouter une playlist', (body) => {
    body.append(make('p', 'modal-intro', 'Collez l’adresse d’une playlist M3U ou choisissez un fichier local.'));
    const name = field('Nom', 'Ex. Mes chaînes'); const source = field('Adresse M3U ou fichier', 'https://…/playlist.m3u');
    body.append(name.label, source.label);
    body.append(button('Choisir un fichier M3U', 'subtle full', async () => { const path = await open({ multiple: false, filters: [{ name: 'Playlist M3U', extensions: ['m3u', 'm3u8'] }] }); if (path) { source.input.value = path; if (!name.input.value) name.input.value = path.split('/').pop().replace(/\.m3u8?$/i, ''); } }));
    body.append(button('Importer la playlist', 'primary full', async (event) => { const target = event.currentTarget; target.disabled = true; target.textContent = 'Importation…'; try { const playlist = await invoke('add_playlist', { name: name.input.value, source: source.input.value }); closeModal(); await refreshLibrary(); setView(playlist.id); toast(`${playlist.channels.length} chaînes importées.`); } catch (error) { toast(errorMessage(error), 'error'); } finally { target.disabled = false; target.textContent = 'Importer la playlist'; } }));
  });
}
function showPlayUrl() {
  modal('Lire une URL', (body) => {
    body.append(make('p', 'modal-intro', 'Ouvrez un flux HLS, une vidéo ou un fichier audio en ligne.'));
    const source = field('Adresse du média', 'https://…/video.m3u8'); body.append(source.label);
    body.append(button('Lancer la lecture', 'primary full', async () => { const value = source.input.value.trim(); if (!/^https?:\/\//i.test(value)) { toast('Entrez une adresse HTTP ou HTTPS.', 'error'); return; } closeModal(); await playSource(value, new URL(value).pathname.split('/').pop() || 'Média en ligne'); }));
  });
}
function showSettings() {
  modal('Sources & guide TV', (body) => {
    body.append(make('p', 'modal-intro', 'Gérez vos playlists et ajoutez un guide des programmes XMLTV.'));
    const epg = field('Guide TV XMLTV', 'Adresse HTTPS ou fichier XML', state.library.epgSource || ''); body.append(epg.label);
    body.append(button('Choisir un fichier XMLTV', 'subtle full', async () => { const path = await open({ multiple: false, filters: [{ name: 'Guide XMLTV', extensions: ['xml'] }] }); if (path) epg.input.value = path; }));
    body.append(button('Enregistrer le guide TV', 'primary full', async (event) => { const target = event.currentTarget; target.disabled = true; try { const count = await invoke('set_epg_source', { source: epg.input.value }); await refreshLibrary(); toast(`${count} programmes chargés.`); if (state.playing?.tvgId) renderPrograms(state.playing.tvgId); } catch (error) { toast(errorMessage(error), 'error'); } finally { target.disabled = false; } }));
    const heading = make('h3', 'settings-heading', 'Playlists'); body.append(heading);
    for (const playlist of state.library.playlists) {
      const row = make('div', 'settings-row'); const info = make('div'); info.append(make('strong', '', playlist.name), make('small', '', `${playlist.channels.length} chaînes`));
      row.append(info, button('↻', 'icon-button', async () => { try { const updated = await invoke('refresh_playlist', { id: playlist.id }); await refreshLibrary(); toast(`${updated.channels.length} chaînes actualisées.`); } catch (error) { toast(errorMessage(error), 'error'); } }), button('×', 'icon-button danger', async () => { if (!window.confirm(`Supprimer « ${playlist.name} » ?`)) return; await invoke('remove_playlist', { id: playlist.id }); if (state.view === playlist.id) state.view = 'all'; await refreshLibrary(); showSettings(); })); body.append(row);
    }
  });
}
async function openMedia() {
  try { const path = await open({ multiple: false, filters: [{ name: 'Audio et vidéo', extensions: ['mp4', 'm4v', 'mov', 'mp3', 'm4a', 'aac', 'wav', 'aiff', 'webm', 'mkv'] }] }); if (!path) return; const allowed = await invoke('allow_media_file', { path }); await playSource(convertFileSrc(allowed), path.split('/').pop()); }
  catch (error) { toast(errorMessage(error), 'error'); }
}
$('addPlaylistShortcut').onclick = showAddPlaylist; $('settingsButton').onclick = showSettings; $('playUrl').onclick = showPlayUrl; $('openMedia').onclick = openMedia;
$('favoriteButton').onclick = () => state.playing?.channelId && toggleFavorite(state.playing.channelId);
$('modalClose').onclick = closeModal; $('modal').onclick = (event) => { if (event.target === $('modal')) closeModal(); };
$('search').oninput = (event) => { state.query = event.target.value; $('channels').scrollTop = 0; render(); };
document.querySelectorAll('.main-nav button').forEach((button) => button.onclick = () => setView(button.dataset.view));
document.addEventListener('keydown', (event) => { if (event.key === 'Escape') closeModal(); if (event.key === ' ' && event.target === document.body && video.src) { event.preventDefault(); if (video.paused) video.play(); else video.pause(); } });
video.addEventListener('error', () => { if (video.src) $('playerError').textContent = 'Ce flux ou ce format ne peut pas être lu par le moteur multimédia de macOS.'; });
refreshLibrary().catch((error) => toast(errorMessage(error), 'error'));
