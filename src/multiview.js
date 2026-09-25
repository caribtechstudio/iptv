// Multiview: up to four channels at once. Only the selected cell plays its sound.

import { Player } from './playback.js';
import { MpvMedia } from './mpv-media.js';

export const MAX_CELLS = 4;

/** `listen` enables the mpv engine in the cells (one instance per cell). */
export function createMultiview(root, { deps, alternatives, onPromote, toast, listen }) {
  const cells = [];
  let active = -1;
  const grid = document.createElement('div');
  grid.className = 'mv-grid';
  const empty = document.createElement('div');
  empty.className = 'mv-empty';
  empty.innerHTML = '<strong>Multivue</strong><span>Cliquez sur des chaînes de la liste pour les ajouter (4 au maximum). Cliquez sur une vignette pour entendre son son.</span>';
  root.replaceChildren(grid, empty);

  function layout() {
    grid.dataset.count = String(cells.length);
    empty.hidden = cells.length > 0;
    cells.forEach((cell, index) => {
      cell.element.classList.toggle('active', index === active);
      cell.video.muted = index !== active;
      if (cell.mpv) cell.mpv.muted = index !== active;
    });
  }

  function focus(index) { active = index; layout(); }

  function remove(cell) {
    const index = cells.indexOf(cell);
    if (index < 0) return;
    cell.player.destroy();
    cell.mpv?.destroy();
    cell.element.remove();
    cells.splice(index, 1);
    if (active >= cells.length) active = cells.length - 1;
    else if (active > index) active -= 1;
    layout();
  }

  function add(channel) {
    if (cells.some((cell) => cell.channel.streamUrl === channel.streamUrl)) { toast('Cette chaîne est déjà dans la multivue.'); return; }
    if (cells.length >= MAX_CELLS) { toast(`La multivue affiche ${MAX_CELLS} chaînes au maximum.`, 'error'); return; }
    const element = document.createElement('div');
    element.className = 'mv-cell';
    const video = document.createElement('video');
    video.playsInline = true; video.muted = true;
    const bar = document.createElement('div');
    bar.className = 'mv-bar';
    const title = document.createElement('strong');
    title.textContent = channel.name;
    const status = document.createElement('span');
    status.className = 'mv-status';
    status.textContent = 'Chargement…';
    const promote = document.createElement('button');
    promote.textContent = '⤢'; promote.title = 'Regarder en grand';
    const close = document.createElement('button');
    close.textContent = '×'; close.title = 'Retirer';
    bar.append(title, status, promote, close);
    element.append(video, bar);
    grid.append(element);
    // Surfaces are named by free slot so a removed cell's view is reused.
    const slot = [0, 1, 2, 3].find((index) => !cells.some((cell) => cell.slot === index));
    const mpv = listen ? new MpvMedia(`mv-${slot}`, { invoke: deps.invoke, listen }) : null;
    if (mpv) {
      mpv.muted = true;
      mpv.track(element, { clip: () => root.getBoundingClientRect() });
      for (const type of ['emptied', 'loadstart']) mpv.addEventListener(type, () => element.classList.toggle('mpv-active', Boolean(mpv.getAttribute('src'))));
    }
    const player = new Player(video, deps, {
      onStatus: (text) => { status.textContent = text; },
      onStarted: () => { status.textContent = ''; },
      onRetry: () => { status.textContent = 'Nouvel essai…'; },
      onFailover: (ctx) => { status.textContent = `Autre source (${ctx.index + 1}/${ctx.alternatives.length})…`; },
      onFailure: (message) => { status.textContent = message; element.classList.add('failed'); },
    }, { watchdog: true, mpv });
    const cell = { channel, element, video, player, mpv, slot };
    cells.push(cell);
    element.onclick = (event) => { if (event.target === close || event.target === promote) return; focus(cells.indexOf(cell)); };
    close.onclick = () => remove(cell);
    promote.onclick = () => onPromote(channel);
    if (active < 0) active = cells.length - 1;
    layout();
    player.play({ alternatives: alternatives(channel) });
  }

  function clear() {
    while (cells.length) remove(cells[0]);
    active = -1;
    layout();
  }

  layout();
  return { add, clear, get size() { return cells.length; }, channels: () => cells.map((cell) => cell.channel) };
}
