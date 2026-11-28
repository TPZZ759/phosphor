/* Front-end — vanilla JS, no build step. */
(() => {
'use strict';

// ---------------------------------------------------------------- utils
const $ = (s, el = document) => el.querySelector(s);
const $$ = (s, el = document) => [...el.querySelectorAll(s)];
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const view = $('#view');
const ITEMS = new Map(); // "k:id" -> card data, for click handling
const keyOf = (it) => `${it.k}:${it.id}`;
const remember = (it) => { ITEMS.set(keyOf(it), it); return keyOf(it); };

let routeGen = 0;
let routeAbort = null;
let routeCleanups = [];
const onRouteCleanup = (fn) => { if (typeof fn === 'function') routeCleanups.push(fn); };
const trackObserver = (io) => { onRouteCleanup(() => { try { io.disconnect(); } catch {} }); return io; };
function runRouteCleanups() {
  if (routeAbort) { try { routeAbort.abort(); } catch {} routeAbort = null; }
  const list = routeCleanups; routeCleanups = [];
  for (const fn of list) { try { fn(); } catch {} }
}
const isAbort = (e) => !!(e && (e.name === 'AbortError' || e.aborted));

const SWR = new Map(); // url -> { data, ts, promise }
function invalidateCache(...prefixes) {
  if (!prefixes.length) { SWR.clear(); return; }
  for (const k of [...SWR.keys()]) {
    if (prefixes.some((p) => k.startsWith(p))) SWR.delete(k);
  }
}

async function api(path, opts = {}) {
  const init = {};
  if (opts.body) { init.method = 'POST'; init.headers = { 'Content-Type': 'application/json' }; init.body = JSON.stringify(opts.body); }
  if (opts.signal) init.signal = opts.signal;
  const res = await fetch(path, init);
  if (res.status === 503) { const j = await res.json(); const e = new Error('loading'); e.loading = true; e.info = j; throw e; }
  const j = await res.json().catch(() => ({}));
  if (!res.ok) throw Object.assign(new Error(j.error || `HTTP ${res.status}`), { status: res.status });
  return j;
}
function apiCached(url, ttlMs = 60000) {
  const now = Date.now();
  const cur = SWR.get(url);
  if (cur && cur.data !== undefined) {
    if (now - cur.ts >= ttlMs && !cur.promise) {
      cur.promise = api(url).then((d) => {
        SWR.set(url, { data: d, ts: Date.now(), promise: null });
        return d;
      }).catch(() => {
        const e = SWR.get(url); if (e) e.promise = null;
      });
    }
    return Promise.resolve(cur.data);
  }
  if (cur?.promise) return cur.promise;
  const p = api(url).then((d) => {
    SWR.set(url, { data: d, ts: Date.now(), promise: null });
    return d;
  }).catch((err) => {
    SWR.delete(url);
    throw err;
  });
  SWR.set(url, { data: undefined, ts: 0, promise: p });
  return p;
}
const post = (path, body) => api(path, { body });

function mountOverlay(node) {
  const host = document.fullscreenElement || document.body;
  if (node && node.parentNode !== host) host.appendChild(node);
}
document.addEventListener('fullscreenchange', () => {
  mountOverlay($('#toast'));
  mountOverlay($('#alertsStack'));
});
function toast(msg) {
  const t = $('#toast'); mountOverlay(t); t.textContent = msg; t.classList.add('on');
  clearTimeout(toast.t); toast.t = setTimeout(() => t.classList.remove('on'), 2400);
}
function showAlert(m) {
  let stack = $('#alertsStack');
  if (!stack) { stack = document.createElement('div'); stack.id = 'alertsStack'; stack.className = 'alerts-stack'; }
  mountOverlay(stack);
  const card = document.createElement('div');
  card.className = `alert-card ${esc(m.kind || '')}`;
  const act = m.action && (m.action.k || m.action.route) ? m.action : null;
  card.innerHTML = `<div class="al-top"><div style="min-width:0;flex:1"><div class="al-t">${esc(m.title || 'Alert')}</div>${m.body ? `<div class="al-b">${esc(m.body)}</div>` : ''}</div><button class="al-x" aria-label="Dismiss">×</button></div>`
    + (act ? `<div class="al-acts"><button class="btn btn-primary btn-sm al-go">${esc(act.label || 'Open')}</button></div>` : '');
  const rm = () => { clearTimeout(card._t); card.remove(); };
  $('.al-x', card).onclick = rm;
  if (act) $('.al-go', card).onclick = async () => {
    rm();
    if (act.k === 'l') {
      playLive({ id: +act.id, title: act.title || m.body || 'Live', epg: act.epg || '', feeds: act.feeds || null });
    } else if (act.k === 's' || act.k === 'm') {
      if (!$('#player').hidden) await Player.close();
      openDetail({ k: act.k, id: +act.id, title: act.title || '' });
    } else if (act.route) {
      if (!$('#player').hidden) await Player.close();
      location.hash = act.route;
    }
  };
  stack.appendChild(card);
  card._t = setTimeout(rm, 11000);
}
const fmtTime = (s) => {
  s = Math.max(0, Math.floor(s)); const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), x = s % 60;
  return h ? `${h}:${String(m).padStart(2, '0')}:${String(x).padStart(2, '0')}` : `${m}:${String(x).padStart(2, '0')}`;
};
const fmtClock = (ts) => new Date(ts).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
function dayLabel(ts) {
  const d = new Date(ts); const t = new Date(); t.setHours(0, 0, 0, 0);
  const diff = Math.round((new Date(d).setHours(0, 0, 0, 0) - t) / 86400e3);
  if (diff === 0) return 'Today'; if (diff === 1) return 'Tomorrow'; if (diff === -1) return 'Yesterday';
  return d.toLocaleDateString([], { weekday: 'long', day: 'numeric', month: 'short' });
}
const runtime = (min) => (min ? (min >= 60 ? `${Math.floor(min / 60)}h ${min % 60}m` : `${min}m`) : '');
const debounce = (fn, ms) => { let t; return (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); }; };

const I = {
  play: '<svg viewBox="0 0 24 24"><path d="M7 4.5v15l12.5-7.5z" fill="currentColor"/></svg>',
  pause: '<svg viewBox="0 0 24 24"><path d="M6.5 4.5h4v15h-4zM13.5 4.5h4v15h-4z" fill="currentColor"/></svg>',
  info: '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" stroke-width="2"/><path d="M12 11v6M12 7.5v.5" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"/></svg>',
  plus: '<svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"/></svg>',
  check: '<svg viewBox="0 0 24 24"><path d="M5 12.5l4.5 4.5L19 7.5" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  close: '<svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"/></svg>',
  left: '<svg viewBox="0 0 24 24"><path d="M15 5l-7 7 7 7" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  right: '<svg viewBox="0 0 24 24"><path d="M9 5l7 7-7 7" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  back: '<svg viewBox="0 0 24 24"><path d="M19 12H5M11 6l-6 6 6 6" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  rew: '<svg viewBox="0 0 24 24"><path d="M12 5V2L7 6l5 4V7a6 6 0 1 1-6 6" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><text x="12" y="16.5" font-size="7" text-anchor="middle" fill="currentColor" font-family="Inter" font-weight="700">10</text></svg>',
  fwd: '<svg viewBox="0 0 24 24"><path d="M12 5V2l5 4-5 4V7a6 6 0 1 0 6 6" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><text x="12" y="16.5" font-size="7" text-anchor="middle" fill="currentColor" font-family="Inter" font-weight="700">10</text></svg>',
  cast: '<svg viewBox="0 0 24 24"><path d="M3 18a3 3 0 0 1 3 3M3 14a7 7 0 0 1 7 7M3 10a11 11 0 0 1 11 11" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><path d="M3 7V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-3" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>',
  pro: '<svg viewBox="0 0 24 24"><rect x="3" y="5" width="18" height="12" rx="2" fill="none" stroke="currentColor" stroke-width="2"/><path d="M8 20h8M10.5 8.5v5l4-2.5z" fill="currentColor" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/></svg>',
  full: '<svg viewBox="0 0 24 24"><path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>',
  unfull: '<svg viewBox="0 0 24 24"><path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>',
  vol: '<svg viewBox="0 0 24 24"><path d="M4 9.5h4L13 5v14l-5-4.5H4z" fill="currentColor"/><path d="M16.5 8.5a5 5 0 0 1 0 7M19 6a8.5 8.5 0 0 1 0 12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>',
  volLow: '<svg viewBox="0 0 24 24"><path d="M4 9.5h4L13 5v14l-5-4.5H4z" fill="currentColor"/><path d="M16.5 8.5a5 5 0 0 1 0 7" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>',
  mute: '<svg viewBox="0 0 24 24"><path d="M4 9.5h4L13 5v14l-5-4.5H4z" fill="currentColor"/><path d="M17 9.5l5 5M22 9.5l-5 5" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>',
  pip: '<svg viewBox="0 0 24 24"><rect x="3" y="5" width="18" height="14" rx="2" fill="none" stroke="currentColor" stroke-width="2"/><rect x="12" y="11" width="7" height="5" rx="1" fill="currentColor"/></svg>',
  tune: '<svg viewBox="0 0 24 24"><path d="M4 7h10M18 7h2M4 17h2M10 17h10M4 12h6M14 12h6" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><circle cx="16" cy="7" r="2" fill="currentColor"/><circle cx="8" cy="17" r="2" fill="currentColor"/><circle cx="12" cy="12" r="2" fill="currentColor"/></svg>',
  help: '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" stroke-width="2"/><path d="M9.5 9.5a2.6 2.6 0 0 1 5 .8c0 1.7-2.5 2-2.5 3.4M12 17v.4" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>',
  star: '<svg viewBox="0 0 24 24"><path d="M12 3.5l2.6 5.4 5.9.8-4.3 4.1 1 5.8L12 16.9l-5.2 2.7 1-5.8L3.5 9.7l5.9-.8z" fill="currentColor"/></svg>',
  starO: '<svg viewBox="0 0 24 24"><path d="M12 3.5l2.6 5.4 5.9.8-4.3 4.1 1 5.8L12 16.9l-5.2 2.7 1-5.8L3.5 9.7l5.9-.8z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"/></svg>',
  next: '<svg viewBox="0 0 24 24"><path d="M5 5v14l10-7zM17 5h2.5v14H17z" fill="currentColor"/></svg>',
  up: '<svg viewBox="0 0 24 24"><path d="M7 10v10H4V10zM7 10l4-7c1.5 0 2.5 1 2.2 2.6L12.6 9H18a2 2 0 0 1 2 2.3l-1.2 7A2 2 0 0 1 16.8 20H7" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linejoin="round"/></svg>',
  down: '<svg viewBox="0 0 24 24"><path d="M17 14V4h3v10zM17 14l-4 7c-1.5 0-2.5-1-2.2-2.6l.6-3.4H6a2 2 0 0 1-2-2.3l1.2-7A2 2 0 0 1 7.2 4H17" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linejoin="round"/></svg>',
  bell: '<svg viewBox="0 0 24 24"><path d="M6 16V11a6 6 0 1 1 12 0v5l1.5 2h-15zM10 20.5a2.2 2.2 0 0 0 4 0" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linejoin="round"/></svg>',
  bellOn: '<svg viewBox="0 0 24 24"><path d="M6 16V11a6 6 0 1 1 12 0v5l1.5 2h-15z" fill="currentColor"/><path d="M10 20.5a2.2 2.2 0 0 0 4 0" fill="none" stroke="currentColor" stroke-width="1.9"/></svg>',
  eye: '<svg viewBox="0 0 24 24"><path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z" fill="none" stroke="currentColor" stroke-width="1.9"/><circle cx="12" cy="12" r="3" fill="currentColor"/></svg>',
  sound: '<svg viewBox="0 0 24 24"><path d="M4 9.5h4L13 5v14l-5-4.5H4z" fill="currentColor"/><path d="M16.5 8.5a5 5 0 0 1 0 7" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>',
  film: '<svg viewBox="0 0 24 24"><rect x="3" y="5" width="18" height="14" rx="2" fill="none" stroke="currentColor" stroke-width="2"/><path d="M10 9.5v5l4-2.5z" fill="currentColor"/></svg>',
};

// ---------------------------------------------------------------- components
const fmtVotes = (n) => (n >= 1000 ? `${(n / 1000).toFixed(n >= 10000 ? 0 : 1)}k` : n > 0 ? String(n) : '');
const COUNTRY_NAMES = { US: 'USA', GB: 'UK', AU: 'Australia', CA: 'Canada', JP: 'Japan', KR: 'South Korea', FR: 'France', DE: 'Germany', ES: 'Spain', IT: 'Italy', IN: 'India', NZ: 'New Zealand', IE: 'Ireland', SE: 'Sweden', NO: 'Norway', DK: 'Denmark', MX: 'Mexico', BR: 'Brazil' };
const countryName = (c) => COUNTRY_NAMES[(c || '').toUpperCase()] || (c || '').toUpperCase();
const getViewMode = () => localGet('viewMode', 'grid') === 'list' ? 'list' : 'grid';
const setViewMode = (m) => localSet('viewMode', m === 'list' ? 'list' : 'grid');
const viewToggleHTML = (mode) => `<div class="view-seg" role="group" aria-label="View mode">
  <button type="button" data-vmode="grid" class="${mode === 'grid' ? 'on' : ''}" title="Poster grid">⊞ Grid</button>
  <button type="button" data-vmode="list" class="${mode === 'list' ? 'on' : ''}" title="Detailed data view">☰ Data</button>
</div>`;

function qualityBadgeHTML(it) {
  if (it.cam || it.camOnly) return '<span class="badge-cam" title="CAM / Telesync rip">CAM</span>';
  if (it.dv || it.isDv || it.qualityLabel === '4K DV') return '<span class="badge4k dv" title="4K Dolby Vision">4K DV</span>';
  if (it.uhd || it.qualityTier >= 3) return '<span class="badge4k">4K</span>';
  if (it.bluray || it.isBluray || it.qualityLabel === 'Blu-Ray') return '<span class="badge4k br" title="Blu-Ray Master">BLU-RAY</span>';
  return '';
}

function poster(it) {
  const k = remember(it);
  const img = it.poster ? `<img src="${esc(it.poster)}" alt="" loading="lazy" onerror="this.remove()">` : '';
  const dur = it.k === 's' ? (it.seasons ? `${it.seasons}S` : '') : (it.runtime ? runtime(it.runtime) : '');
  const subParts = [...(it.docTopics?.slice(0, 1) || it.genres?.slice(0, 2) || []), ...(it.platforms?.slice(0, 1) || [])];
  const topLeft = it.dubStatus === 'yes'
    ? '<span class="badge-dub" title="Confirmed English dubbed audio">DUB</span>'
    : (it.isDoc && location.hash.indexOf('#/docs') !== 0 ? '<span class="badge-doc" title="Documentary">DOC</span>' : '');
  const qBadge = qualityBadgeHTML(it);
  const upgBadge = it.upgradeBadge ? `<span class="badge-upg" title="Recently upgraded">${esc(it.upgradeBadge)}</span>` : '';
  return `<div class="card" tabindex="0" data-open="${k}" role="button" aria-label="${esc(it.title)}">
    <div class="fallback">${esc(it.title)}</div>${img}
    ${topLeft}${qBadge}${upgBadge}${it.k === 'x' ? `<span class="badge-ext">${it.wanted ? '🔔 Alert set' : it.release && new Date(it.release) > Date.now() ? new Date(it.release).toLocaleDateString([], { day: 'numeric', month: 'short' }) : 'Not in library'}</span>` : ''}
    <div class="cap"><div class="cap-title">${esc(it.title)}</div>
      <div class="cap-meta">${it.year ? `<span>${it.year}</span>` : ''}${it.rating ? `<span class="star" title="${it.votes ? `${it.votes.toLocaleString()} votes` : ''}">★ ${(+it.rating).toFixed(1)}</span>` : ''}${dur ? `<span>${esc(dur)}</span>` : ''}</div>
      ${subParts.length ? `<div class="cap-sub">${esc(subParts.join(' · '))}</div>` : ''}
      ${it.tmdb ? `<div class="rate"><button data-rate="1" data-key="${k}" title="I like this" aria-label="Like">${I.up}</button><button data-rate="-1" data-key="${k}" title="Not for me (hide it)" aria-label="Not for me">${I.down}</button></div>` : ''}</div>
  </div>`;
}

function richCard(it) {
  const k = remember(it);
  const img = it.poster ? `<img src="${esc(it.poster)}" alt="" loading="lazy" onerror="this.remove()">` : '';
  const typeLabel = it.isDoc ? (it.k === 's' ? 'Docuseries' : 'Documentary') : it.isAnime ? (it.k === 's' ? 'Anime Series' : 'Anime Film') : (it.k === 's' ? 'Series' : 'Movie');
  const dataBits = [
    it.rating ? `<span class="star">★ ${(+it.rating).toFixed(1)}${it.votes ? ` <span class="votes">(${fmtVotes(it.votes)})</span>` : ''}</span>` : '',
    it.scores?.imdb ? `<span class="rc-sc">IMDb ${esc(it.scores.imdb)}</span>` : '',
    it.scores?.rt ? `<span class="rc-sc">${parseInt(it.scores.rt, 10) >= 60 ? '🍅' : '🤢'} ${esc(it.scores.rt)}</span>` : '',
    it.probedKbps ? `<span class="rc-sc">${Math.round(it.probedKbps / 1000)} Mbps${it.probedHdr ? ` · ${esc(it.probedHdr)}` : ''}</span>` : '',
    it.k === 's' && it.seasons ? `<span>${it.seasons} season${it.seasons === 1 ? '' : 's'}${it.status ? ` · ${esc(it.status)}` : ''}</span>` : (it.runtime ? `<span>${runtime(it.runtime)}</span>` : ''),
    it.country ? `<span>${esc(countryName(it.country))}</span>` : '',
    it.lang ? `<span>${esc(langName(it.lang) || it.lang.toUpperCase())}</span>` : '',
    it.versions > 1 ? `<span>${it.versions} copies</span>` : '',
  ].filter(Boolean).join('');
  const tags = [
    ...(it.upgradeBadge ? [`<span class="rc-tag topic">${esc(it.upgradeBadge)}</span>`] : []),
    ...(it.docTopics || []).map((t) => `<span class="rc-tag topic">${esc(t)}</span>`),
    ...(it.genres || []).filter((g) => !(it.isDoc && g === 'Documentary')).slice(0, 5).map((g) => `<span class="rc-tag">${esc(g)}</span>`),
    ...(it.platforms || []).slice(0, 3).map((p) => `<span class="rc-tag plat">${esc(p)}</span>`),
  ].join('');
  const topLeft = it.dubStatus === 'yes' ? '<span class="badge-dub">DUB</span>' : (it.isDoc ? '<span class="badge-doc">DOC</span>' : '');
  return `<article class="rc" data-open="${k}" tabindex="0" role="button">
    <div class="rc-thumb">${img}${topLeft}${qualityBadgeHTML(it)}</div>
    <div class="rc-body">
      <div class="rc-top"><h3 class="rc-title">${esc(it.title)} ${it.year ? `<span class="rc-yr">(${it.year})</span>` : ''}</h3><span class="pill ${it.isDoc ? 'doc' : it.isAnime ? 'dub' : ''}">${esc(typeLabel)}</span></div>
      ${dataBits ? `<div class="rc-data">${dataBits}</div>` : ''}
      ${tags ? `<div class="rc-tags">${tags}</div>` : ''}
      <p class="rc-ov">${esc(it.overview || 'No description available.')}</p>
      <div class="rc-acts">
        ${it.k !== 'x' ? `<button class="btn btn-primary btn-sm" data-quickplay="${k}">${I.play}${it.k === 's' ? 'Episodes' : 'Play'}</button>` : ''}
        <button class="btn btn-ghost btn-sm" data-open="${k}">More info</button>
        ${it.tmdb ? `<button class="pbtn sm" data-rate="1" data-key="${k}" title="Like">${I.up}</button><button class="pbtn sm" data-rate="-1" data-key="${k}" title="Not for me">${I.down}</button>` : ''}
      </div>
    </div>
  </article>`;
}

const renderCards = (items, mode) => mode === 'list'
  ? `<div class="rich-list">${items.map(richCard).join('')}</div>`
  : `<div class="grid">${items.map(poster).join('')}</div>`;
function newEpCard(it) {
  const k = remember(it); const img = it.backdrop || it.poster;
  return `<div class="wide" data-open="${k}" tabindex="0" role="button">
    <div class="thumb">${img ? `<img src="${esc(img)}" alt="" loading="lazy" onerror="this.remove()">` : ''}<span class="new-badge">NEW</span>
      <div class="play-ov"><span>${I.play}</span></div></div>
    <div class="w-title">${esc(it.title)}</div><div class="w-sub">${esc(it.sub || '')}</div></div>`;
}
async function rateItem(it, v, quiet) {
  const mt = it.k === 's' ? 'tv' : 'movie';
  try { await post('/api/rate', { mt, tmdb: it.tmdb, v, title: it.title }); }
  catch (e) { return toast(e.message); }
  invalidateCache('/api/home', '/api/movies', '/api/series', '/api/anime', '/api/docs', '/api/showcase', '/api/mylist');
  if (!quiet) toast(v > 0 ? `Got it, more like ${it.title}` : v < 0 ? `${it.title} hidden. You won't see it again` : 'Rating cleared');
  if (v < 0) $$(`[data-open="${keyOf(it)}"]`).forEach((c) => (c.closest('.top10') || c).remove());
}
function top10(it, i) {
  return `<div class="top10"><span class="rank">${i + 1}</span>${poster(it)}</div>`;
}
const fmtLeft = (s) => {
  s = Math.max(0, Math.round(s || 0));
  if (s < 60) return '<1m left';
  const h = Math.floor(s / 3600), m = Math.round((s % 3600) / 60);
  return h ? `${h}h ${m}m left` : `${m}m left`;
};
function continueCard(it) {
  const k = remember({ ...it, k: it.k === 'e' ? 'e' : it.k, id: it.id });
  const img = it.backdrop || it.still || it.poster;
  const cwKey = it.cwKey || it.key || '';
  const epCount = it.totalEps ? ` · ${it.watchedEps || 0}/${it.totalEps} eps` : '';
  const metaLine = it.nextUp
    ? `Next up${epCount}`
    : it.remainingSecs > 0 ? `${fmtLeft(it.remainingSecs)}${epCount}` : (it.resume ? `${fmtTime(it.resume)} watched` : '');
  const subLine = it.sub || '';
  const doneTitle = it.k === 'e' ? 'Mark episode watched (advance to next)' : 'Mark as watched';
  return `<div class="wide" data-resume="${k}" data-cw-card="${esc(cwKey)}" tabindex="0" role="button">
    <div class="thumb">${img ? `<img src="${esc(img)}" alt="" loading="lazy" onerror="this.remove()">` : ''}
      ${it.pinned ? '<span class="cw-pin-pill">PINNED</span>' : ''}
      ${it.nextUp ? `<span class="cw-next-pill ${it.pinned ? 'shifted' : ''}">NEXT UP</span>` : ''}
      ${cwKey ? `<div class="cw-acts">
        <button class="cw-act ${it.pinned ? 'on' : ''}" data-cw-pin="${esc(cwKey)}" data-pinned="${it.pinned ? '1' : '0'}" title="${it.pinned ? 'Unpin' : 'Pin to front'}" aria-label="Pin">📌</button>
        ${it.k !== 'cu' ? `<button class="cw-act" data-cw-done="${k}" title="${doneTitle}" aria-label="${doneTitle}">✓</button>` : ''}
        ${!it.nextUp ? `<button class="cw-act" data-cw-restart="${k}" title="Start from beginning" aria-label="Restart">⟲</button>` : ''}
        ${it.k !== 'cu' ? `<button class="cw-act" data-cw-info="${k}" title="Details & episodes" aria-label="Info">ⓘ</button>` : ''}
        <button class="cw-act danger" data-cw-remove="${esc(cwKey)}" title="Remove from Continue Watching" aria-label="Remove">×</button>
      </div>` : ''}
      <div class="play-ov"><span>${I.play}</span></div>
      ${it.progress > 0 ? `<div class="bar"><i style="width:${Math.min(100, Math.round((it.progress || 0) * 100))}%"></i></div>` : ''}</div>
    <div class="w-title">${esc(it.title)}</div>
    ${subLine ? `<div class="w-sub">${esc(subLine)}</div>` : ''}
    ${metaLine ? `<div class="w-meta">${esc(metaLine)}</div>` : ''}
  </div>`;
}
function eventCard(e) {
  const feeds = e.alts?.length ? [{ id: e.id, channel: e.channel, label: e.channel }, ...e.alts.map((a) => ({ id: a.id, channel: a.channel, label: a.channel }))] : null;
  const k = remember({ k: 'l', id: e.id, title: e.title, sub: e.channel, logo: e.logo, feeds });
  let status;
  if (e.status === 'live') status = '<span class="ev-status live"><span class="live-dot"></span>Live</span>';
  else if (e.start) status = `<span class="ev-status">${esc(dayLabel(e.start))} · ${esc(fmtClock(e.start))}</span>`;
  else status = '<span class="ev-status">Listed</span>';
  return `<div class="event" data-live="${k}" tabindex="0" role="button">
    <div class="ev-top"><span class="ev-league">${esc(e.league)}</span>${status}</div>
    <div class="ev-title">${esc(e.title)}</div>
    <div class="ev-ch">${esc(e.channel)}${e.alts?.length ? ` · +${e.alts.length} more feed${e.alts.length > 1 ? 's' : ''}` : ''}</div>
  </div>`;
}
function channelWide(ch) {
  const k = remember({ k: 'l', id: ch.id, title: ch.title, logo: ch.logo, epg: ch.epg });
  const pct = ch.now ? Math.min(100, Math.max(0, ((Date.now() - ch.now.s) / (ch.now.e - ch.now.s)) * 100)) : 0;
  return `<div class="wide" data-live="${k}" tabindex="0" role="button">
    <div class="thumb">${ch.logo ? `<img class="contain" src="${esc(ch.logo)}" alt="" loading="lazy" onerror="this.remove()">` : ''}
      <div class="play-ov"><span>${I.play}</span></div>${ch.now ? `<div class="bar"><i style="width:${pct}%"></i></div>` : ''}</div>
    <div class="w-title">${esc(ch.now?.title || ch.title)}</div>
    <div class="w-sub">${esc(ch.now ? `${ch.title} · until ${fmtClock(ch.now.e)}` : ch.catName || '')}</div>
  </div>`;
}
function channelRow(ch) {
  const k = remember({ k: 'l', id: ch.id, title: ch.title, logo: ch.logo, epg: ch.epg });
  const pct = ch.now ? Math.min(100, Math.max(0, ((Date.now() - ch.now.s) / (ch.now.e - ch.now.s)) * 100)) : 0;
  const qt = ch.qtag ? `<span class="qtag ${/^4K/i.test(ch.qtag) ? 'uhd' : ''}">${esc(ch.qtag)}</span>` : '';
  return `<div class="ch" data-live="${k}" tabindex="0" role="button">
    <div class="logo">${ch.logo ? `<img src="${esc(ch.logo)}" alt="" loading="lazy" onerror="this.replaceWith(Object.assign(document.createElement('b'),{textContent:'TV'}))">` : '<b>TV</b>'}</div>
    <div style="min-width:0">
      <div class="ch-name">${esc(ch.title)}${qt}</div>
      ${ch.now ? `<div class="ch-now">${esc(ch.now.title)} <em>· ${fmtClock(ch.now.s)}–${fmtClock(ch.now.e)}</em>${ch.next ? ` <em>· Next: ${esc(ch.next.title)}</em>` : ''}</div><div class="ch-prog"><i style="width:${pct}%"></i></div>` : ''}
    </div>
    <button class="fav-btn ${ch.fav ? 'on' : ''}" data-fav="${k}" title="${ch.fav ? 'Remove from favourites' : 'Add to favourites'}" aria-label="Favourite">${ch.fav ? I.star : I.starO}</button>
  </div>`;
}

function row(r, body) {
  const manage = r.style === 'continue' ? '<button class="btn btn-ghost btn-sm row-manage" data-cw-manage>Manage</button>' : '';
  return `<section class="row" data-row="${esc(r.id)}">
    <div class="row-head"><h2 class="row-title">${esc(r.title)}</h2>${r.sub ? `<span class="row-sub">${esc(r.sub)}</span>` : ''}${manage}</div>
    <div class="row-wrap"><button class="arrow l" aria-label="Scroll left">${I.left}</button>
      <div class="scroller">${body}</div>
      <button class="arrow r" aria-label="Scroll right">${I.right}</button></div>
  </section>`;
}
function refreshCwRows(items) {
  invalidateCache('/api/home', '/api/anime', '/api/docs', '/api/mylist');
  const rows = $$('[data-row="continue"], [data-row="an-continue"], [data-row="doc-continue"]');
  for (const r of rows) {
    const isAn = r.dataset.row === 'an-continue';
    const isDoc = r.dataset.row === 'doc-continue';
    const list = isDoc ? items.filter((x) => x.isDoc) : isAn ? items.filter((x) => x.isAnime) : items;
    if (!list.length) { r.remove(); continue; }
    const sc = $('.scroller', r);
    if (sc) sc.innerHTML = list.map(continueCard).join('');
  }
}
async function openManageCw() {
  const d = await api('/api/continue').catch(() => null);
  if (!d || !Array.isArray(d.items)) {
    return toast('Server update is queued — it will activate ~2 min after you stop watching');
  }
  const draw = (items, dismissedCount) => {
    refreshCwRows(items);
    const listHtml = items.length ? items.map((it) => {
      const k = remember({ ...it, k: it.k === 'e' ? 'e' : it.k, id: it.id });
      const cwKey = it.cwKey || it.key || '';
      const img = it.backdrop || it.still || it.poster;
      const status = it.nextUp
        ? `Next up${it.totalEps ? ` · ${it.watchedEps || 0}/${it.totalEps} eps watched` : ''}`
        : `${Math.round((it.progress || 0) * 100)}% · ${fmtLeft(it.remainingSecs)}${it.totalEps ? ` · ${it.watchedEps || 0}/${it.totalEps} eps` : ''}`;
      return `<div class="cw-mgr-row">
        <div class="th">${img ? `<img src="${esc(img)}" alt="" loading="lazy">` : ''}${it.progress > 0 ? `<div class="bar"><i style="width:${Math.min(100, Math.round(it.progress * 100))}%"></i></div>` : ''}</div>
        <div class="info"><b>${it.pinned ? '📌 ' : ''}${esc(it.title)}</b><small>${esc([it.sub, status].filter(Boolean).join(' · '))}</small></div>
        <div class="acts">
          <button class="btn btn-primary btn-sm" data-m-play="${k}">${I.play}${it.nextUp ? 'Play' : 'Resume'}</button>
          ${it.k !== 'cu' ? `<button class="btn btn-ghost btn-sm" data-m-done="${k}">${it.k === 'e' ? '✓ Next ep' : '✓ Watched'}</button>` : ''}
          ${!it.nextUp ? `<button class="btn btn-ghost btn-sm" data-m-restart="${k}">⟲</button>` : ''}
          <button class="btn btn-ghost btn-sm" data-m-pin="${esc(cwKey)}" data-pinned="${it.pinned ? '1' : '0'}">${it.pinned ? 'Unpin' : 'Pin'}</button>
          ${it.k !== 'cu' ? `<button class="btn btn-ghost btn-sm" data-m-info="${k}">Info</button>` : ''}
          <button class="btn btn-ghost btn-sm" data-m-rm="${esc(cwKey)}">×</button>
        </div>
      </div>`;
    }).join('') : '<div class="empty">Nothing in Continue Watching right now.</div>';
    openModal(`<div class="sheet settings cw-mgr"><button class="close" aria-label="Close">${I.close}</button>
      <h2>Continue Watching</h2><p>Pin favourites to the front, advance to the next episode, mark finished films watched, or clean up your row.</p>
      <div class="cw-mgr-list">${listHtml}</div>
      ${dismissedCount > 0 ? `<div class="btns" style="margin-top:16px"><button class="btn btn-ghost btn-sm" id="cwRestoreAll">Restore ${dismissedCount} removed item${dismissedCount === 1 ? '' : 's'}</button></div>` : ''}
    </div>`, { noHistory: !modal.hidden });
    $$('[data-m-play]', modal).forEach((b) => (b.onclick = () => { closeModal(); resumeItem(ITEMS.get(b.dataset.mPlay)); }));
    $$('[data-m-restart]', modal).forEach((b) => (b.onclick = () => { closeModal(); restartCwItem(ITEMS.get(b.dataset.mRestart)); }));
    $$('[data-m-info]', modal).forEach((b) => (b.onclick = () => { openCwInfo(ITEMS.get(b.dataset.mInfo)); }));
    $$('[data-m-pin]', modal).forEach((b) => (b.onclick = async () => {
      const on = b.dataset.pinned !== '1';
      const r = await post('/api/continue/pin', { key: b.dataset.mPin, on });
      toast(on ? 'Pinned to front' : 'Unpinned');
      draw(r.items || [], dismissedCount);
    }));
    $$('[data-m-done]', modal).forEach((b) => (b.onclick = async () => {
      const it = ITEMS.get(b.dataset.mDone); if (!it) return;
      const r = await post('/api/continue/done', { key: it.key || (it.k === 'e' ? `e:${it.id}` : `m:${it.id}`), dur: it.dur, tmdb: it.tmdb, item: it });
      toast(it.k === 'e' ? 'Episode marked watched' : 'Marked as watched');
      draw(r.items || [], dismissedCount);
    }));
    $$('[data-m-rm]', modal).forEach((b) => (b.onclick = async () => {
      const r = await post('/api/continue/remove', { key: b.dataset.mRm });
      toast('Removed from Continue Watching');
      draw(r.items || [], dismissedCount + 1);
    }));
    if ($('#cwRestoreAll')) $('#cwRestoreAll').onclick = async () => {
      const r = await post('/api/continue/restore', {});
      toast('Restored removed items');
      draw(r.items || [], 0);
    };
  };
  draw(d.items, d.dismissedCount || 0);
}
async function restartCwItem(it) {
  if (!it) return;
  if (it.k === 'm') { const d = await api(`/api/movie/${it.id}`); return playMovie(d, d.best ?? d.id, 0); }
  if (it.k === 'e') {
    const d = await api(`/api/series/${it.sid}`);
    const flat = d.seasons.flatMap((s) => s.episodes.map((e) => ({ ...e, season: s.n })));
    const ep = flat.find((x) => x.id === it.id);
    if (ep) return playEpisode(d, flat, ep, 0);
    return openDetail({ k: 's', id: it.sid });
  }
  if (it.k === 'cu') return resumeItem({ ...it, resume: 0 });
}
function openCwInfo(it) {
  if (!it) return;
  if (it.k === 'e' && it.sid) return openDetail({ k: 's', id: it.sid, title: it.title });
  if (it.k === 'm') return openDetail({ k: 'm', id: it.id, title: it.title });
}
function renderRow(r) {
  if (!r.items?.length) return '';
  if (r.style === 'top10') return row(r, r.items.slice(0, 10).map(top10).join(''));
  if (r.style === 'continue') return row(r, r.items.map(continueCard).join(''));
  if (r.style === 'wide') return row(r, r.items.map(newEpCard).join(''));
  if (r.style === 'franchise') return row(r, r.items.map(franchiseCard).join(''));
  return row(r, r.items.map(poster).join(''));
}
function wireRows(root = view) {
  for (const w of $$('.row-wrap', root)) {
    const sc = $('.scroller', w); const l = $('.arrow.l', w); const r = $('.arrow.r', w);
    const upd = () => { l.disabled = sc.scrollLeft < 8; r.disabled = sc.scrollLeft + sc.clientWidth >= sc.scrollWidth - 8; };
    l.onclick = () => sc.scrollBy({ left: -sc.clientWidth * 0.85 });
    r.onclick = () => sc.scrollBy({ left: sc.clientWidth * 0.85 });
    sc.addEventListener('scroll', debounce(upd, 60), { passive: true }); upd();
  }
}

// ---------------------------------------------------------------- hero
let heroTimer;
let heroGen = 0;
let heroTrailerGen = 0;
let heroTrailerT; let heroTrailerFadeT; let heroTrailerEndT; let heroTrailerRmT; let detailTrailerT;
function stopHero() {
  heroGen++;
  heroTrailerGen++;
  clearInterval(heroTimer);
  clearTimeout(heroTrailerT);
  clearTimeout(heroTrailerFadeT);
  clearTimeout(heroTrailerEndT);
  clearTimeout(heroTrailerRmT);
  $('.hero-trailer')?.remove();
  $('#heroSound')?.remove();
}
function heroHTML(items) {
  if (!items?.length) return '<div style="height:96px"></div>';
  return `<section class="hero">
    ${items.map((h, i) => `<div class="hero-slide ${i ? '' : 'on'}"><img src="${esc(h.backdrop)}" alt="" ${i ? 'loading="lazy"' : ''}></div>`).join('')}
    <div class="hero-info" id="heroInfo"></div>
    ${items.length > 1 ? `<div class="hero-dots">${items.map((_, i) => `<button data-hero="${i}" class="${i ? '' : 'on'}" aria-label="Slide ${i + 1}"></button>`).join('')}</div>` : ''}
  </section>`;
}
// Muted trailer fades in over the hero after a few seconds; rotation pauses while it plays.
function heroTrailer(h, onStart, onEnd) {
  const myHero = heroGen;
  const myTr = ++heroTrailerGen;
  clearTimeout(heroTrailerT); clearTimeout(heroTrailerFadeT); clearTimeout(heroTrailerEndT); clearTimeout(heroTrailerRmT);
  $('.hero-trailer')?.remove(); $('#heroSound')?.remove();
  heroTrailerT = setTimeout(async () => {
    if (myHero !== heroGen || myTr !== heroTrailerGen) return;
    const { key } = await api(`/api/trailer/${h.k}/${h.id}`).catch(() => ({}));
    if (myHero !== heroGen || myTr !== heroTrailerGen) return;
    const hero = $('.hero'); if (!key || !hero || !$('.hero-slide.on') || !$('#player').hidden) return;
    const f = document.createElement('div'); f.className = 'hero-trailer';
    f.innerHTML = `<iframe src="https://www.youtube-nocookie.com/embed/${encodeURIComponent(key)}?autoplay=1&mute=1&controls=0&rel=0&playsinline=1&modestbranding=1&iv_load_policy=3&start=4&end=64&enablejsapi=1" allow="autoplay" tabindex="-1"></iframe>`;
    hero.insertBefore(f, $('#heroInfo'));
    const snd = document.createElement('button'); snd.id = 'heroSound'; snd.className = 'btn btn-round hero-sound'; snd.title = 'Sound'; snd.innerHTML = I.mute; let muted = true;
    snd.onclick = () => { muted = !muted; f.querySelector('iframe')?.contentWindow?.postMessage(JSON.stringify({ event: 'command', func: muted ? 'mute' : 'unMute', args: [] }), '*'); snd.innerHTML = muted ? I.mute : I.sound; };
    hero.appendChild(snd);
    heroTrailerFadeT = setTimeout(() => { if (myHero === heroGen && myTr === heroTrailerGen && f.isConnected) f.classList.add('on'); }, 1800);
    onStart();
    heroTrailerEndT = setTimeout(() => {
      if (myHero !== heroGen || myTr !== heroTrailerGen || !f.isConnected) return;
      f.classList.remove('on');
      heroTrailerRmT = setTimeout(() => { f.remove(); snd.remove(); }, 800);
      onEnd();
    }, 62000);
  }, 4500);
}
function wireHero(items, kicker) {
  stopHero();
  if (!items?.length) return;
  const myHero = heroGen;
  let cur = 0;
  const show = (i) => {
    if (myHero !== heroGen) return;
    cur = i; const h = items[i]; const k = remember(h);
    heroTrailer(h, () => clearInterval(heroTimer), () => { if (myHero === heroGen) restart(); });
    $$('.hero-slide').forEach((s, j) => s.classList.toggle('on', j === i));
    $$('.hero-dots button').forEach((s, j) => s.classList.toggle('on', j === i));
    const info = $('#heroInfo');
    if (info) info.innerHTML = `
      <div class="hero-kicker">${esc(kicker(i, h))}</div>
      <h1 class="hero-title">${esc(h.title)}</h1>
      <div class="hero-meta">${h.rating ? `<span class="star">★ ${(+h.rating).toFixed(1)}</span>` : ''}${h.year ? `<span>${h.year}</span>` : ''}<span>${h.k === 's' ? 'Series' : 'Movie'}</span>${h.uhd ? '<span class="pill uhd">4K</span>' : ''}</div>
      <p class="hero-overview">${esc(h.overview || '')}</p>
      <div class="btns"><button class="btn btn-primary" data-quickplay="${k}">${I.play}Play</button>
      <button class="btn btn-ghost" data-open="${k}">${I.info}More info</button></div>`;
  };
  show(0);
  $$('.hero-dots button').forEach((b) => (b.onclick = () => { show(+b.dataset.hero); restart(); }));
  const restart = () => {
    clearInterval(heroTimer);
    if (myHero !== heroGen) return;
    heroTimer = setInterval(() => {
      if (myHero !== heroGen || !$('.hero')) { clearInterval(heroTimer); return; }
      show((cur + 1) % items.length);
    }, 9000);
  };
  restart();
}

// ---------------------------------------------------------------- loading / errors
function loadingScreen(msg = 'Loading your catalogue…') {
  view.innerHTML = `<div class="loading"><div><div class="spinner"></div><div>${esc(msg)}</div></div></div>`;
}
async function guard(fn) {
  const my = routeGen;
  try { await fn(); }
  catch (e) {
    if (isAbort(e) || my !== routeGen) return;
    if (e.loading) {
      loadingScreen(e.info?.error ? `Catalogue sync failed: ${e.info.error}. Retrying…` : 'First-time setup: downloading the catalogue (about a minute)…');
      const rt = setTimeout(() => { if (my === routeGen) route(); }, 4000);
      onRouteCleanup(() => clearTimeout(rt));
    } else {
      view.innerHTML = `<div class="loading"><div><h3>Something went wrong</h3><p>${esc(e.message)}</p><button class="btn btn-ghost" onclick="location.reload()">Reload</button></div></div>`;
    }
  }
}

// ---------------------------------------------------------------- sports fixtures
const FIXTURES = new Map();
const spoilerFree = () => localGet('spoilers', '0') === '1';
function teamHTML(t, f) {
  const showScore = f.state !== 'pre' && !spoilerFree() && t.score !== '' && t.score != null;
  return `<div class="fx-team ${f.state === 'post' && t.winner && !spoilerFree() ? 'win' : ''}">
    ${t.logo ? `<img src="${esc(t.logo)}" alt="" loading="lazy" onerror="this.style.visibility='hidden'">` : '<span class="fx-nologo"></span>'}
    <span class="fx-name">${esc(t.short || t.name)}</span>${showScore ? `<b class="fx-score">${esc(t.score)}</b>` : ''}</div>`;
}
function fixtureCard(f) {
  FIXTURES.set(f.id, f);
  let status;
  if (f.state === 'in') status = `<span class="ev-status live"><span class="live-dot"></span>${spoilerFree() ? 'Live' : esc(f.detail || 'Live')}</span>`;
  else if (f.state === 'post') status = `<span class="ev-status">${spoilerFree() ? 'Finished' : esc(f.detail || 'Final')}</span>`;
  else status = `<span class="ev-status">${esc(dayLabel(f.start))} · ${esc(fmtClock(f.start))}</span>`;
  const body = f.solo ? `<div class="ev-title">${esc(f.title)}</div>` : `<div class="fx-teams">${teamHTML(f.away, f)}${teamHTML(f.home, f)}</div>`;
  const feed = f.feeds.length ? `${esc(f.feeds[0].channel)}${f.feeds.length > 1 ? ` · +${f.feeds.length - 1} backup${f.feeds.length > 2 ? 's' : ''}` : ''}` : 'No stream found yet';
  return `<div class="event fixture ${f.fav ? 'fav' : ''} ${f.feeds.length ? '' : 'nofeed'}" data-fixture="${esc(f.id)}" tabindex="0" role="button">
    <div class="ev-top"><span class="ev-league">${f.fav ? '★ ' : ''}${esc(f.leagueName)}</span>${status}</div>
    ${body}<div class="ev-ch">${feed}</div></div>`;
}
function playFixture(f) {
  if (!f) return;
  if (!f.feeds.length) return toast('No stream for this game yet. Feeds usually appear shortly before kickoff');
  const first = f.feeds[0];
  playLive({ id: first.id, title: f.title, sub: `${f.leagueName} · ${first.channel}`, feeds: f.feeds });
}
function fixtureRows(fx, { favRow = true } = {}) {
  let html = '';
  const uniq = []; const seen = new Set();
  for (const f of fx) { if (!seen.has(f.id)) { seen.add(f.id); uniq.push(f); } }
  const fav = uniq.filter((f) => f.fav && f.state !== 'post');
  const favIds = new Set(favRow ? fav.map((f) => f.id) : []);
  if (favRow && fav.length) html += row({ id: 'fx-fav', title: 'Your Teams', sub: 'Next games and live' }, fav.map(fixtureCard).join(''));
  const live = uniq.filter((f) => !favIds.has(f.id) && f.state === 'in' && f.feeds.length);
  if (live.length) html += row({ id: 'fx-live', title: 'Live Now', sub: `${live.length} game${live.length > 1 ? 's' : ''}` }, live.map(fixtureCard).join(''));
  const byDay = new Map();
  for (const f of uniq.filter((x) => !favIds.has(x.id) && x.state === 'pre' && x.feeds.length)) { const l = dayLabel(f.start); if (!byDay.has(l)) byDay.set(l, []); byDay.get(l).push(f); }
  for (const [day, list] of byDay) html += row({ id: 'fx-' + day, title: day, sub: `${list.length} game${list.length > 1 ? 's' : ''}` }, list.map(fixtureCard).join(''));
  const done = uniq.filter((f) => f.state === 'post' && f.feeds.length);
  if (done.length) html += row({ id: 'fx-done', title: 'Recently Finished', sub: spoilerFree() ? 'Scores hidden' : 'Last 12 hours' }, done.reverse().map(fixtureCard).join(''));
  return html;
}

// ---------------------------------------------------------------- pages
let STATUS = {};
let ACCOUNT = null;
async function pageHome() {
  const my = routeGen;
  const [hero, rows, sp] = await Promise.all([apiCached('/api/hero', 60000), apiCached('/api/home', 45000), apiCached('/api/sports', 45000).catch(() => null)]);
  if (my !== routeGen) return;
  let html = heroHTML(hero) + '<div class="rows">';
  if (ACCOUNT?.daysLeft != null && ACCOUNT.daysLeft <= 7) html += `<div class="banner warn">${I.info}<span><b>Your IPTV subscription ${ACCOUNT.daysLeft > 0 ? `ends in ${ACCOUNT.daysLeft} day${ACCOUNT.daysLeft === 1 ? '' : 's'}` : 'has expired'}.</b> Renew with your seller to keep watching.</span></div>`;
  if (!STATUS.tmdb) html += `<div class="banner">${I.info}<span><b>Turn on real trending rows.</b> Add a free TMDB key and Home will show what's actually popular this week, matched to your catalogue.</span><button class="btn btn-accent btn-sm" data-settings>Set up</button></div>`;
  const fx = sp?.fixtures || [];
  for (const f of fx) FIXTURES.set(f.id, f);
  const favGames = fx.filter((f) => f.fav && f.state !== 'post');
  const favIds = new Set(favGames.map((f) => f.id));
  if (favGames.length) html += row({ id: 'fx-fav', title: 'Your Teams', sub: 'Next games and live' }, favGames.map(fixtureCard).join(''));
  for (const r of rows) {
    if (r.id === 'sports-slot') {
      const games = fx.filter((f) => !favIds.has(f.id) && f.feeds.length && (f.state === 'in' || (f.state === 'pre' && f.start - Date.now() < 36 * 3600e3))).slice(0, 30);
      if (games.length) html += row({ id: 'sports', title: 'Live & Upcoming Sports', sub: 'Times shown in your timezone' }, games.map(fixtureCard).join(''));
      continue;
    }
    html += renderRow(r);
  }
  view.innerHTML = html + '</div>';
  wireHero(hero, (i, h) => (STATUS.tmdb ? `#${i + 1} trending today` : h.k === 's' ? 'New series' : 'New movie'));
  wireRows();
}

async function pageBrowse(kind) {
  const my = routeGen;
  const [hero, rows] = await Promise.all([apiCached('/api/hero', 60000), apiCached(kind === 'movies' ? '/api/movies' : '/api/series', 120000)]);
  if (my !== routeGen) return;
  const want = kind === 'movies' ? 'm' : 's';
  const bk = kind === 'movies' ? 'movie' : 'tv';
  let heroItems = hero.filter((h) => h.k === want);
  if (heroItems.length < 2) heroItems = (rows[0]?.items || []).filter((x) => x.backdrop).slice(0, 5);
  const quickGenres = kind === 'movies'
    ? [['28', 'Action'], ['878', 'Sci-Fi'], ['53', 'Thriller'], ['80', 'Crime'], ['35', 'Comedy'], ['18', 'Drama'], ['27', 'Horror'], ['9648', 'Mystery'], ['99', 'Documentary']]
    : [['10759', 'Action'], ['10765', 'Sci-Fi & Fantasy'], ['80', 'Crime'], ['18', 'Drama'], ['35', 'Comedy'], ['9648', 'Mystery'], ['99', 'Documentary']];
  const quickPlats = [['netflix', 'Netflix'], ['hbo', 'HBO'], ['apple', 'Apple TV+'], ['amazon', 'Amazon'], ['disney', 'Disney+']];
  const hubBar = `<div class="hub-bar">
    <a class="chip on" href="#/browse?kind=${bk}">Explore &amp; Filter All ${kind === 'movies' ? 'Movies' : 'Series'} →</a>
    <a class="chip sm" href="#/browse?kind=${bk}&uhd=1">4K Only</a>
    ${quickGenres.map(([id, lab]) => `<a class="chip sm" href="#/browse?kind=${bk}&g=${id}">${esc(lab)}</a>`).join('')}
    ${quickPlats.map(([slug, lab]) => `<a class="chip sm" href="#/browse?kind=${bk}&platform=${slug}">${esc(lab)}</a>`).join('')}
    <button class="btn btn-ghost btn-sm" data-surprise="${bk}" style="margin-left:auto">🎲 Pick for me</button>
  </div>`;
  view.innerHTML = heroHTML(heroItems) + '<div class="rows">' + hubBar + rows.map(renderRow).join('') + '</div>';
  wireHero(heroItems, () => (kind === 'movies' ? 'Trending movie' : 'Trending series'));
  wireRows();
}

let docsState = { mode: 'curated', kind: 'all', topic: '', platform: '', country: '', decade: '', rmin: '', rt: '', format: '', uhd: false, unwatched: false, sort: 'popular', q: '' };
async function pageDocs() {
  const my = routeGen;
  const d = await apiCached('/api/docs', 120000).catch(() => null);
  if (my !== routeGen) return;
  if (!d || !Array.isArray(d.rows)) {
    view.innerHTML = `<div class="page-pad"><div class="page-head"><h1 class="page-title">Documentaries</h1></div>
      <div class="banner">${I.info}<span><b>Server update queued.</b> The Documentaries hub activates automatically after the server restarts. Press <b>Ctrl+R</b> once updated.</span></div></div>`;
    return;
  }
  const heroItems = d.hero || [];
  const topicList = d.topics || [];
  const platOpts = [['', 'All platforms'], ...(d.platforms || []).map((p) => [p.name.toLowerCase().replace(/[^a-z]/g, ''), `${p.name} (${p.count})`])];
  const ctryOpts = [['', 'Any country'], ...(d.countries || []).map((c) => [c.code, `${countryName(c.code)} (${c.count})`])];
  const DOC_RUNTIMES = [['', 'Any length'], ['0-60', 'Under 60m'], ['60-95', '60–95m'], ['95-999', 'Over 95m']];
  const DOC_FORMATS = [['', 'All formats'], ['feature', 'Feature docs'], ['limited', 'Limited series (1S)'], ['multi', 'Multi-season']];
  const vmode = getViewMode();
  view.innerHTML = heroHTML(heroItems) + `<div class="rows">
    <div class="anime-bar">
      <div class="anime-bar-row">
        <div class="chips">
          <button class="chip ${docsState.mode === 'curated' ? 'on' : ''}" data-dmode="curated">Curated</button>
          <button class="chip ${docsState.mode === 'grid' && docsState.kind === 'all' ? 'on' : ''}" data-dmode="all">All Docs <span class="chip-cnt">${(d.counts?.total || 0).toLocaleString()}</span></button>
          <button class="chip ${docsState.mode === 'grid' && docsState.kind === 'movie' ? 'on' : ''}" data-dmode="movie">Movies <span class="chip-cnt">${(d.counts?.movies || 0).toLocaleString()}</span></button>
          <button class="chip ${docsState.mode === 'grid' && docsState.kind === 'tv' ? 'on' : ''}" data-dmode="tv">Series <span class="chip-cnt">${(d.counts?.series || 0).toLocaleString()}</span></button>
        </div>
        <input type="search" class="explore-search" id="dQuery" placeholder="Search docs by title, topic, subject…" value="${esc(docsState.q)}">
        <button class="btn btn-ghost btn-sm" data-surprise="docs">🎲 Pick a doc</button>
        ${viewToggleHTML(vmode)}
      </div>
      <div class="anime-bar-row" id="dTopics">
        <button class="chip sm ${docsState.topic === '' && docsState.mode === 'grid' ? 'on' : ''}" data-dt="">All topics</button>
        ${topicList.map((t) => `<button class="chip sm ${docsState.topic === t.id && docsState.mode === 'grid' ? 'on' : ''}" data-dt="${esc(t.id)}">${esc(t.label)}<span class="chip-cnt">${t.count}</span></button>`).join('')}
      </div>
      <div class="anime-bar-row">
        ${sel('dPlat', platOpts, docsState.platform)}
        ${sel('dCountry', ctryOpts, docsState.country)}
        ${sel('dDecade', DECADES, docsState.decade)}
        ${sel('dRating', RATINGS_DEEP, docsState.rmin)}
        ${sel('dRuntime', DOC_RUNTIMES, docsState.rt)}
        ${sel('dFormat', DOC_FORMATS, docsState.format)}
        ${sel('dSort', SORTS_DEEP, docsState.sort)}
        <label class="tog"><input type="checkbox" id="dUhd" ${docsState.uhd ? 'checked' : ''}> 4K (${d.counts?.uhd || 0})</label>
        <label class="tog"><input type="checkbox" id="dUnw" ${docsState.unwatched ? 'checked' : ''}> Unwatched</label>
        <button class="btn btn-ghost btn-sm" id="dReset">Reset</button>
      </div>
    </div>
    <div id="docsBody"></div><div id="docsMore" class="more-sentinel"></div></div>`;
  wireHero(heroItems, (i, h) => h.docTopics?.[0] ? `Documentary · ${h.docTopics[0]}` : 'Acclaimed Documentary');

  let page = 0; let loading = false; let done = false; let reqId = 0; let curItems = [];
  const buildParams = (pg) => {
    const q = new URLSearchParams({ kind: docsState.kind, sort: docsState.sort, page: String(pg), per: '60' });
    if (docsState.q.trim()) q.set('q', docsState.q.trim());
    if (docsState.topic) q.set('topic', docsState.topic);
    if (docsState.platform) q.set('platform', docsState.platform);
    if (docsState.country) q.set('country', docsState.country);
    if (docsState.decade) { const [a, b] = docsState.decade.split('-'); if (+a) q.set('y1', a); if (+b < 9999) q.set('y2', b); }
    if (docsState.rmin) q.set('rmin', docsState.rmin);
    if (docsState.rt) { const [a, b] = docsState.rt.split('-'); if (+a) q.set('rtmin', a); if (+b < 999) q.set('rtmax', b); }
    if (docsState.format) q.set('format', docsState.format);
    if (docsState.uhd) q.set('uhd', '1');
    if (docsState.unwatched) q.set('unwatched', '1');
    return q;
  };
  const renderGridItems = (items, append) => {
    const mode = getViewMode();
    const host = $('#docsItems');
    if (!host) return;
    if (!append) {
      host.className = mode === 'list' ? 'rich-list' : 'grid';
      host.innerHTML = items.length ? items.map(mode === 'list' ? richCard : poster).join('') : '<div class="empty" style="grid-column:1/-1">No documentaries match those filters. Try widening the topic or year.</div>';
    } else {
      host.insertAdjacentHTML('beforeend', items.map(mode === 'list' ? richCard : poster).join(''));
    }
  };
  const loadGrid = async (pg, reset) => {
    if (loading && !reset) return;
    loading = true;
    const my = ++reqId;
    const body = $('#docsBody'); if (!body) return;
    if (reset) {
      page = 0; done = false; curItems = [];
      body.innerHTML = '<div class="browse-meta" id="docsMeta">Loading…</div><div id="docsItems" class="grid"></div>';
    }
    try {
      const r = await api(`/api/docs/browse?${buildParams(pg)}`);
      if (my !== reqId || docsState.mode !== 'grid') return;
      const items = r.items || [];
      curItems = reset ? items : curItems.concat(items);
      const f = r.facets || {};
      const stats = [`<b>${(r.total || 0).toLocaleString()}</b> documentar${r.total === 1 ? 'y' : 'ies'}`];
      if (f.movies != null && f.series != null) stats.push(`${f.movies.toLocaleString()} movies · ${f.series.toLocaleString()} series`);
      if (f.uhd) stats.push(`${f.uhd} in 4K`);
      if ($('#docsMeta')) $('#docsMeta').innerHTML = `<div class="browse-stats">${stats.join(' · ')}</div>`;
      renderGridItems(items, !reset);
      page = pg;
      done = (pg + 1) * (r.per || 60) >= (r.total || 0);
    } finally {
      if (my === reqId) loading = false;
    }
  };
  const drawBody = () => {
    const body = $('#docsBody'); if (!body) return;
    if (docsState.mode === 'curated') {
      done = true; curItems = [];
      body.innerHTML = (d.rows || []).map(renderRow).join('') || '<div class="empty">No documentaries found in your library yet.</div>';
      wireRows();
      return;
    }
    loadGrid(0, true);
  };
  const syncActive = () => {
    $$('[data-dmode]').forEach((b) => b.classList.toggle('on', b.dataset.dmode === 'curated' ? docsState.mode === 'curated' : (docsState.mode === 'grid' && docsState.kind === b.dataset.dmode)));
    $$('[data-dt]').forEach((b) => b.classList.toggle('on', docsState.mode === 'grid' && docsState.topic === b.dataset.dt));
  };
  $$('[data-dmode]').forEach((b) => (b.onclick = () => {
    const m = b.dataset.dmode;
    if (m === 'curated') { docsState.mode = 'curated'; }
    else { docsState.mode = 'grid'; docsState.kind = m; }
    syncActive(); drawBody();
  }));
  $$('[data-dt]').forEach((b) => (b.onclick = () => { docsState.mode = 'grid'; docsState.topic = b.dataset.dt; syncActive(); drawBody(); }));
  $('#dQuery').oninput = debounce((e) => { docsState.q = e.target.value; if (docsState.q.trim() || docsState.mode === 'grid') { docsState.mode = 'grid'; syncActive(); drawBody(); } }, 220);
  $('#dPlat').onchange = (e) => { docsState.platform = e.target.value; docsState.mode = 'grid'; syncActive(); drawBody(); };
  $('#dCountry').onchange = (e) => { docsState.country = e.target.value; docsState.mode = 'grid'; syncActive(); drawBody(); };
  $('#dDecade').onchange = (e) => { docsState.decade = e.target.value; docsState.mode = 'grid'; syncActive(); drawBody(); };
  $('#dRating').onchange = (e) => { docsState.rmin = e.target.value; docsState.mode = 'grid'; syncActive(); drawBody(); };
  $('#dRuntime').onchange = (e) => { docsState.rt = e.target.value; docsState.mode = 'grid'; syncActive(); drawBody(); };
  $('#dFormat').onchange = (e) => { docsState.format = e.target.value; docsState.mode = 'grid'; syncActive(); drawBody(); };
  $('#dSort').onchange = (e) => { docsState.sort = e.target.value; docsState.mode = 'grid'; syncActive(); drawBody(); };
  $('#dUhd').onchange = (e) => { docsState.uhd = e.target.checked; docsState.mode = 'grid'; syncActive(); drawBody(); };
  $('#dUnw').onchange = (e) => { docsState.unwatched = e.target.checked; docsState.mode = 'grid'; syncActive(); drawBody(); };
  $('#dReset').onclick = () => {
    docsState = { mode: 'curated', kind: 'all', topic: '', platform: '', country: '', decade: '', rmin: '', rt: '', format: '', uhd: false, unwatched: false, sort: 'popular', q: '' };
    pageDocs();
  };
  $$('[data-vmode]', view).forEach((b) => (b.onclick = () => {
    setViewMode(b.dataset.vmode);
    $$('[data-vmode]', view).forEach((x) => x.classList.toggle('on', x === b));
    if (docsState.mode === 'grid') renderGridItems(curItems, false);
    else { docsState.mode = 'grid'; syncActive(); drawBody(); }
  }));
  const io = trackObserver(new IntersectionObserver((ents) => { if (ents[0].isIntersecting && docsState.mode === 'grid' && !done && !loading) loadGrid(page + 1, false); }, { rootMargin: '800px' }));
  io.observe($('#docsMore'));
  drawBody();
}

let animeState = { mode: 'curated', kind: 'all', g: '', q: '', decade: '', rmin: '', platform: '', status: '', dub: '', noFiller: false, uhd: false, unwatched: false, sort: 'popular' };
async function pageAnime() {
  const my = routeGen;
  const d = await apiCached('/api/anime', 120000).catch(() => ({}));
  if (my !== routeGen) return;
  if (!d || !Array.isArray(d.rows)) {
    view.innerHTML = `<div class="page-pad"><div class="page-head"><h1 class="page-title">Anime</h1></div>
      <div class="banner">${I.info}<span><b>Server update queued.</b> Once the update swaps in, press <b>Ctrl+R</b> to load everything.</span></div></div>`;
    return;
  }
  const heroItems = d.hero || [];
  const gCnt = d.genres || {};
  const anGenres = [['', 'All genres'], ['10759', 'Action & Shonen'], ['10765', 'Fantasy & Sci-Fi'], ['18', 'Drama'], ['35', 'Comedy'], ['9648', 'Mystery'], ['80', 'Crime & Dark']];
  const platList = (d.platforms || []).length
    ? d.platforms.map((p) => [p.name.toLowerCase().replace(/[^a-z]/g, ''), `${p.name} (${p.count})`])
    : [['crunchyroll', 'Crunchyroll'], ['netflix', 'Netflix'], ['disney', 'Disney+'], ['amazon', 'Amazon'], ['hbo', 'HBO']];
  const anPlats = [['', 'All platforms'], ...platList];
  const anStatus = [['', 'All statuses'], ['airing', 'Airing / Returning'], ['ended', 'Completed'], ['short', 'Short & Complete (1–2S)']];
  const anDub = [['', 'English Dubbed'], ['verified', 'Verified Dubs Only'], ['all', 'All (incl. Sub-Only)']];
  const vmode = getViewMode();
  const totalCnt = d.counts?.total ?? ((d.counts?.series || 0) + (d.counts?.movies || 0));
  const skipMatched = d.skipStats?.matched || 0;
  view.innerHTML = heroHTML(heroItems) + `<div class="rows">
    <div class="anime-bar">
      <div class="anime-bar-row">
        <div class="chips">
          <button class="chip ${animeState.mode === 'curated' ? 'on' : ''}" data-amode="curated">Curated</button>
          <button class="chip ${animeState.mode === 'grid' && animeState.kind === 'all' ? 'on' : ''}" data-amode="all">All Anime <span class="chip-cnt">${totalCnt}</span></button>
          <button class="chip ${animeState.mode === 'grid' && animeState.kind === 'tv' ? 'on' : ''}" data-amode="tv">Series <span class="chip-cnt">${d.counts?.series || 0}</span></button>
          <button class="chip ${animeState.mode === 'grid' && animeState.kind === 'movie' ? 'on' : ''}" data-amode="movie">Movies <span class="chip-cnt">${d.counts?.movies || 0}</span></button>
        </div>
        <input type="search" class="explore-search" id="aQuery" placeholder="Search anime by title, plot, genre…" value="${esc(animeState.q)}">
        ${skipMatched > 0 ? `<span class="pill plat" title="AniSkip + IntroDB + TheIntroDB auto-skip active for OP, Recap & ED">⚡ Auto-Skip Active</span>` : ''}
        <button class="btn btn-ghost btn-sm" data-surprise="anime">🎲 Pick anime</button>
        ${viewToggleHTML(vmode)}
      </div>
      <div class="anime-bar-row">
        <div class="chips">${anGenres.map(([id, lab]) => {
          const c = id ? (gCnt[id] || 0) : 0;
          return `<button class="chip sm ${animeState.g === id && animeState.mode === 'grid' ? 'on' : ''}" data-ag="${id}">${esc(lab)}${c ? `<span class="chip-cnt">${c}</span>` : ''}</button>`;
        }).join('')}</div>
      </div>
      <div class="anime-bar-row">
        ${sel('aPlat', anPlats, animeState.platform)}
        ${sel('aStatus', anStatus, animeState.status)}
        ${sel('aDub', anDub, animeState.dub)}
        ${sel('aDecade', DECADES, animeState.decade)}
        ${sel('aRating', RATINGS_DEEP, animeState.rmin)}
        ${sel('aSort', [['popular', 'Popular'], ['rating', 'Top rated'], ['votes', 'Most voted'], ['newest', 'Newest'], ['oldest', 'Oldest'], ['added', 'Recently added'], ['az', 'A–Z']], animeState.sort)}
        <label class="tog"><input type="checkbox" id="aNoFill" ${animeState.noFiller ? 'checked' : ''}> Low filler (&lt;20%)</label>
        <label class="tog"><input type="checkbox" id="aUhd" ${animeState.uhd ? 'checked' : ''}> 4K${d.counts?.uhd ? ` (${d.counts.uhd})` : ''}</label>
        <label class="tog"><input type="checkbox" id="aUnw" ${animeState.unwatched ? 'checked' : ''}> Unwatched</label>
        <button class="btn btn-ghost btn-sm" id="aReset">Reset</button>
      </div>
    </div>
    <div id="animeBody"></div><div id="animeMore" class="more-sentinel"></div></div>`;
  wireHero(heroItems, (i, h) => h.k === 'm' ? 'Acclaimed Anime Film' : h.year >= 2024 ? `New & Trending Anime · ${h.year}` : `#${i + 1} Trending Anime`);

  let page = 0; let loading = false; let done = false; let reqId = 0; let curItems = [];
  const buildParams = (pg) => {
    const q = new URLSearchParams({ kind: animeState.kind, sort: animeState.sort, page: String(pg), per: '60' });
    if (animeState.q.trim()) q.set('q', animeState.q.trim());
    if (animeState.g) q.set('g', animeState.g);
    if (animeState.platform) q.set('platform', animeState.platform);
    if (animeState.status) q.set('status', animeState.status);
    if (animeState.dub) q.set('dub', animeState.dub);
    if (animeState.decade) { const [a, b] = animeState.decade.split('-'); if (+a) q.set('y1', a); if (+b < 9999) q.set('y2', b); }
    if (animeState.rmin) q.set('rmin', animeState.rmin);
    if (animeState.uhd) q.set('uhd', '1');
    if (animeState.unwatched) q.set('unwatched', '1');
    if (animeState.noFiller) q.set('noFiller', '1');
    return q;
  };
  const renderGridItems = (items, append) => {
    const mode = getViewMode();
    const host = $('#animeItems');
    if (!host) return;
    if (!append) {
      host.className = mode === 'list' ? 'rich-list' : 'grid';
      host.innerHTML = items.length ? items.map(mode === 'list' ? richCard : poster).join('') : '<div class="empty" style="grid-column:1/-1">No anime matches those filters. Try widening the genre or status.</div>';
    } else {
      host.insertAdjacentHTML('beforeend', items.map(mode === 'list' ? richCard : poster).join(''));
    }
  };
  const loadGrid = async (pg, reset) => {
    if (loading && !reset) return;
    loading = true;
    const my = ++reqId;
    const body = $('#animeBody'); if (!body) return;
    if (reset) {
      page = 0; done = false; curItems = [];
      body.innerHTML = '<div class="browse-meta" id="animeMeta">Loading…</div><div id="animeItems" class="grid"></div>';
    }
    try {
      const r = await api(`/api/anime/browse?${buildParams(pg)}`);
      if (my !== reqId || animeState.mode !== 'grid') return;
      const items = r.items || [];
      curItems = reset ? items : curItems.concat(items);
      const f = r.facets || {};
      const bits = [`<b>${(r.total || 0).toLocaleString()}</b> anime title${r.total === 1 ? '' : 's'}`];
      if (f.series != null && f.movies != null) bits.push(`${f.series} series · ${f.movies} films`);
      if (f.verified) bits.push(`${f.verified} verified dubs`);
      if (f.uhd) bits.push(`${f.uhd} in 4K`);
      if ($('#animeMeta')) $('#animeMeta').innerHTML = `<div class="browse-stats">${bits.join(' · ')}</div>`;
      renderGridItems(items, !reset);
      page = pg;
      done = (pg + 1) * (r.per || 60) >= (r.total || 0);
    } finally {
      if (my === reqId) loading = false;
    }
  };
  const drawBody = () => {
    const body = $('#animeBody'); if (!body) return;
    if (animeState.mode === 'curated') {
      done = true; curItems = [];
      body.innerHTML = (d.rows || []).map(renderRow).join('') || '<div class="empty">No anime found in your library yet.</div>';
      wireRows();
      return;
    }
    loadGrid(0, true);
  };
  const syncActive = () => {
    $$('[data-amode]').forEach((b) => b.classList.toggle('on', b.dataset.amode === 'curated' ? animeState.mode === 'curated' : (animeState.mode === 'grid' && animeState.kind === b.dataset.amode)));
    $$('[data-ag]').forEach((b) => b.classList.toggle('on', animeState.mode === 'grid' && animeState.g === b.dataset.ag));
  };
  $$('[data-amode]').forEach((b) => (b.onclick = () => {
    const m = b.dataset.amode;
    if (m === 'curated') { animeState.mode = 'curated'; }
    else { animeState.mode = 'grid'; animeState.kind = m; }
    syncActive(); drawBody();
  }));
  $$('[data-ag]').forEach((b) => (b.onclick = () => { animeState.mode = 'grid'; animeState.g = b.dataset.ag; syncActive(); drawBody(); }));
  $('#aQuery').oninput = debounce((e) => { animeState.q = e.target.value; if (animeState.q.trim() || animeState.mode === 'grid') { animeState.mode = 'grid'; syncActive(); drawBody(); } }, 220);
  $('#aPlat').onchange = (e) => { animeState.platform = e.target.value; animeState.mode = 'grid'; syncActive(); drawBody(); };
  $('#aStatus').onchange = (e) => { animeState.status = e.target.value; animeState.mode = 'grid'; syncActive(); drawBody(); };
  $('#aDub').onchange = (e) => { animeState.dub = e.target.value; animeState.mode = 'grid'; syncActive(); drawBody(); };
  $('#aDecade').onchange = (e) => { animeState.decade = e.target.value; animeState.mode = 'grid'; syncActive(); drawBody(); };
  $('#aRating').onchange = (e) => { animeState.rmin = e.target.value; animeState.mode = 'grid'; syncActive(); drawBody(); };
  $('#aNoFill').onchange = (e) => { animeState.noFiller = e.target.checked; animeState.mode = 'grid'; syncActive(); drawBody(); };
  $('#aUhd').onchange = (e) => { animeState.uhd = e.target.checked; animeState.mode = 'grid'; syncActive(); drawBody(); };
  $('#aUnw').onchange = (e) => { animeState.unwatched = e.target.checked; animeState.mode = 'grid'; syncActive(); drawBody(); };
  $('#aSort').onchange = (e) => { animeState.sort = e.target.value; animeState.mode = 'grid'; syncActive(); drawBody(); };
  $('#aReset').onclick = () => { animeState = { mode: 'curated', kind: 'all', g: '', q: '', decade: '', rmin: '', platform: '', status: '', dub: '', noFiller: false, uhd: false, unwatched: false, sort: 'popular' }; pageAnime(); };
  $$('[data-vmode]', view).forEach((b) => (b.onclick = () => {
    setViewMode(b.dataset.vmode);
    $$('[data-vmode]', view).forEach((x) => x.classList.toggle('on', x === b));
    if (animeState.mode === 'grid') renderGridItems(curItems, false);
    else { animeState.mode = 'grid'; syncActive(); drawBody(); }
  }));
  const io = trackObserver(new IntersectionObserver((ents) => { if (ents[0].isIntersecting && animeState.mode === 'grid' && !done && !loading) loadGrid(page + 1, false); }, { rootMargin: '800px' }));
  io.observe($('#animeMore'));
  drawBody();
}

// ---------------------------------------------------------------- 4K OLED Showcase & CAM Watch (#/4k)
let scState = { mode: 'curated', tier: 'all', kind: 'all', platform: '', genre: '', rmin: '', unwatched: false, sort: 'trending', q: '' };
async function pageShowcase(qs) {
  if (qs) {
    const p = new URLSearchParams(qs);
    if (p.get('tier')) { scState.mode = 'grid'; scState.tier = p.get('tier'); }
    if (p.get('kind')) { scState.mode = 'grid'; scState.kind = p.get('kind'); }
  }
  const my = routeGen;
  const d = await apiCached('/api/showcase', 120000).catch(() => null);
  if (my !== routeGen) return;
  if (!d || !Array.isArray(d.rows)) {
    view.innerHTML = '<div class="page-pad"><div class="page-head"><h1 class="page-title">4K OLED Showcase</h1></div><div class="empty">Loading showcase…</div></div>';
    return;
  }
  const heroItems = d.hero || [];
  const cnt = d.counts || {};
  const platOpts = [['', 'All platforms'], ...(d.platforms || []).map((p) => [p.name.toLowerCase().replace(/[^a-z]/g, ''), `${p.name} (${p.count})`])];
  const genreOpts = [['', 'All genres'], ...(d.genres || []).map((g) => [String(g.id), `${g.name} (${g.count})`])];
  const scSorts = [['trending', 'Trending'], ['rating', 'Highest rated'], ['newest', 'Newest year'], ['added', 'Recently added'], ['tier', 'Highest quality (DV first)'], ['az', 'A–Z']];
  const vmode = getViewMode();
  view.innerHTML = heroHTML(heroItems) + `<div class="rows">
    <div class="anime-bar">
      <div class="anime-bar-row">
        <div class="chips">
          <button class="chip ${scState.mode === 'curated' ? 'on' : ''}" data-scmode="curated">Curated OLED</button>
          <button class="chip ${scState.mode === 'grid' && scState.tier === 'all' ? 'on' : ''}" data-sctier="all">All 4K &amp; Blu-Ray <span class="chip-cnt">${(cnt.total || 0).toLocaleString()}</span></button>
          <button class="chip ${scState.mode === 'grid' && scState.tier === 'dv' ? 'on' : ''}" data-sctier="dv">4K Dolby Vision <span class="chip-cnt">${(cnt.dv || 0).toLocaleString()}</span></button>
          <button class="chip ${scState.mode === 'grid' && scState.tier === '4k' ? 'on' : ''}" data-sctier="4k">4K UHD <span class="chip-cnt">${(cnt.uhd4k || 0).toLocaleString()}</span></button>
          <button class="chip ${scState.mode === 'grid' && scState.tier === 'bluray' ? 'on' : ''}" data-sctier="bluray">Blu-Ray Masters <span class="chip-cnt">${(cnt.bluray || 0).toLocaleString()}</span></button>
          <button class="chip ${scState.mode === 'grid' && scState.tier === 'cam' ? 'on' : ''}" data-sctier="cam">CAM-Only Watch <span class="chip-cnt">${(cnt.camOnly || 0).toLocaleString()}</span></button>
        </div>
        <input type="search" class="explore-search" id="scQuery" placeholder="Search 4K Dolby Vision, HDR, Blu-Ray…" value="${esc(scState.q)}">
        ${viewToggleHTML(vmode)}
      </div>
      <div class="anime-bar-row">
        <div class="chips">
          ${[['all', 'All'], ['movie', 'Movies'], ['tv', 'Series'], ['docs', 'Docs']].map(([k, l]) => `<button class="chip sm ${scState.kind === k && scState.mode === 'grid' ? 'on' : ''}" data-sckind="${k}">${l}</button>`).join('')}
        </div>
        ${sel('scPlat', platOpts, scState.platform)}
        ${sel('scGenre', genreOpts, scState.genre)}
        ${sel('scRating', RATINGS_DEEP, scState.rmin)}
        ${sel('scSort', scSorts, scState.sort)}
        <label class="tog"><input type="checkbox" id="scUnw" ${scState.unwatched ? 'checked' : ''}> Unwatched</label>
        <button class="btn btn-ghost btn-sm" id="scReset">Reset</button>
      </div>
    </div>
    <div id="scBody"></div><div id="scMore" class="more-sentinel"></div></div>`;
  wireHero(heroItems, (i, h) => `${h.qualityLabel || '4K OLED'} Showcase${h.platforms?.[0] ? ` · ${h.platforms[0]}` : ''}`);

  let page = 0; let loading = false; let done = false; let reqId = 0; let curItems = [];
  const loadGrid = async (pg, reset) => {
    if (loading && !reset) return;
    loading = true; const my = ++reqId;
    const body = $('#scBody'); if (!body) return;
    if (reset) { page = 0; done = false; curItems = []; body.innerHTML = '<div class="browse-meta" id="scMeta">Loading…</div><div id="scItems" class="grid"></div>'; }
    const q = new URLSearchParams({ tier: scState.tier, kind: scState.kind, sort: scState.sort, page: String(pg), limit: '60' });
    if (scState.q.trim()) q.set('q', scState.q.trim());
    if (scState.platform) q.set('platform', scState.platform);
    if (scState.genre) q.set('genre', scState.genre);
    if (scState.rmin) q.set('minRating', scState.rmin);
    if (scState.unwatched) q.set('unwatched', '1');
    try {
      const r = await api(`/api/showcase/browse?${q}`);
      if (my !== reqId || scState.mode !== 'grid') return;
      const items = r.items || [];
      curItems = reset ? items : curItems.concat(items);
      const tc = r.tierCounts || {};
      const stats = [`<b>${(r.total || 0).toLocaleString()}</b> ${scState.tier === 'cam' ? 'CAM-only releases' : 'verified 4K / Blu-Ray titles'}`];
      if (scState.tier !== 'cam' && tc.dv) stats.push(`${tc.dv.toLocaleString()} Dolby Vision`);
      if (scState.tier !== 'cam' && tc['4k']) stats.push(`${tc['4k'].toLocaleString()} 4K UHD`);
      if (scState.tier !== 'cam' && tc.bluray) stats.push(`${tc.bluray.toLocaleString()} Blu-Ray`);
      if ($('#scMeta')) $('#scMeta').innerHTML = `<div class="browse-stats">${stats.join(' · ')}</div>`;
      const host = $('#scItems'); const mode = getViewMode();
      if (host) {
        if (reset) { host.className = mode === 'list' ? 'rich-list' : 'grid'; host.innerHTML = items.length ? items.map(mode === 'list' ? richCard : poster).join('') : '<div class="empty" style="grid-column:1/-1">No titles match those filters.</div>'; }
        else host.insertAdjacentHTML('beforeend', items.map(mode === 'list' ? richCard : poster).join(''));
      }
      page = pg; done = !r.more;
    } finally { if (my === reqId) loading = false; }
  };
  const syncActive = () => {
    $$('[data-scmode]').forEach((b) => b.classList.toggle('on', scState.mode === 'curated'));
    $$('[data-sctier]').forEach((b) => b.classList.toggle('on', scState.mode === 'grid' && b.dataset.sctier === scState.tier));
    $$('[data-sckind]').forEach((b) => b.classList.toggle('on', scState.mode === 'grid' && b.dataset.sckind === scState.kind));
  };
  const drawBody = () => {
    const body = $('#scBody'); if (!body) return;
    if (scState.mode === 'curated') {
      body.innerHTML = d.rows.map(renderRow).join('') || '<div class="empty">No 4K Showcase titles found yet.</div>';
      wireRows();
    } else loadGrid(0, true);
  };
  const io = trackObserver(new IntersectionObserver((es) => { if (es[0]?.isIntersecting && scState.mode === 'grid' && !done && !loading) loadGrid(page + 1, false); }, { rootMargin: '600px' }));
  const sen = $('#scMore'); if (sen) io.observe(sen);
  $('[data-scmode="curated"]').onclick = () => { scState.mode = 'curated'; syncActive(); drawBody(); };
  $$('[data-sctier]').forEach((b) => (b.onclick = () => { scState.mode = 'grid'; scState.tier = b.dataset.sctier; syncActive(); drawBody(); }));
  $$('[data-sckind]').forEach((b) => (b.onclick = () => { scState.mode = 'grid'; scState.kind = b.dataset.sckind; syncActive(); drawBody(); }));
  $('#scQuery').oninput = debounce((e) => { scState.q = e.target.value; scState.mode = 'grid'; syncActive(); drawBody(); }, 220);
  $('#scPlat').onchange = (e) => { scState.platform = e.target.value; scState.mode = 'grid'; syncActive(); drawBody(); };
  $('#scGenre').onchange = (e) => { scState.genre = e.target.value; scState.mode = 'grid'; syncActive(); drawBody(); };
  $('#scRating').onchange = (e) => { scState.rmin = e.target.value; scState.mode = 'grid'; syncActive(); drawBody(); };
  $('#scSort').onchange = (e) => { scState.sort = e.target.value; scState.mode = 'grid'; syncActive(); drawBody(); };
  $('#scUnw').onchange = (e) => { scState.unwatched = e.target.checked; scState.mode = 'grid'; syncActive(); drawBody(); };
  $('#scReset').onclick = () => { scState = { mode: 'curated', tier: 'all', kind: 'all', platform: '', genre: '', rmin: '', unwatched: false, sort: 'trending', q: '' }; pageShowcase(); };
  $$('[data-vmode]', view).forEach((b) => (b.onclick = () => {
    setViewMode(b.dataset.vmode);
    $$('[data-vmode]', view).forEach((x) => x.classList.toggle('on', x === b));
    if (scState.mode === 'grid' && curItems.length) {
      const host = $('#scItems'); const mode = getViewMode();
      if (host) { host.className = mode === 'list' ? 'rich-list' : 'grid'; host.innerHTML = curItems.map(mode === 'list' ? richCard : poster).join(''); }
    } else { scState.mode = 'grid'; syncActive(); drawBody(); }
  }));
  drawBody();
}

// ---------------------------------------------------------------- UFC & Sports Replays Hub (#/fights)
let fightState = { mode: 'curated', sport: 'ufc', fighter: '', year: '', q: '', unwatched: false, sort: 'latest' };
const ufcSpoilerFree = () => localGet('ufcSpoilerFree', '0') === '1';

function playFightReplay(f, fresh = false) {
  const sf = ufcSpoilerFree();
  Player.open({
    kind: 'movie', id: f.id, ext: f.ext || 'mp4',
    title: f.title || f.cleanTitle || 'Fight Replay',
    sub: [f.matchup || f.sportLabel, f.dateStr || f.year].filter(Boolean).join(' · '),
    resume: fresh ? 0 : (f.progress < 0.92 ? (f.resume || 0) : 0),
    duration: f.duration || 0,
    spoilerFree: sf && (f.sportKey === 'ufc' || f.sportKey === 'boxing'),
    progressKey: `m:${f.id}`, tmdb: f.tmdb || null, historyKind: 'm', historyTitle: f.title,
    item: { k: 'm', id: f.id, title: f.title, year: f.year, poster: f.poster, backdrop: f.backdrop },
    label: f.cat || f.sportLabel || 'Main',
    alternates: (f.alternates || []).map((a) => ({ id: a.id, ext: a.ext, label: a.label })),
  });
}

async function openFightCardModal(f) {
  openModal(`<div class="sheet" style="max-width:740px"><button class="close" aria-label="Close">${I.close}</button>
    <div class="sheet-section" style="padding:28px 32px">
      <div style="display:flex;align-items:center;gap:10px;flex-wrap:wrap">
        <span class="fbadge ppv">${esc(f.eventNum ? `UFC ${f.eventNum}` : (f.sportLabel || 'UFC'))}</span>
        <span class="spoiler-bar">🛡️ 100% Spoiler-Free Card — Zero results shown</span>
      </div>
      <h2 style="margin:12px 0 4px">${esc(f.title || f.cleanTitle)}</h2>
      <div class="muted" id="fcMeta">${esc(f.matchup || '')}${f.dateStr ? ` · ${esc(f.dateStr)}` : ''}</div>
      <div class="btns" style="margin-top:14px">
        <button class="btn btn-primary btn-sm" id="fcPlay">${I.play}${f.resume > 30 && f.progress < 0.92 && !ufcSpoilerFree() ? `Resume (${fmtTime(f.resume)})` : 'Watch Replay'}</button>
        ${f.resume > 30 ? '<button class="btn btn-ghost btn-sm" id="fcRestart">⟲ Start from beginning</button>' : ''}
      </div>
      <div id="fcBody" style="margin-top:18px"><p class="muted">Loading spoiler-free fight card lineup…</p></div>
    </div></div>`);
  $('#fcPlay').onclick = () => { closeModal(true); playFightReplay(f, false); };
  if ($('#fcRestart')) $('#fcRestart').onclick = () => { closeModal(true); playFightReplay(f, true); };
  const r = await api(`/api/fights/card/${f.id}`).catch(() => null);
  const body = $('#fcBody'); if (!body || !body.isConnected) return;
  if (!r || !r.found || !r.bouts?.length) {
    body.innerHTML = `<div class="empty" style="padding:20px 0">No bout-by-bout card published on ESPN for this event yet — hit <b>Watch Replay</b> above to jump straight into the broadcast!</div>`;
    return;
  }
  if ($('#fcMeta')) $('#fcMeta').textContent = [r.sydneyDate || r.date, r.venue].filter(Boolean).join(' · ');
  body.innerHTML = `<div class="bout-list">${r.bouts.map((b) => `<div class="bout-row ${b.isMainEvent ? 'main' : ''}">
    <div class="bout-sec">${esc(b.section || b.card || 'Bout')}</div>
    <div><div class="bout-vs">${esc(b.fighterA)} <span class="muted">vs.</span> ${esc(b.fighterB)}${b.isTitle ? ' <span class="pill uhd" style="margin-left:6px">TITLE</span>' : ''}</div>
      ${b.weightClass ? `<div class="bout-wc">${esc(b.weightClass)}</div>` : ''}</div>
    <span class="bout-rds">${b.rounds || 3} Rds</span>
  </div>`).join('')}</div>`;
}

const FIGHT_ITEMS = new Map();
function fightCardHTML(f) {
  const fk = `f:${f.id}`;
  FIGHT_ITEMS.set(fk, f);
  const sf = ufcSpoilerFree();
  const img = f.backdrop || f.poster;
  const tag = f.eventNum && f.eventKind === 'ppv' ? `<span class="fbadge ppv">UFC ${f.eventNum}</span>`
    : f.eventKind === 'fn' ? `<span class="fbadge">${f.eventNum ? `Fight Night ${f.eventNum}` : 'Fight Night'}</span>`
    : `<span class="fbadge">${esc(f.sportLabel || 'Replay')}</span>`;
  const showProg = !sf && f.progress > 0.02 && f.progress < 0.94;
  return `<article class="fcard" data-fight-play="${fk}" tabindex="0" role="button">
    <div class="fcard-thumb">
      <div class="fcard-ph">${esc(f.eventNum ? `UFC ${f.eventNum}` : (f.sportLabel || 'UFC'))}</div>
      ${img ? `<img src="${esc(img)}" alt="" loading="lazy" onerror="this.remove()">` : ''}
      <div class="fcard-badges">${tag}${f.isAnz ? '<span class="fbadge anz">ANZ</span>' : ''}${f.watched ? '<span class="fbadge watched">✓ Watched</span>' : ''}</div>
      ${showProg ? `<div class="bar"><i style="width:${Math.min(100, Math.round(f.progress * 100))}%"></i></div>` : ''}
    </div>
    <div class="fcard-body">
      <div class="fcard-title">${esc(f.cleanTitle || f.title)}</div>
      ${f.matchup ? `<div class="fcard-match">${esc(f.matchup)}</div>` : ''}
      <div class="fcard-meta">${f.dateStr ? `<span>${esc(f.dateStr)}</span>` : (f.year ? `<span>${f.year}</span>` : '')}${f.variantCount > 1 ? `<span>${f.variantCount} feeds</span>` : ''}${!sf && f.resume > 30 && !f.watched ? `<span>${fmtTime(f.resume)} watched</span>` : ''}</div>
      <div class="fcard-acts">
        <button class="btn btn-primary btn-sm" data-fight-play="${fk}">${I.play}${!sf && f.resume > 30 && !f.watched ? 'Resume' : 'Watch'}</button>
        ${f.sportKey === 'ufc' ? `<button class="btn btn-ghost btn-sm" data-fight-card="${fk}">🥊 Fight Card</button>` : ''}
      </div>
    </div>
  </article>`;
}

async function pageFights(qs) {
  if (qs) {
    const p = new URLSearchParams(qs);
    if (p.get('sport')) { fightState.mode = 'grid'; fightState.sport = p.get('sport'); }
    if (p.get('fighter')) { fightState.mode = 'grid'; fightState.fighter = p.get('fighter'); }
  }
  const my = routeGen;
  const d = await apiCached('/api/fights', 120000).catch(() => null);
  if (my !== routeGen) return;
  if (!d || !Array.isArray(d.rows)) {
    view.innerHTML = '<div class="page-pad"><div class="page-head"><h1 class="page-title">UFC &amp; Sports Replays</h1></div><div class="empty">Loading fight library…</div></div>';
    return;
  }
  const h = d.hero;
  const cnt = d.counts || {};
  const sf = ufcSpoilerFree();
  if (h) FIGHT_ITEMS.set(`f:${h.id}`, h);
  const heroBg = h?.backdrop || h?.poster || '';
  const heroHtml = h ? `<section class="hero" style="min-height:420px;height:min(62vh,560px)">
    <div class="hero-slide on">${heroBg ? `<img src="${esc(heroBg)}" alt="" onerror="this.remove()">` : ''}</div>
    <div class="hero-info">
      <div class="hero-kicker">🥊 Latest UFC PPV Replay · ${cnt.ufc || 0} UFC Events Ready</div>
      <h1 class="hero-title">${esc(h.cleanTitle || h.title)}</h1>
      <div class="hero-meta">${h.matchup ? `<b style="color:var(--accent)">${esc(h.matchup)}</b>` : ''}${h.dateStr ? `<span>${esc(h.dateStr)}</span>` : (h.year ? `<span>${h.year}</span>` : '')}${h.variantCount > 1 ? `<span class="pill">${h.variantCount} backup feeds</span>` : ''}${h.isAnz ? '<span class="pill" style="border-color:#3ddc97;color:#7af2be">Aussie / Kiwi Headliner</span>' : ''}</div>
      <div class="btns">
        <button class="btn btn-primary" id="fhPlay">${I.play}Watch Replay</button>
        <button class="btn btn-ghost" id="fhCard">🥊 Spoiler-Free Fight Card</button>
        <button class="btn btn-ghost" id="fhSpoiler">${sf ? '🛡️ Spoiler-Free: ON' : '🛡️ Spoiler-Free: OFF'}</button>
        <a class="btn btn-ghost" href="#/radar">📅 Upcoming UFC Schedule</a>
      </div>
    </div>
  </section>` : '<div class="page-pad"></div>';

  const sportTabs = [
    ['ufc', 'UFC All', cnt.ufc], ['ppv', 'UFC PPVs', cnt.ppv], ['fn', 'Fight Nights', cnt.fn], ['anz', 'Aussie & Kiwi', cnt.anz],
    ['boxing', 'Boxing', cnt.boxing], ['f1', 'Formula 1', cnt.f1], ['rugby', 'Rugby', cnt.rugby], ['afl', 'AFL', cnt.afl],
    ['nba', 'NBA', cnt.nba], ['nfl', 'NFL', cnt.nfl], ['football', 'Football', cnt.football], ['wwe', 'WWE', cnt.wwe],
  ].filter(([, , n]) => (n || 0) > 0);
  const yrOpts = [['', 'All years'], ...(d.years || []).map((y) => [String(y), String(y)])];
  const fSorts = [['latest', 'Latest first'], ['num_desc', 'UFC # High → Low'], ['num_asc', 'UFC # Low → High'], ['oldest', 'Oldest first'], ['title', 'A–Z']];

  view.innerHTML = heroHtml + `<div class="rows">
    <div class="anime-bar">
      <div class="anime-bar-row">
        <div class="chips">
          <button class="chip ${fightState.mode === 'curated' ? 'on' : ''}" data-fmode="curated">Curated</button>
          ${sportTabs.map(([k, l, n]) => `<button class="chip ${fightState.mode === 'grid' && fightState.sport === k ? 'on' : ''}" data-fsport="${k}">${esc(l)} <span class="chip-cnt">${(n || 0).toLocaleString()}</span></button>`).join('')}
        </div>
        <input type="search" class="explore-search" id="fQuery" placeholder="Search fighter, UFC #, team (e.g. 322, Volkanovski)…" value="${esc(fightState.q)}">
      </div>
      ${(d.topFighters || []).length ? `<div class="anime-bar-row">
        <span class="muted" style="font-size:12px;font-weight:700;text-transform:uppercase;letter-spacing:.05em">Fighters:</span>
        <button class="chip sm ${!fightState.fighter && fightState.mode === 'grid' ? 'on' : ''}" data-ffighter="">All</button>
        ${d.topFighters.slice(0, 16).map((f) => `<button class="chip sm ${fightState.fighter === f.name ? 'on' : ''}" data-ffighter="${esc(f.name)}">${esc(f.name)} <span class="chip-cnt">${f.count}</span></button>`).join('')}
      </div>` : ''}
      <div class="anime-bar-row">
        ${sel('fYear', yrOpts, fightState.year)}
        ${sel('fSort', fSorts, fightState.sort)}
        <label class="tog"><input type="checkbox" id="fUnw" ${fightState.unwatched ? 'checked' : ''}> Unwatched only</label>
        <label class="tog"><input type="checkbox" id="fSpoil" ${sf ? 'checked' : ''}> 🛡️ Spoiler-free mode (hide runtime &amp; progress bars)</label>
        <button class="btn btn-ghost btn-sm" id="fReset">Reset</button>
      </div>
    </div>
    <div id="fightsBody"></div><div id="fightsMore" class="more-sentinel"></div></div>`;

  if (h) {
    $('#fhPlay').onclick = () => playFightReplay(h);
    $('#fhCard').onclick = () => openFightCardModal(h);
    $('#fhSpoiler').onclick = () => {
      const next = !ufcSpoilerFree();
      localSet('ufcSpoilerFree', next ? '1' : '0');
      post('/api/prefs', { ufcSpoilerFree: next }).catch(() => {});
      toast(next ? 'Spoiler-Free UFC mode ON (runtimes & progress bars hidden)' : 'Spoiler-Free UFC mode OFF');
      pageFights();
    };
  }

  let page = 0; let loading = false; let done = false; let reqId = 0;
  const loadGrid = async (pg, reset) => {
    if (loading && !reset) return;
    loading = true; const my = ++reqId;
    const body = $('#fightsBody'); if (!body) return;
    if (reset) { page = 0; done = false; body.innerHTML = '<div class="browse-meta" id="fMeta">Loading…</div><div id="fItems" class="fight-grid"></div>'; }
    const q = new URLSearchParams({ sport: fightState.sport, sort: fightState.sort, page: String(pg), limit: '60' });
    if (fightState.fighter) q.set('fighter', fightState.fighter);
    if (fightState.year) q.set('year', fightState.year);
    if (fightState.q.trim()) q.set('q', fightState.q.trim());
    if (fightState.unwatched) q.set('unwatched', '1');
    try {
      const r = await api(`/api/fights/browse?${q}`);
      if (my !== reqId || fightState.mode !== 'grid') return;
      const items = r.items || [];
      if ($('#fMeta')) $('#fMeta').innerHTML = `<div class="browse-stats"><b>${(r.total || 0).toLocaleString()}</b> replay${r.total === 1 ? '' : 's'}${fightState.fighter ? ` featuring <b>${esc(fightState.fighter)}</b>` : ''}</div>`;
      const host = $('#fItems');
      if (host) {
        if (reset) host.innerHTML = items.length ? items.map(fightCardHTML).join('') : '<div class="empty" style="grid-column:1/-1">No replays match those filters.</div>';
        else host.insertAdjacentHTML('beforeend', items.map(fightCardHTML).join(''));
      }
      page = pg; done = !r.more;
    } finally { if (my === reqId) loading = false; }
  };
  const syncActive = () => {
    $$('[data-fmode]').forEach((b) => b.classList.toggle('on', fightState.mode === 'curated'));
    $$('[data-fsport]').forEach((b) => b.classList.toggle('on', fightState.mode === 'grid' && b.dataset.fsport === fightState.sport));
    $$('[data-ffighter]').forEach((b) => b.classList.toggle('on', fightState.mode === 'grid' && b.dataset.ffighter === fightState.fighter));
  };
  const drawBody = () => {
    const body = $('#fightsBody'); if (!body) return;
    if (fightState.mode === 'curated') {
      body.innerHTML = d.rows.map((r) => `<section class="row" data-row="${esc(r.id)}">
        <div class="row-head"><h2 class="row-title">${esc(r.title)}</h2>${r.sub ? `<span class="row-sub">${esc(r.sub)}</span>` : ''}</div>
        <div class="row-wrap"><button class="arrow l" aria-label="Scroll left">${I.left}</button>
          <div class="scroller">${(r.items || []).map(fightCardHTML).join('')}</div>
          <button class="arrow r" aria-label="Scroll right">${I.right}</button></div></section>`).join('');
      wireRows();
    } else loadGrid(0, true);
  };
  const io = trackObserver(new IntersectionObserver((es) => { if (es[0]?.isIntersecting && fightState.mode === 'grid' && !done && !loading) loadGrid(page + 1, false); }, { rootMargin: '600px' }));
  const sen = $('#fightsMore'); if (sen) io.observe(sen);
  $('[data-fmode="curated"]').onclick = () => { fightState.mode = 'curated'; syncActive(); drawBody(); };
  $$('[data-fsport]').forEach((b) => (b.onclick = () => { fightState.mode = 'grid'; fightState.sport = b.dataset.fsport; fightState.fighter = ''; syncActive(); drawBody(); }));
  $$('[data-ffighter]').forEach((b) => (b.onclick = () => { fightState.mode = 'grid'; fightState.sport = 'ufc'; fightState.fighter = b.dataset.ffighter; syncActive(); drawBody(); }));
  $('#fQuery').oninput = debounce((e) => { fightState.q = e.target.value; fightState.mode = 'grid'; syncActive(); drawBody(); }, 220);
  $('#fYear').onchange = (e) => { fightState.year = e.target.value; fightState.mode = 'grid'; syncActive(); drawBody(); };
  $('#fSort').onchange = (e) => { fightState.sort = e.target.value; fightState.mode = 'grid'; syncActive(); drawBody(); };
  $('#fUnw').onchange = (e) => { fightState.unwatched = e.target.checked; fightState.mode = 'grid'; syncActive(); drawBody(); };
  $('#fSpoil').onchange = (e) => {
    localSet('ufcSpoilerFree', e.target.checked ? '1' : '0');
    post('/api/prefs', { ufcSpoilerFree: e.target.checked }).catch(() => {});
    toast(e.target.checked ? 'Spoiler-Free UFC mode ON' : 'Spoiler-Free UFC mode OFF');
    pageFights();
  };
  $('#fReset').onclick = () => { fightState = { mode: 'curated', sport: 'ufc', fighter: '', year: '', q: '', unwatched: false, sort: 'latest' }; pageFights(); };
  drawBody();
}

// ---------------------------------------------------------------- Release Radar & Calendar (#/radar)
let radarTab = 'all';
async function pageRadar() {
  view.innerHTML = '<div class="page-pad"><div class="page-head"><h1 class="page-title">Release Radar</h1></div><div class="empty">Scanning your followed shows, digital drops &amp; UFC schedule…</div></div>';
  const d = await api('/api/radar').catch(() => null);
  if (!d) { view.innerHTML = '<div class="page-pad"><div class="empty">Couldn’t load Release Radar.</div></div>'; return; }
  const cnt = d.counts || {};
  const fmtDays = (n) => n === 0 ? 'Today' : n === 1 ? 'Tomorrow' : n > 1 ? `In ${n} days` : n === -1 ? 'Yesterday' : `${Math.abs(n)}d ago`;
  const draw = () => {
    const show = (t) => radarTab === 'all' || radarTab === t;
    let body = '';
    if (show('episodes')) {
      const eps = d.episodes || [];
      body += `<div class="section-label">Upcoming &amp; Recent Episodes (Shows You Watch) · ${eps.length}</div>`;
      body += eps.length ? `<div class="radar-grid">${eps.map((e) => {
        const k = e.sid ? remember({ k: 's', id: e.sid, tmdb: e.tmdb, title: e.showTitle, poster: e.poster, backdrop: e.backdrop }) : '';
        return `<div class="radar-card" ${k ? `data-open="${k}" tabindex="0" role="button" style="cursor:pointer"` : ''}>
          <div class="radar-thumb">${e.poster ? `<img src="${esc(e.poster)}" alt="" loading="lazy">` : ''}</div>
          <div>
            <div class="radar-kicker"><span class="rcd-pill ${e.daysUntil <= 0 ? 'ok' : ''}">${esc(fmtDays(e.daysUntil))}</span><span class="rcd-pill muted">S${e.season}:E${e.episode}</span>${e.airDate ? `<span class="muted" style="font-size:12px">${esc(e.airDate)}</span>` : ''}</div>
            <div class="radar-title">${esc(e.showTitle)}</div>
            <div class="radar-sub">${esc(e.epName || e.status || '')}</div>
          </div></div>`;
      }).join('')}</div>` : '<div class="empty" style="padding:18px var(--gutter)">No upcoming episode air dates yet — follow any ongoing series to track it here.</div>';
    }
    if (show('ufc')) {
      const ufcs = d.ufc || [];
      body += `<div class="section-label">Upcoming UFC Schedule (Sydney Time · Spoiler-Free) · ${ufcs.length}</div>`;
      body += ufcs.length ? `<div class="radar-grid">${ufcs.map((u, idx) => `<div class="radar-card" style="grid-template-columns:1fr" tabindex="0" role="button">
        <div>
          <div class="radar-kicker">
            <span class="fbadge ${u.isPpv ? 'ppv' : ''}">${esc(u.shortName || 'UFC')}</span>
            <span class="rcd-pill ${u.daysUntil <= 1 ? 'ok' : ''}">${esc(fmtDays(u.daysUntil))}</span>
            ${u.replayAvailable ? '<span class="rcd-pill ok">Replay Ready</span>' : ''}
          </div>
          <div class="radar-title">${esc(u.name)}</div>
          ${u.mainEvent ? `<div class="fcard-match" style="margin-top:3px">${esc(u.mainEvent)}</div>` : ''}
          <div class="radar-sub">${esc([u.sydneyDate, u.mainCardSydneyTime ? `Main card ${u.mainCardSydneyTime}` : u.sydneyTime, u.venue].filter(Boolean).join(' · '))}</div>
          <div class="radar-acts">
            ${u.replayAvailable && u.replayId ? `<button class="btn btn-primary btn-sm" data-open="${remember({ k: 'm', id: u.replayId, title: u.name })}">${I.play}Watch Replay</button>` : ''}
            ${u.bouts?.length ? `<button class="btn btn-ghost btn-sm" data-radar-ufc="${idx}">🥊 Preview ${u.bouts.length} Bouts</button>` : ''}
          </div>
        </div></div>`).join('')}</div>` : '<div class="empty" style="padding:18px var(--gutter)">No upcoming UFC events returned by ESPN right now.</div>';
    }
    if (show('movies')) {
      const movs = d.movies || [];
      body += `<div class="section-label">Upcoming Theatrical &amp; Digital Drops · ${movs.length}</div>`;
      body += movs.length ? `<div class="radar-grid">${movs.map((m, idx) => {
        const stCls = m.libraryState === '4k' ? 'ok' : m.libraryState === 'cam' ? 'cam' : '';
        const k = m.inLibrary && m.id ? remember({ k: 'm', id: m.id, tmdb: m.tmdb, title: m.title, year: m.year, poster: m.poster, backdrop: m.backdrop }) : '';
        return `<div class="radar-card" ${k ? `data-open="${k}" tabindex="0" role="button" style="cursor:pointer"` : ''}>
          <div class="radar-thumb">${m.poster ? `<img src="${esc(m.poster)}" alt="" loading="lazy">` : ''}</div>
          <div>
            <div class="radar-kicker">
              <span class="rcd-pill ${stCls}">${esc(m.status || m.libraryState)}</span>
              ${m.releaseDate ? `<span class="muted" style="font-size:12px">${esc(m.releaseDate)}</span>` : ''}
            </div>
            <div class="radar-title">${esc(m.title)}${m.year ? ` <span class="muted">(${m.year})</span>` : ''}</div>
            ${m.overview ? `<div class="radar-sub" style="-webkit-line-clamp:2;display:-webkit-box;-webkit-box-orient:vertical;overflow:hidden">${esc(m.overview)}</div>` : ''}
            <div class="radar-acts">
              ${k ? `<button class="btn btn-primary btn-sm" data-open="${k}">Open</button>` : ''}
              ${m.tmdb && m.libraryState !== '4k' ? `<button class="btn btn-ghost btn-sm" data-radar-want="${idx}">${m.wanted ? '🔔 Watching' : '🔔 Notify for HD/4K'}</button>` : ''}
            </div>
          </div></div>`;
      }).join('')}</div>` : '<div class="empty" style="padding:18px var(--gutter)">No upcoming movie drops found.</div>';
    }
    if (show('upgrades')) {
      const ups = d.upgrades || [];
      body += `<div class="section-label">Recent 4K &amp; Clean HD Upgrades in Your Library · ${ups.length}</div>`;
      body += ups.length ? `<div class="grid" style="padding:0 var(--gutter)">${ups.map(poster).join('')}</div>`
        : '<div class="empty" style="padding:18px var(--gutter)">Quality baseline recorded — as your provider upgrades titles from CAM → HD or HD → 4K DV on future syncs, they’ll appear here automatically!</div>';
    }
    view.innerHTML = `<div class="page-pad"><div class="page-head">
      <h1 class="page-title">Release Radar</h1>
      <div class="chips">
        ${[['all', 'Everything', (cnt.episodes || 0) + (cnt.ufc || 0) + (cnt.movies || 0)],
           ['episodes', 'Episodes', cnt.episodes || 0],
           ['ufc', 'Upcoming UFC', cnt.ufc || 0],
           ['movies', 'Digital Drops', cnt.movies || 0],
           ['upgrades', '4K Upgrades', cnt.upgrades || 0]].map(([k, l, n]) => `<button class="chip ${radarTab === k ? 'on' : ''}" data-rtab="${k}">${l} <span class="chip-cnt">${n}</span></button>`).join('')}
      </div>
      <a class="chip" href="#/4k?tier=cam" style="margin-left:auto">CAM-Only Watch (${cnt.camWatch || 0}) →</a>
    </div>${body}</div>`;
    $$('[data-rtab]').forEach((b) => (b.onclick = () => { radarTab = b.dataset.rtab; draw(); }));
    $$('[data-radar-ufc]').forEach((b) => (b.onclick = (ev) => {
      ev.stopPropagation();
      const u = (d.ufc || [])[+b.dataset.radarUfc]; if (!u) return;
      openModal(`<div class="sheet" style="max-width:740px"><button class="close" aria-label="Close">${I.close}</button>
        <div class="sheet-section" style="padding:28px 32px">
          <span class="spoiler-bar">🛡️ Spoiler-Free Bout Lineup</span>
          <h2 style="margin:12px 0 4px">${esc(u.name)}</h2>
          <div class="muted">${esc([u.sydneyDate, u.mainCardSydneyTime ? `Main card ${u.mainCardSydneyTime}` : u.sydneyTime, u.venue].filter(Boolean).join(' · '))}</div>
          <div class="bout-list">${(u.bouts || []).map((bt) => `<div class="bout-row ${bt.isMainEvent ? 'main' : ''}">
            <div class="bout-sec">${esc(bt.section || bt.card)}</div>
            <div><div class="bout-vs">${esc(bt.fighterA)} <span class="muted">vs.</span> ${esc(bt.fighterB)}${bt.isTitle ? ' <span class="pill uhd">TITLE</span>' : ''}</div>
              ${bt.weightClass ? `<div class="bout-wc">${esc(bt.weightClass)}</div>` : ''}</div>
            <span class="bout-rds">${bt.rounds || 3} Rds</span></div>`).join('')}</div>
        </div></div>`);
    }));
    $$('[data-radar-want]').forEach((b) => (b.onclick = async (ev) => {
      ev.stopPropagation();
      const m = (d.movies || [])[+b.dataset.radarWant]; if (!m) return;
      m.wanted = !m.wanted;
      await post('/api/wanted', { mt: 'movie', tmdb: m.tmdb, on: m.wanted, title: m.title, poster: m.poster || '', year: m.year || null }).catch(() => {});
      b.textContent = m.wanted ? '🔔 Watching' : '🔔 Notify for HD/4K';
      toast(m.wanted ? `Watching ${m.title} for clean HD/4K` : `Removed ${m.title} from watch alerts`);
    }));
  };
  draw();
}

// ---------------------------------------------------------------- Marathons & Watch Order (#/marathons)
async function pageMarathons(slug, qs) {
  if (slug) {
    const order = new URLSearchParams(qs || '').get('order') === 'release' ? 'release' : 'chrono';
    const d = await api(`/api/marathons/${encodeURIComponent(slug)}?order=${order}`).catch(() => null);
    if (!d) { view.innerHTML = '<div class="page-pad"><div class="empty">Marathon not found. <a href="#/marathons" style="color:var(--accent)">Back to Marathons</a></div></div>'; return; }
    const fmtMins = (m) => { const h = Math.floor((m || 0) / 60), r = (m || 0) % 60; return h ? `${h}h ${r}m` : `${r}m`; };
    const nextKey = d.nextUp?.id ? remember(d.nextUp) : '';
    view.innerHTML = `<section class="hero" style="min-height:380px;height:min(54vh,480px)">
      <div class="hero-slide on">${d.backdrop ? `<img src="${esc(d.backdrop)}" alt="">` : ''}</div>
      <div class="hero-info">
        <div class="hero-kicker"><a href="#/marathons" style="color:var(--muted)">← All Marathons</a> · ${d.watchedCount}/${d.availableCount} Watched (${d.pctComplete}%)</div>
        <h1 class="hero-title">${esc(d.icon || '🎬')} ${esc(d.title)}</h1>
        <p class="hero-overview">${esc(d.tagline || '')}</p>
        <div class="hero-meta"><span><b>${d.availableCount}</b> of ${d.total} in your library</span><span><b>${d.uhdCount}</b> in 4K</span>${d.totalRuntimeMins ? `<span>Total: ${fmtMins(d.totalRuntimeMins)}${d.remainingRuntimeMins && d.watchedCount ? ` · ${fmtMins(d.remainingRuntimeMins)} left` : ''}</span>` : ''}</div>
        <div class="btns">
          ${nextKey ? `<button class="btn btn-primary" data-quickplay="${nextKey}">${I.play}Next: ${esc(d.nextUp.title)}</button>` : ''}
          ${d.hasDistinctOrder ? `<a class="btn ${order === 'chrono' ? 'btn-accent' : 'btn-ghost'}" href="#/marathons/${encodeURIComponent(slug)}?order=chrono">Chronological Story Order</a>
          <a class="btn ${order === 'release' ? 'btn-accent' : 'btn-ghost'}" href="#/marathons/${encodeURIComponent(slug)}?order=release">Release Order</a>` : ''}
        </div>
      </div>
    </section>
    <div class="rows" style="margin-top:20px">
      <div class="mar-steps">${(d.items || []).map((it) => {
        const isNext = d.nextUp && it.available && it.id === d.nextUp.id;
        const k = it.available ? remember(it) : '';
        return `<div class="mar-step ${it.available ? 'avail' : ''} ${isNext ? 'nextup' : ''} ${it.watched ? 'done' : ''}" ${k ? `data-open="${k}" tabindex="0" role="button"` : ''}>
          <div class="mar-num">${it.index}</div>
          <div class="mar-step-thumb">${it.poster ? `<img src="${esc(it.poster)}" alt="" loading="lazy">` : ''}</div>
          <div style="min-width:0">
            <div style="display:flex;gap:7px;align-items:center;flex-wrap:wrap">
              <b>${esc(it.title)}</b>
              ${it.year ? `<span class="muted">(${it.year})</span>` : ''}
              ${it.phase ? `<span class="rc-tag">${esc(it.phase)}</span>` : ''}
              ${isNext ? '<span class="rcd-pill">NEXT IN MARATHON</span>' : ''}
              ${it.watched ? '<span class="rcd-pill ok">✓ Watched</span>' : ''}
              ${it.available ? (it.dv ? '<span class="pill dv">4K DV</span>' : it.uhd ? '<span class="pill uhd">4K</span>' : '') : '<span class="rcd-pill muted">Not in library</span>'}
            </div>
            <div class="muted" style="font-size:12.5px;margin-top:4px">${[it.rating ? `★ ${(+it.rating).toFixed(1)}` : '', it.runtime ? runtime(it.runtime) : '', it.progress > 0.03 && !it.watched ? `${Math.round(it.progress * 100)}% watched` : ''].filter(Boolean).join(' · ')}</div>
          </div>
          <div class="btns">
            ${k ? `<button class="btn ${isNext ? 'btn-primary' : 'btn-ghost'} btn-sm" data-quickplay="${k}">${I.play}${it.k === 's' ? 'Series' : 'Play'}</button>` : ''}
          </div>
        </div>`;
      }).join('')}</div>
    </div>`;
    return;
  }

  const my = routeGen;
  const d = await apiCached('/api/marathons', 120000).catch(() => null);
  if (my !== routeGen) return;
  if (!d) { view.innerHTML = '<div class="page-pad"><div class="empty">Couldn’t load marathons.</div></div>'; return; }
  const card = (m) => `<a class="mar-card" href="#/marathons/${encodeURIComponent(m.slug)}">
    <div class="mar-hero">
      ${m.backdrop ? `<img src="${esc(m.backdrop)}" alt="" loading="lazy">` : ''}
      <div class="mar-head"><h3>${esc(m.icon || '🎬')} ${esc(m.title)}</h3><p>${esc(m.tagline || '')}</p></div>
      <div class="mar-posters">${(m.posters || []).slice(0, 4).map((p) => `<img src="${esc(p)}" alt="" loading="lazy">`).join('')}</div>
    </div>
    <div class="mar-body">
      <div class="mar-stats">
        <span><b>${m.availableCount}</b>/${m.total} available</span>
        <span><b>${m.watchedCount}</b> watched (${m.pctComplete}%)</span>
        ${m.uhdCount ? `<span><b>${m.uhdCount}</b> in 4K</span>` : ''}
        ${m.hasDistinctOrder ? '<span class="pill">Chrono + Release</span>' : ''}
      </div>
      <div class="mar-prog"><i style="width:${m.pctComplete || 0}%"></i></div>
      ${m.nextUp ? `<div class="mar-next">▶ Next up: ${esc(m.nextUp.title)}</div>` : m.completed ? '<div class="ok" style="font-size:12.5px;font-weight:700">✓ Marathon Complete</div>' : ''}
    </div>
  </a>`;
  view.innerHTML = `<div class="page-pad">
    <div class="page-head"><h1 class="page-title">Marathons &amp; Watch Order</h1><span class="muted">Chronological &amp; release binge guides matched live to your library</span></div>
    <div class="mar-grid">${(d.marathons || []).map(card).join('')}</div>
    ${(d.collections || []).length ? `<div class="section-label" style="margin-top:30px">More Franchises &amp; Collections in Your Library</div><div class="mar-grid">${d.collections.map(card).join('')}</div>` : ''}
  </div>`;
}

// ---------------------------------------------------------------- Personal Cinema DNA (#/dna)
async function pageDna() {
  view.innerHTML = '<div class="page-pad"><div class="page-head"><h1 class="page-title">Cinema DNA</h1></div><div class="empty">Analyzing your watch history, ratings &amp; filmmakers…</div></div>';
  const d = await api('/api/dna').catch(() => null);
  if (!d) { view.innerHTML = '<div class="page-pad"><div class="empty">Couldn’t load Cinema DNA.</div></div>'; return; }
  const maxG = Math.max(1, ...(d.topGenres || []).map((x) => x.pct || x.count || 1));
  const maxDec = Math.max(1, ...(d.topDecades || []).map((x) => x.pct || x.count || 1));
  view.innerHTML = `<div class="page-pad">
    <div class="dna-hero">
      <div>
        <span class="dna-tag">🧬 Your Cinema DNA</span>
        <h1>${esc(d.persona?.tag || 'Cinema Explorer')}</h1>
        <p>${esc(d.persona?.summary || 'Watch a few movies or episodes to build your taste profile.')}</p>
        <div class="btns" style="margin-top:16px">
          <a class="btn btn-accent btn-sm" href="#/specials">🎤 Stand-Up, Concerts &amp; Prestige →</a>
          <a class="btn btn-ghost btn-sm" href="#/4k">✨ 4K OLED Showcase</a>
          <a class="btn btn-ghost btn-sm" href="#/marathons">🎬 Marathons</a>
        </div>
      </div>
      <div class="dna-kpis">
        <div class="dna-kpi"><b>${(d.totalHours || 0).toLocaleString()}h</b><span>Watch Time</span></div>
        <div class="dna-kpi"><b>${(d.moviesWatched || 0).toLocaleString()}</b><span>Films Watched</span></div>
        <div class="dna-kpi"><b>${(d.episodesWatched || 0).toLocaleString()}</b><span>Episodes Watched</span></div>
        <div class="dna-kpi"><b>${d.avgRating ? `★ ${d.avgRating}` : '—'}</b><span>Avg Score</span></div>
        <div class="dna-kpi"><b>${d.uhdSharePct || 0}%</b><span>Watched in 4K</span></div>
        <div class="dna-kpi"><b>${esc(d.runtimeSweetSpot || '—')}</b><span>Film Sweet Spot</span></div>
      </div>
    </div>
    <div class="dna-panels">
      <div class="dna-panel"><h3>Top Genres</h3>${(d.topGenres || []).length ? d.topGenres.map((g) => `<div class="dna-bar-row"><span>${esc(g.name)}</span><div class="dna-bar-track"><i style="width:${Math.round((g.pct || g.count) * 100 / maxG)}%"></i></div><span class="muted">${g.pct}%</span></div>`).join('') : '<p class="muted">Watch a few titles to see your genre breakdown.</p>'}</div>
      <div class="dna-panel"><h3>Favorite Decades &amp; Platforms</h3>
        ${(d.topDecades || []).map((x) => `<div class="dna-bar-row"><span>${esc(x.decade)}</span><div class="dna-bar-track"><i style="width:${Math.round((x.pct || x.count) * 100 / maxDec)}%"></i></div><span class="muted">${x.pct}%</span></div>`).join('')}
        ${(d.topPlatforms || []).length ? `<div class="chips" style="margin-top:14px">${d.topPlatforms.map((p) => `<a class="chip sm" href="#/browse?platform=${encodeURIComponent(p.name.toLowerCase().replace(/[^a-z]/g, ''))}">${esc(p.name)} <span class="chip-cnt">${p.count}</span></a>`).join('')}</div>` : ''}
      </div>
      ${(d.topDirectors || []).length || (d.topActors || []).length ? `<div class="dna-panel">
        ${(d.topDirectors || []).length ? `<h3>Favorite Directors</h3><div class="dna-people" style="margin-bottom:16px">${d.topDirectors.slice(0, 8).map((p) => `<a class="dna-person" href="#/person/${p.id}">${p.photo ? `<img src="${esc(p.photo)}" alt="">` : ''}<span>${esc(p.name)} <small>(${p.count})</small></span></a>`).join('')}</div>` : ''}
        ${(d.topActors || []).length ? `<h3>Most-Watched Actors</h3><div class="dna-people">${d.topActors.slice(0, 10).map((p) => `<a class="dna-person" href="#/person/${p.id}">${p.photo ? `<img src="${esc(p.photo)}" alt="">` : ''}<span>${esc(p.name)} <small>(${p.count})</small></span></a>`).join('')}</div>` : ''}
      </div>` : ''}
    </div>
    <div class="rows">${(d.becauseYouLoved || []).map(renderRow).join('')}</div>
  </div>`;
  wireRows();
}

// ---------------------------------------------------------------- Stand-Up, Concerts, World Cinema & Prestige (#/specials)
let specState = { mode: 'curated', bucket: 'standup', q: '', uhd: false, unwatched: false, sort: '', page: 0 };
async function pageSpecials(qs) {
  if (qs) { const b = new URLSearchParams(qs).get('bucket'); if (b) { specState.mode = 'grid'; specState.bucket = b; } }
  const my = routeGen;
  const d = await apiCached('/api/specials', 120000).catch(() => null);
  if (my !== routeGen) return;
  if (!d) { view.innerHTML = '<div class="page-pad"><div class="empty">Couldn’t load specialty hubs.</div></div>'; return; }
  const c = d.counts || {};
  const buckets = [
    ['standup', '🎤 Stand-Up Comedy', c.standup], ['concerts', '🎸 Concerts & Music', c.concerts],
    ['world', '🌍 World Cinema', c.worldCinema], ['apple', 'Apple TV+ Prestige', c.apple],
    ['hbo', 'HBO Prestige', c.hbo], ['imdb', 'IMDb Top 250', c.imdbTop], ['short', '⏱ Under 95 Min', c.shortGems],
  ];
  const spSorts = [['', 'Curated'], ['rating', 'Highest rated'], ['newest', 'Newest'], ['added', 'Recently added'], ['az', 'A–Z']];
  view.innerHTML = `<div class="page-pad"><div class="page-head"><h1 class="page-title">Stand-Up, Concerts &amp; Prestige</h1>${viewToggleHTML(getViewMode())}</div>
    <div class="anime-bar" style="margin-bottom:18px">
      <div class="anime-bar-row">
        <div class="chips">
          <button class="chip ${specState.mode === 'curated' ? 'on' : ''}" data-spmode="curated">All Shelves</button>
          ${buckets.map(([k, l, n]) => `<button class="chip ${specState.mode === 'grid' && specState.bucket === k ? 'on' : ''}" data-spb="${k}">${l} <span class="chip-cnt">${(n || 0).toLocaleString()}</span></button>`).join('')}
        </div>
        <input type="search" class="explore-search" id="spQuery" placeholder="Search comedian, artist, language (korean, french)…" value="${esc(specState.q)}">
      </div>
      <div class="anime-bar-row">
        ${sel('spSort', spSorts, specState.sort)}
        <label class="tog"><input type="checkbox" id="spUhd" ${specState.uhd ? 'checked' : ''}> 4K only</label>
        <label class="tog"><input type="checkbox" id="spUnw" ${specState.unwatched ? 'checked' : ''}> Unwatched</label>
        <button class="btn btn-ghost btn-sm" id="spReset">Reset</button>
      </div>
    </div>
    <div id="spBody"></div><div id="spMore" class="more-sentinel"></div></div>`;

  let page = 0; let loading = false; let done = false; let reqId = 0; let curItems = [];
  const loadGrid = async (pg, reset) => {
    if (loading && !reset) return;
    loading = true; const my = ++reqId;
    const body = $('#spBody'); if (!body) return;
    if (reset) { page = 0; done = false; curItems = []; body.innerHTML = '<div class="browse-meta" id="spMeta">Loading…</div><div id="spItems" class="grid"></div>'; }
    const q = new URLSearchParams({ bucket: specState.bucket, page: String(pg), limit: '60' });
    if (specState.q.trim()) q.set('q', specState.q.trim());
    if (specState.sort) q.set('sort', specState.sort);
    if (specState.uhd) q.set('uhd', '1');
    if (specState.unwatched) q.set('unwatched', '1');
    try {
      const r = await api(`/api/specials/browse?${q}`);
      if (my !== reqId || specState.mode !== 'grid') return;
      const items = r.items || [];
      curItems = reset ? items : curItems.concat(items);
      if ($('#spMeta')) $('#spMeta').innerHTML = `<div class="browse-stats"><b>${(r.total || 0).toLocaleString()}</b> titles</div>`;
      const host = $('#spItems'); const mode = getViewMode();
      if (host) {
        if (reset) { host.className = mode === 'list' ? 'rich-list' : 'grid'; host.innerHTML = items.length ? items.map(mode === 'list' ? richCard : poster).join('') : '<div class="empty" style="grid-column:1/-1">Nothing matches those filters.</div>'; }
        else host.insertAdjacentHTML('beforeend', items.map(mode === 'list' ? richCard : poster).join(''));
      }
      page = pg; done = !r.more;
    } finally { if (my === reqId) loading = false; }
  };
  const sync = () => {
    $$('[data-spmode]').forEach((b) => b.classList.toggle('on', specState.mode === 'curated'));
    $$('[data-spb]').forEach((b) => b.classList.toggle('on', specState.mode === 'grid' && b.dataset.spb === specState.bucket));
  };
  const drawBody = () => {
    const body = $('#spBody'); if (!body) return;
    if (specState.mode === 'curated') { body.innerHTML = `<div class="rows">${(d.rows || []).map(renderRow).join('')}</div>`; wireRows(); }
    else loadGrid(0, true);
  };
  const io = trackObserver(new IntersectionObserver((es) => { if (es[0]?.isIntersecting && specState.mode === 'grid' && !done && !loading) loadGrid(page + 1, false); }, { rootMargin: '600px' }));
  const sen = $('#spMore'); if (sen) io.observe(sen);
  $('[data-spmode="curated"]').onclick = () => { specState.mode = 'curated'; sync(); drawBody(); };
  $$('[data-spb]').forEach((b) => (b.onclick = () => { specState.mode = 'grid'; specState.bucket = b.dataset.spb; sync(); drawBody(); }));
  $('#spQuery').oninput = debounce((e) => { specState.q = e.target.value; specState.mode = 'grid'; sync(); drawBody(); }, 220);
  $('#spSort').onchange = (e) => { specState.sort = e.target.value; specState.mode = 'grid'; sync(); drawBody(); };
  $('#spUhd').onchange = (e) => { specState.uhd = e.target.checked; specState.mode = 'grid'; sync(); drawBody(); };
  $('#spUnw').onchange = (e) => { specState.unwatched = e.target.checked; specState.mode = 'grid'; sync(); drawBody(); };
  $('#spReset').onclick = () => { specState = { mode: 'curated', bucket: 'standup', q: '', uhd: false, unwatched: false, sort: '', page: 0 }; pageSpecials(); };
  $$('[data-vmode]', view).forEach((b) => (b.onclick = () => {
    setViewMode(b.dataset.vmode);
    $$('[data-vmode]', view).forEach((x) => x.classList.toggle('on', x === b));
    if (specState.mode === 'grid' && curItems.length) {
      const host = $('#spItems'); const mode = getViewMode();
      if (host) { host.className = mode === 'list' ? 'rich-list' : 'grid'; host.innerHTML = curItems.map(mode === 'list' ? richCard : poster).join(''); }
    } else { specState.mode = 'grid'; sync(); drawBody(); }
  }));
  drawBody();
}

let sportsLeague = 'All';
async function pageSports() {
  const my = routeGen;
  const d = await apiCached('/api/sports', 45000);
  if (my !== routeGen) return;
  const draw = () => {
    const fx = d.fixtures.filter((f) => sportsLeague === 'All' || f.sport === sportsLeague || f.leagueName === sportsLeague);
    const ev = d.events.filter((e) => sportsLeague === 'All' || e.league === sportsLeague);
    const live = ev.filter((e) => e.status === 'live');
    const up = ev.filter((e) => e.status === 'upcoming');
    const listed = ev.filter((e) => e.status === 'listed');
    const count = (l) => d.fixtures.filter((f) => f.sport === l).length * 3 + d.events.filter((e) => e.league === l).length;
    const leagues = ['All', ...d.leagues.sort((a, b) => count(b) - count(a))];
    let html = `<div class="page-pad"><div class="page-head"><h1 class="page-title">Sports</h1>
      <div class="chips">${leagues.map((l) => `<button class="chip ${l === sportsLeague ? 'on' : ''}" data-league="${esc(l)}">${esc(l)}</button>`).join('')}</div>
      <div class="chips" style="margin-left:auto"><a class="chip on" href="#/fights">🥊 UFC &amp; Sports Replays →</a>
      <button class="chip ${spoilerFree() ? 'on' : ''}" id="spoilBtn" title="Hide scores and results">${spoilerFree() ? 'Scores hidden' : 'Hide scores'}</button>
      <button class="chip" id="teamsBtn">★ My teams${d.teams.length ? ` (${d.teams.length})` : ''}</button></div></div>`;
    html += fixtureRows(fx);
    if (live.length || up.length) html += `<div class="section-label">More events from your channels</div>`;
    if (live.length) html += row({ id: 'live', title: 'Also Live', sub: `${live.length} event${live.length > 1 ? 's' : ''}` }, live.map(eventCard).join(''));
    const byDay = new Map(); for (const e of up) { const l = dayLabel(e.start); if (!byDay.has(l)) byDay.set(l, []); byDay.get(l).push(e); }
    for (const [day, list] of byDay) html += row({ id: 'd' + day, title: `${day} · other events`, sub: `${list.length}` }, list.slice(0, 60).map(eventCard).join(''));
    if (listed.length) html += row({ id: 'listed', title: 'Event Channels', sub: 'No start time published' }, listed.slice(0, 60).map(eventCard).join(''));
    if (!fx.length && !ev.length) html += `<div class="empty">No ${esc(sportsLeague)} games listed right now.</div>`;
    if (sportsLeague === 'All') {
      const onNow = d.networks.filter((n) => n.now);
      if (onNow.length) html += row({ id: 'nets', title: 'On Sports Channels Now' }, onNow.map(channelWide).join(''));
      if (d.replays.length) html += row({ id: 'rep', title: 'Replays & Fight Nights' }, d.replays.map(poster).join(''));
    }
    view.innerHTML = html + '</div>';
    $$('[data-league]').forEach((b) => (b.onclick = () => { sportsLeague = b.dataset.league; draw(); }));
    $('#spoilBtn').onclick = () => { localSet('spoilers', spoilerFree() ? '0' : '1'); draw(); };
    $('#teamsBtn').onclick = () => openTeams(() => pageSports());
    wireRows();
  };
  draw();
}

// Pick favourite teams per league (ESPN team lists).
async function openTeams(onDone) {
  const leagues = await api('/api/leagues');
  let league = localGet('teamsLeague', 'nfl'); if (!leagues.some((l) => l.id === league)) league = leagues[0].id;
  openModal(`<div class="sheet settings teams-sheet"><button class="close" aria-label="Close">${I.close}</button>
    <h2>My teams</h2><p>Star teams to pin their games to the top of Home and Sports, and get an alert 10 minutes before kickoff.</p>
    <div class="chips" style="margin:14px 0">${leagues.map((l) => `<button class="chip ${l.id === league ? 'on' : ''}" data-tl="${l.id}">${esc(l.name)}</button>`).join('')}</div>
    <input class="team-filter" id="teamFilter" placeholder="Filter teams"><div class="team-grid" id="teamGrid"><div class="spinner" style="position:static;margin:30px auto"></div></div></div>`);
  let teams = [];
  const draw = () => {
    const f = ($('#teamFilter')?.value || '').toLowerCase();
    $('#teamGrid').innerHTML = teams.filter((t) => !f || t.name.toLowerCase().includes(f)).map((t, i) => `<button class="team ${t.fav ? 'on' : ''}" data-ti="${teams.indexOf(t)}">
      ${t.logo ? `<img src="${esc(t.logo)}" alt="" loading="lazy">` : ''}<span>${esc(t.name)}</span><i>${t.fav ? I.star : I.starO}</i></button>`).join('') || '<div class="empty">No teams found.</div>';
    $$('[data-ti]', modal).forEach((b) => (b.onclick = async () => {
      const t = teams[+b.dataset.ti]; t.fav = !t.fav;
      await post('/api/teams', { team: t, on: t.fav }); invalidateCache('/api/sports', '/api/home'); draw();
      toast(t.fav ? `${t.short || t.name} added to your teams` : `${t.short || t.name} removed`);
    }));
  };
  const load = async () => { teams = await api(`/api/teams/${league}`); draw(); };
  $$('[data-tl]', modal).forEach((b) => (b.onclick = () => { league = b.dataset.tl; localSet('teamsLeague', league); $$('[data-tl]', modal).forEach((x) => x.classList.toggle('on', x === b)); load(); }));
  $('#teamFilter').oninput = draw;
  $$('.close', modal).forEach((b) => (b.onclick = () => { closeModal(); onDone?.(); }));
  load();
}

let liveState = { group: null, filter: '', region: 'AU' };
async function pageLive() {
  const my = routeGen;
  const { groups, favs, recent = [] } = await apiCached('/api/live/groups', 120000);
  if (my !== routeGen) return;
  const regions = ['AU', 'UK', 'US', 'CA', '4K', 'Sports'];
  const inRegion = (g) => (liveState.region === 'Sports' ? g.sport : liveState.region === '4K' ? g.country === '4K' || /4K|3840|UHD/i.test(g.name) : g.country === liveState.region || (liveState.region === 'UK' && g.country === 'IE'));
  const biggest = () => groups.filter(inRegion).sort((a, b) => b.count - a.count)[0]?.id;
  if (!liveState.group) liveState.group = favs.length ? 'favs' : recent.length ? 'recent' : biggest();
  view.innerHTML = `<div class="page-pad"><div class="page-head"><h1 class="page-title">Live TV</h1>
    <div class="chips"><a class="chip on" href="#/live">Channels</a><a class="chip" href="#/guide">Guide</a></div>
    <div class="chips">${regions.map((r) => `<button class="chip ${r === liveState.region ? 'on' : ''}" data-region="${r}">${r}</button>`).join('')}</div></div>
    <div class="live-layout"><aside class="groups"><input id="groupFilter" placeholder="Filter groups" value="${esc(liveState.filter)}"><div class="group-list" id="groupList"></div></aside>
    <section class="channels" id="channelList"></section></div></div>`;
  let chGen = 0; let chRaf = 0;
  onRouteCleanup(() => { chGen++; cancelAnimationFrame(chRaf); });
  const drawGroups = () => {
    const f = liveState.filter.toLowerCase();
    const list = groups.filter((g) => inRegion(g) && (!f || g.name.toLowerCase().includes(f)));
    $('#groupList').innerHTML = `<button class="group ${liveState.group === 'favs' ? 'on' : ''}" data-group="favs"><span>★ Favourites</span><small>${favs.length}</small></button>` +
      (recent.length ? `<button class="group ${liveState.group === 'recent' ? 'on' : ''}" data-group="recent"><span>🕘 Recent</span><small>${recent.length}</small></button>` : '') +
      list.map((g) => `<button class="group ${g.id === liveState.group ? 'on' : ''}" data-group="${esc(g.id)}"><span>${esc(g.name)}</span><small>${g.count}</small></button>`).join('');
    $$('[data-group]').forEach((b) => (b.onclick = () => { liveState.group = b.dataset.group; drawGroups(); drawChannels(); }));
  };
  const drawChannels = async () => {
    const myCh = ++chGen;
    cancelAnimationFrame(chRaf);
    const el = $('#channelList'); if (!el) return;
    el.innerHTML = '<div class="loading" style="min-height:200px"><div class="spinner"></div></div>';
    const chs = await api(`/api/live/channels?cat=${encodeURIComponent(liveState.group || '')}`, { signal: routeAbort?.signal }).catch((e) => { if (isAbort(e)) return null; throw e; });
    if (!chs || myCh !== chGen || my !== routeGen || !el.isConnected) return;
    CHANNEL_CONTEXT = chs;
    const strip = recent.length && liveState.group !== 'recent'
      ? `<div class="recent-strip">${recent.slice(0, 8).map((c) => { const k = remember({ k: 'l', id: c.id, title: c.title, logo: c.logo, epg: c.epg }); return `<button class="recent-pill" data-live="${k}" title="${esc(c.now ? `${c.title} · ${c.now.title}` : c.title)}">${c.logo ? `<img src="${esc(c.logo)}" alt="" loading="lazy" onerror="this.remove()">` : ''}<span>${esc(c.title)}</span></button>`; }).join('')}</div>`
      : '';
    if (!chs.length) {
      el.innerHTML = strip + `<div class="empty">${liveState.group === 'favs' ? 'Star channels to pin them here.' : liveState.group === 'recent' ? 'Channels you watch will appear here.' : 'No channels in this group.'}</div>`;
      return;
    }
    if (chs.length <= 150) {
      el.innerHTML = strip + chs.map(channelRow).join('');
      return;
    }
    // Progressive chunked render for large channel groups (>150 rows) to prevent UI jank
    const FIRST = 120; const CHUNK = 100;
    el.innerHTML = strip + chs.slice(0, FIRST).map(channelRow).join('');
    let idx = FIRST;
    const pump = () => {
      if (myCh !== chGen || my !== routeGen || !el.isConnected) return;
      const end = Math.min(chs.length, idx + CHUNK);
      el.insertAdjacentHTML('beforeend', chs.slice(idx, end).map(channelRow).join(''));
      idx = end;
      if (idx < chs.length) chRaf = requestAnimationFrame(pump);
    };
    chRaf = requestAnimationFrame(pump);
  };
  $('#groupFilter').oninput = debounce((e) => { liveState.filter = e.target.value; drawGroups(); }, 120);
  $$('[data-region]').forEach((b) => (b.onclick = () => { liveState.region = b.dataset.region; liveState.group = biggest(); pageLive(); }));
  drawGroups(); drawChannels();
}
let CHANNEL_CONTEXT = [];

async function pageMyList() {
  const d = await api('/api/mylist');
  let html = `<div class="page-pad"><div class="page-head"><h1 class="page-title">My List</h1></div>`;
  if (d.continue.length) html += renderRow({ id: 'continue', title: 'Continue Watching', items: d.continue, style: 'continue' });
  html += d.list.length ? `<div class="grid">${d.list.map(poster).join('')}</div>` : '<div class="empty">Add movies and series with + and they will show up here.</div>';
  if (d.favs.length) html += `<div class="section-label">Favourite channels</div><div class="channels" style="padding:0 var(--gutter)">${d.favs.map((c) => channelRow({ ...c, fav: true })).join('')}</div>`;
  view.innerHTML = html + '</div>'; wireRows();
}

let searchState = { all: false, tab: 'all', sort: 'relevance', rmin: '', decade: '', platform: '', uhd: false };
async function pageSearch(q) {
  if (!q) {
    view.innerHTML = `<div class="page-pad"><div class="empty">Search titles, topics, plot keywords, actors, directors, documentaries, anime, channels and teams.<br>
      <div style="margin-top:14px;display:flex;gap:8px;justify-content:center;flex-wrap:wrap">
        <a class="chip sm" href="#/search?q=true%20crime%20documentary">Try: “true crime documentary”</a>
        <a class="chip sm" href="#/search?q=nature%204k">Try: “nature 4k”</a>
        <a class="chip sm" href="#/search?q=best%2090s%20thrillers">Try: “best 90s thrillers”</a>
        <a class="chip sm" href="#/search?q=netflix%20miniseries">Try: “netflix miniseries”</a>
      </div></div></div>`;
    return;
  }
  const sq = new URLSearchParams({ q });
  if (searchState.all) sq.set('all', '1');
  if (searchState.sort && searchState.sort !== 'relevance') sq.set('sort', searchState.sort);
  if (searchState.rmin) sq.set('rmin', searchState.rmin);
  if (searchState.decade) { const [a, b] = searchState.decade.split('-'); if (+a) sq.set('y1', a); if (+b < 9999) sq.set('y2', b); }
  if (searchState.platform) sq.set('platform', searchState.platform);
  if (searchState.uhd) sq.set('uhd', '1');
  const d = await api(`/api/search?${sq}`);
  const movies = d.movies || [];
  const series = d.series || [];
  const docs = d.docs || [];
  const anime = d.anime || [];
  const topicHits = d.topicHits || [];
  const people = d.people || [];
  const live = d.live || [];
  const missing = d.missing || [];
  const smart = d.smart?.items || [];
  const counts = d.counts || {
    movies: movies.length, series: series.length, docs: docs.length, anime: anime.length,
    topicHits: topicHits.length, people: people.length, live: live.length, missing: missing.length,
    total: movies.length + series.length + live.length + smart.length + people.length + missing.length + topicHits.length,
  };
  const tabs = [
    ['all', 'All', counts.total + smart.length],
    ['movies', 'Movies', movies.length],
    ['series', 'Series', series.length],
    ['docs', 'Documentaries', docs.length],
    ['anime', 'Anime', anime.length],
    ['topics', 'Topic & Plot', topicHits.length],
    ['people', 'People', people.length],
    ['live', 'Live TV', live.length],
    ['missing', 'Not in Library', missing.length],
  ].filter(([id, , n]) => id === 'all' || n > 0);
  if (!tabs.some(([id]) => id === searchState.tab)) searchState.tab = 'all';

  const drawSearch = () => {
    const mode = getViewMode();
    const t = searchState.tab;
    let html = `<div class="page-pad"><div class="page-head"><h1 class="page-title">“${esc(q)}”</h1>
      <div class="chips"><button class="chip ${searchState.all ? '' : 'on'}" data-scope="0">English &amp; Sports</button><button class="chip ${searchState.all ? 'on' : ''}" data-scope="1">All regions</button></div>
      ${viewToggleHTML(mode)}</div>
      <div class="search-tabs">${tabs.map(([id, lab, n]) => `<button class="chip sm ${t === id ? 'on' : ''}" data-stab="${id}">${esc(lab)}<span class="chip-cnt">${n}</span></button>`).join('')}</div>
      <div class="search-bar">
        ${sel('sSort', [['relevance', 'Best match'], ['rating', 'Highest rated'], ['newest', 'Newest'], ['popular', 'Most popular']], searchState.sort)}
        ${sel('sPlat', PLATFORMS_ALL, searchState.platform)}
        ${sel('sDecade', DECADES, searchState.decade)}
        ${sel('sRating', RATINGS_DEEP, searchState.rmin)}
        <label class="tog"><input type="checkbox" id="sUhd" ${searchState.uhd ? 'checked' : ''}> 4K only</label>
        ${(searchState.sort !== 'relevance' || searchState.rmin || searchState.decade || searchState.platform || searchState.uhd) ? '<button class="btn btn-ghost btn-sm" id="sReset">Clear filters</button>' : ''}
      </div>`;
    const totalShown = movies.length + series.length + docs.length + anime.length + topicHits.length + live.length + smart.length + people.length + missing.length;
    if (!totalShown) html += '<div class="empty">No matches. Try fewer words, clear filters, or switch to All regions.</div>';
    if (t === 'all' && smart.length) html += `<div class="section-label">Smart results · ${esc(d.smart.label)}</div>${renderCards(smart, mode)}`;
    if ((t === 'all' || t === 'people') && people.length) html += `<div class="section-label">People (${people.length})</div><div class="people">${people.map((p) => `<a class="person" href="#/person/${p.id}"><img src="${esc(p.photo)}" alt="" loading="lazy"><b>${esc(p.name)}</b><small>${esc(p.dept || '')}</small></a>`).join('')}</div>`;
    if (t === 'all' && d.fuzzy) html += '<div class="section-label dim-note">Showing close matches for your spelling</div>';
    if ((t === 'all' || t === 'docs') && docs.length) html += `<div class="section-label">Documentaries (${docs.length})</div>${renderCards(t === 'all' ? docs.slice(0, 18) : docs, mode)}`;
    if ((t === 'all' || t === 'anime') && anime.length) html += `<div class="section-label">Anime (${anime.length})</div>${renderCards(t === 'all' ? anime.slice(0, 18) : anime, mode)}`;
    if ((t === 'all' || t === 'series') && series.length) html += `<div class="section-label">Series (${series.length})</div>${renderCards(series, mode)}`;
    if ((t === 'all' || t === 'movies') && movies.length) html += `<div class="section-label">Movies (${movies.length})</div>${renderCards(movies, mode)}`;
    if ((t === 'all' || t === 'topics') && topicHits.length) html += `<div class="section-label">Matched in synopsis, genre or topic (${topicHits.length})</div>${renderCards(topicHits, mode)}`;
    if ((t === 'all' || t === 'missing') && missing.length) html += `<div class="section-label">Not in your library · get alerted when they land</div>${renderCards(missing, mode)}`;
    if ((t === 'all' || t === 'live') && live.length) { CHANNEL_CONTEXT = live; html += `<div class="section-label">Live TV &amp; Guide Matches (${live.length})</div><div class="channels" style="padding:0 var(--gutter)">${live.slice(0, 50).map(channelRow).join('')}</div>`; }
    view.innerHTML = html + '</div>';
    $$('[data-scope]').forEach((b) => (b.onclick = () => { searchState.all = b.dataset.scope === '1'; pageSearch(q); }));
    $$('[data-stab]').forEach((b) => (b.onclick = () => { searchState.tab = b.dataset.stab; drawSearch(); }));
    $$('[data-vmode]', view).forEach((b) => (b.onclick = () => { setViewMode(b.dataset.vmode); drawSearch(); }));
    $('#sSort').onchange = (e) => { searchState.sort = e.target.value; pageSearch(q); };
    $('#sPlat').onchange = (e) => { searchState.platform = e.target.value; pageSearch(q); };
    $('#sDecade').onchange = (e) => { searchState.decade = e.target.value; pageSearch(q); };
    $('#sRating').onchange = (e) => { searchState.rmin = e.target.value; pageSearch(q); };
    $('#sUhd').onchange = (e) => { searchState.uhd = e.target.checked; pageSearch(q); };
    if ($('#sReset')) $('#sReset').onclick = () => { searchState = { ...searchState, sort: 'relevance', rmin: '', decade: '', platform: '', uhd: false }; pageSearch(q); };
  };
  drawSearch();
}

// ---------------------------------------------------------------- browse (deep multi-facet explorer, sort, infinite grid / data view)
let GENRES = null;
const DECADES = [['', 'Any year'], ['2020-2029', '2020s'], ['2010-2019', '2010s'], ['2000-2009', '2000s'], ['1990-1999', '1990s'], ['1980-1989', '1980s'], ['1970-1979', '1970s'], ['0-1969', 'Before 1970']];
const RATINGS = [['', 'Any rating'], ['6', '6+'], ['7', '7+'], ['7.5', '7.5+'], ['8', '8+']];
const RATINGS_DEEP = [['', 'Any rating'], ['6', '6.0+'], ['6.5', '6.5+'], ['7', '7.0+'], ['7.5', '7.5+'], ['8', '8.0+'], ['8.5', '8.5+']];
const RUNTIMES = [['', 'Any length'], ['0-60', 'Under 60m'], ['0-90', 'Under 90m'], ['90-120', '90m–2h'], ['0-120', 'Under 2h'], ['120-150', '2h–2.5h'], ['150-999', 'Over 2.5h']];
const BROWSE_LANGS = [['', 'Any language'], ['en', 'English'], ['nonen', 'Not English'], ['ko', 'Korean'], ['ja', 'Japanese'], ['fr', 'French'], ['es', 'Spanish'], ['de', 'German'], ['it', 'Italian'], ['hi', 'Hindi'], ['nordic', 'Nordic']];
const PLATFORMS_ALL = [['', 'All platforms'], ['netflix', 'Netflix'], ['apple', 'Apple TV+'], ['disney', 'Disney+'], ['amazon', 'Amazon'], ['hbo', 'HBO'], ['discovery', 'Discovery+'], ['paramount', 'Paramount+'], ['peacock', 'Peacock'], ['showtime', 'Showtime'], ['sky', 'Sky'], ['crunchyroll', 'Crunchyroll'], ['marvel', 'Marvel']];
const COUNTRIES_ALL = [['', 'Any country'], ['US', 'USA'], ['GB', 'UK'], ['AU', 'Australia'], ['CA', 'Canada'], ['JP', 'Japan'], ['KR', 'South Korea'], ['FR', 'France'], ['DE', 'Germany'], ['ES', 'Spain'], ['IT', 'Italy'], ['IN', 'India'], ['NZ', 'New Zealand'], ['SE', 'Sweden']];
const VOTES_TIERS = [['', 'Any popularity'], ['0-2000', 'Hidden gems (<2k votes)'], ['300-0', 'Well-known (300+)'], ['1500-0', 'Major hits (1.5k+)'], ['5000-0', 'Classics (5k+)']];
const SERIES_STATUSES = [['', 'Any status'], ['miniseries', 'Limited / Miniseries'], ['ended', 'Ended'], ['returning', 'Ongoing']];
const SORTS = [['popular', 'Most popular'], ['rating', 'Highest rated'], ['newest', 'Newest'], ['added', 'Recently added'], ['az', 'A–Z']];
const SORTS_DEEP = [['popular', 'Most popular'], ['rating', 'Highest rated'], ['votes', 'Most voted'], ['newest', 'Newest'], ['oldest', 'Oldest'], ['runtime_asc', 'Shortest'], ['runtime_desc', 'Longest'], ['added', 'Recently added'], ['az', 'A–Z']];
const sel = (id, opts, v) => `<select class="sel" id="${id}">${opts.map(([k, l]) => `<option value="${k}" ${String(v ?? '') === k ? 'selected' : ''}>${l}</option>`).join('')}</select>`;

async function pageBrowseAll(qs) {
  GENRES ||= await api('/api/genres');
  const p = Object.fromEntries(new URLSearchParams(qs || ''));
  p.kind = ['movie', 'tv', 'all', 'docs', 'anime'].includes(p.kind) ? p.kind : 'movie';
  p.sort ||= 'popular';
  const gsel = new Set((p.g || '').split(',').filter(Boolean));
  const tsel = new Set((p.topic || '').split(',').filter(Boolean));
  const saved = (await api('/api/settings')).savedRows || [];
  const decade = p.y1 || p.y2 ? `${p.y1 || 0}-${p.y2 || 9999}` : '';
  const rt = p.rtmax || p.rtmin ? `${p.rtmin || 0}-${p.rtmax || 999}` : '';
  const votesTier = p.vmin || p.vmax ? `${p.vmin || 0}-${p.vmax || 0}` : '';
  const genreMap = p.kind === 'all' ? (GENRES.all || GENRES.movie) : p.kind === 'tv' ? GENRES.tv : p.kind === 'anime' ? GENRES.tv : GENRES.movie;
  const docTopics = GENRES.docTopics || [];
  const vmode = getViewMode();
  const isDocs = p.kind === 'docs';
  const chipHtml = isDocs
    ? docTopics.map((t) => `<button class="chip sm ${tsel.has(t.id) ? 'on' : ''}" data-bt="${esc(t.id)}">${esc(t.label)}<span class="chip-cnt" data-fc="${esc(t.id)}"></span></button>`).join('')
    : Object.entries(genreMap || {}).map(([id, n]) => `<button class="chip sm ${gsel.has(id) ? 'on' : ''}" data-bg="${id}">${esc(n)}<span class="chip-cnt" data-fc="${id}"></span></button>`).join('');

  view.innerHTML = `<div class="page-pad"><div class="page-head"><h1 class="page-title">Browse &amp; Explore</h1>
      <div class="chips">
        ${[['all', 'All'], ['movie', 'Movies'], ['tv', 'Series'], ['docs', 'Documentaries'], ['anime', 'Anime']].map(([k, l]) => `<button class="chip ${p.kind === k ? 'on' : ''}" data-bk="${k}">${l}</button>`).join('')}
      </div>
      <input type="search" class="explore-search" id="fQuery" placeholder="Filter by title, plot, studio, topic…" value="${esc(p.q || '')}">
      <button class="btn btn-ghost btn-sm" data-surprise="${p.kind}" style="margin-left:auto">🎲 Pick for me</button>
      ${viewToggleHTML(vmode)}</div>
    <div class="filters">
      <div class="chips genre-chips">${chipHtml}
        ${!isDocs ? `<button class="chip sm ${p.gmode === 'all' ? 'on' : ''}" id="fGmode" title="Require titles to match ALL selected genres instead of ANY">Match: ${p.gmode === 'all' ? 'ALL genres' : 'ANY genre'}</button>` : ''}
      </div>
      <div class="filter-row">
        ${sel('fPlat', PLATFORMS_ALL, p.platform || '')}
        ${sel('fCountry', COUNTRIES_ALL, p.country || '')}
        ${sel('fDecade', DECADES, DECADES.some(([k]) => k === decade) ? decade : '')}
        ${sel('fRating', RATINGS_DEEP, p.rmin || '')}
        ${!isDocs && p.kind !== 'anime' ? sel('fVotes', VOTES_TIERS, VOTES_TIERS.some(([k]) => k === votesTier) ? votesTier : '') : ''}
        ${p.kind !== 'tv' && p.kind !== 'anime' ? sel('fRuntime', RUNTIMES, RUNTIMES.some(([k]) => k === rt) ? rt : '') : ''}
        ${(p.kind === 'tv' || p.kind === 'all') ? sel('fStatus', SERIES_STATUSES, p.status || '') : ''}
        ${p.kind !== 'anime' ? sel('fLang', BROWSE_LANGS, p.lang || '') : ''}
        ${sel('fSort', SORTS_DEEP, p.sort)}
        <label class="tog"><input type="checkbox" id="fUnw" ${p.unwatched === '1' ? 'checked' : ''}> Unwatched</label>
        <label class="tog"><input type="checkbox" id="fUhd" ${p.uhd === '1' ? 'checked' : ''}> 4K only</label>
        ${(p.kind === 'movie' || p.kind === 'tv') ? '<button class="btn btn-ghost btn-sm" id="fSave">Save as row</button>' : ''}
        <button class="btn btn-ghost btn-sm" id="fReset">Reset</button></div>
      ${saved.filter((r) => r.kind === p.kind).length ? `<div class="saved-rows">Saved rows: ${saved.filter((r) => r.kind === p.kind).map((r) => `<span class="chip sm saved"><a href="#/browse?${new URLSearchParams({ ...r.params, kind: r.kind })}">${esc(r.name)}</a><button data-unsave="${r.id}" title="Remove">×</button></span>`).join('')}</div>` : ''}
    </div>
    <div class="browse-meta" id="bMeta"></div><div class="${vmode === 'list' ? 'rich-list' : 'grid'}" id="bGrid"></div><div id="bMore" class="more-sentinel"></div></div>`;

  let gmodeAll = p.gmode === 'all';
  const params = () => {
    const q = { kind: p.kind, sort: $('#fSort').value };
    const qtxt = ($('#fQuery')?.value || '').trim(); if (qtxt) q.q = qtxt;
    if (isDocs) { const ts = [...tsel]; if (ts.length) q.topic = ts.join(','); }
    else { const g = [...gsel]; if (g.length) q.g = g.join(','); if (gmodeAll && g.length > 1) q.gmode = 'all'; }
    if ($('#fPlat')?.value) q.platform = $('#fPlat').value;
    if ($('#fCountry')?.value) q.country = $('#fCountry').value;
    const d = $('#fDecade').value; if (d) { const [a, b] = d.split('-'); if (+a) q.y1 = a; if (+b < 9999) q.y2 = b; }
    if ($('#fRating').value) q.rmin = $('#fRating').value;
    const vt = $('#fVotes')?.value; if (vt) { const [a, b] = vt.split('-'); if (+a) q.vmin = a; if (+b) q.vmax = b; }
    const r = $('#fRuntime')?.value; if (r) { const [a, b] = r.split('-'); if (+a) q.rtmin = a; if (+b < 999) q.rtmax = b; }
    if ($('#fStatus')?.value) q.status = $('#fStatus').value;
    if ($('#fLang')?.value) q.lang = $('#fLang').value;
    if ($('#fUnw').checked) q.unwatched = '1'; if ($('#fUhd').checked) q.uhd = '1';
    return q;
  };
  const go = (q) => { history.replaceState(history.state, '', `#/browse?${new URLSearchParams(q)}`); load(q, 0, true); };
  let page = 0; let loading = false; let done = false; let cur = null; let loadedItems = [];
  const paintItems = (items, append) => {
    const mode = getViewMode();
    const grid = $('#bGrid'); if (!grid) return;
    if (!append) {
      grid.className = mode === 'list' ? 'rich-list' : 'grid';
      grid.innerHTML = items.length ? items.map(mode === 'list' ? richCard : poster).join('') : '<div class="empty" style="grid-column:1/-1">Nothing matches. Try loosening a filter.</div>';
    } else {
      grid.insertAdjacentHTML('beforeend', items.map(mode === 'list' ? richCard : poster).join(''));
    }
  };
  const updateFacets = (f) => {
    if (!f) return;
    if (isDocs && Array.isArray(f.topics)) {
      const m = new Map(f.topics.map((t) => [t.id, t.count]));
      $$('[data-fc]', view).forEach((el) => { const n = m.get(el.dataset.fc); el.textContent = n != null ? n : ''; });
    } else if (f.genres && typeof f.genres === 'object') {
      $$('[data-fc]', view).forEach((el) => { const n = f.genres[el.dataset.fc]; el.textContent = n ? n.toLocaleString() : ''; });
    }
  };
  const load = async (q, pg, reset) => {
    if (loading && !reset) return;
    loading = true; cur = q;
    const more = $('#bMore'); if (more) more.innerHTML = '';
    if (reset) { $('#bGrid').innerHTML = ''; page = 0; done = false; loadedItems = []; }
    try {
      const r = await api(`/api/browse?${new URLSearchParams({ ...q, page: pg })}`);
      if (cur !== q) return;
      const items = r.items || [];
      loadedItems = reset ? items : loadedItems.concat(items);
      paintItems(items, !reset);
      if (reset) updateFacets(r.facets);
      const f = r.facets || {};
      const stats = [`<b>${(r.total || 0).toLocaleString()}</b> title${r.total === 1 ? '' : 's'}`];
      if (f.movies != null && f.series != null && (p.kind === 'all' || p.kind === 'docs' || p.kind === 'anime')) stats.push(`${f.movies.toLocaleString()} movies · ${f.series.toLocaleString()} series`);
      if (f.uhd) stats.push(`${f.uhd.toLocaleString()} in 4K`);
      if (Array.isArray(f.platforms) && f.platforms.length) stats.push(`Top: ${f.platforms.slice(0, 3).map((x) => `${x.name} (${x.count})`).join(', ')}`);
      $('#bMeta').innerHTML = `<div class="browse-stats">${stats.join(' · ')}</div>${r.indexing ? ` · <span class="dim">still indexing (${Math.round((r.indexing.done / r.indexing.total) * 100)}%)</span>` : ''}`;
      page = pg; done = (pg + 1) * (r.per || 60) >= (r.total || 0);
    } catch (e) {
      if (cur === q && more) {
        more.innerHTML = '<div class="browse-retry"><button class="btn btn-ghost btn-sm" id="bRetry">Could not load more · Try again</button></div>';
        $('#bRetry').onclick = () => load(q, pg, reset);
      }
    } finally {
      if (cur === q) loading = false;
    }
  };
  $$('[data-bk]').forEach((b) => (b.onclick = () => { location.hash = `#/browse?kind=${b.dataset.bk}&sort=${$('#fSort').value}`; }));
  $$('[data-bg]').forEach((b) => (b.onclick = () => { const id = b.dataset.bg; gsel.has(id) ? gsel.delete(id) : gsel.add(id); b.classList.toggle('on'); go(params()); }));
  $$('[data-bt]').forEach((b) => (b.onclick = () => { const id = b.dataset.bt; tsel.has(id) ? tsel.delete(id) : tsel.add(id); b.classList.toggle('on'); go(params()); }));
  if ($('#fGmode')) $('#fGmode').onclick = () => { gmodeAll = !gmodeAll; $('#fGmode').classList.toggle('on', gmodeAll); $('#fGmode').textContent = `Match: ${gmodeAll ? 'ALL genres' : 'ANY genre'}`; go(params()); };
  $('#fQuery').oninput = debounce(() => go(params()), 220);
  for (const id of ['fPlat', 'fCountry', 'fDecade', 'fRating', 'fVotes', 'fRuntime', 'fStatus', 'fLang', 'fSort', 'fUnw', 'fUhd']) if ($('#' + id)) $('#' + id).onchange = () => go(params());
  $$('[data-vmode]', view).forEach((b) => (b.onclick = () => {
    setViewMode(b.dataset.vmode);
    $$('[data-vmode]', view).forEach((x) => x.classList.toggle('on', x === b));
    paintItems(loadedItems, false);
  }));
  $('#fReset').onclick = () => { location.hash = `#/browse?kind=${p.kind}`; };
  if ($('#fSave')) $('#fSave').onclick = async () => {
    const name = await promptText('Name this row', suggestName(params()));
    if (!name) return;
    const q = params(); delete q.kind;
    await post('/api/saved-rows', { name, kind: p.kind, params: q });
    toast(`Saved. "${name}" now appears at the top of ${p.kind === 'tv' ? 'Series' : 'Movies'}`); pageBrowseAll(location.hash.split('?')[1]);
  };
  $$('[data-unsave]').forEach((b) => (b.onclick = async (e) => { e.preventDefault(); await post('/api/saved-rows', { remove: b.dataset.unsave }); pageBrowseAll(location.hash.split('?')[1]); }));
  const io = trackObserver(new IntersectionObserver((ents) => { if (ents[0].isIntersecting && !done && !loading && cur) load(cur, page + 1, false); }, { rootMargin: '800px' }));
  io.observe($('#bMore'));
  load(params(), 0, true);
}
function suggestName(q) {
  const parts = [];
  if (q.g) parts.push(q.g.split(',').map((g) => (GENRES[q.kind] || GENRES.all || {})[g]).filter(Boolean).join(' & '));
  if (q.platform) parts.push(PLATFORMS_ALL.find(([k]) => k === q.platform)?.[1]);
  if (q.country) parts.push(countryName(q.country));
  if (q.y1 || q.y2) parts.push(q.y2 && q.y1 && q.y2 - q.y1 === 9 ? `${q.y1}s` : q.y1 ? `${q.y1}+` : `before ${q.y2}`);
  if (q.rmin) parts.push(`${q.rmin}+`);
  if (q.lang) parts.push(BROWSE_LANGS.find(([k]) => k === q.lang)?.[1]);
  return parts.filter(Boolean).join(' · ') || (q.kind === 'tv' ? 'My series' : 'My movies');
}
function promptText(title, value = '') {
  return new Promise((resolve) => {
    let settled = false;
    const settle = (v) => { if (settled) return; settled = true; resolve(v); };
    const done = (v) => { settle(v); closeModal(); };
    openModal(`<div class="sheet settings" style="max-width:440px"><button class="close" aria-label="Close">${I.close}</button><h2>${esc(title)}</h2>
      <div class="field" style="margin-top:14px"><input id="ptIn" value="${esc(value)}" maxlength="60"><button class="btn btn-accent btn-sm" id="ptOk" style="height:42px">Save</button></div></div>`, { noHistory: true, onClose: () => settle(null) });
    $('#ptOk').onclick = () => done($('#ptIn').value.trim());
    $('#ptIn').onkeydown = (e) => { if (e.key === 'Enter') done($('#ptIn').value.trim()); };
    $$('.close', modal).forEach((b) => (b.onclick = () => done(null)));
    setTimeout(() => $('#ptIn')?.select(), 30);
  });
}

// ---------------------------------------------------------------- person page
async function pagePerson(id) {
  const p = await api(`/api/person/${id}`);
  view.innerHTML = `<div class="page-pad"><div class="person-head">
      ${p.photo ? `<img src="${esc(p.photo)}" alt="">` : ''}
      <div><h1 class="page-title">${esc(p.name)}</h1><div class="dim">${esc(p.dept || '')}${p.born ? ` · born ${esc(p.born.slice(0, 4))}` : ''}</div>
      ${p.bio ? `<p class="bio">${esc(p.bio)}${p.bio.length >= 700 ? '…' : ''}</p>` : ''}</div></div>
    ${p.acting.length ? `<div class="section-label">Acting · ${p.acting.length} in your library</div><div class="grid">${p.acting.map(poster).join('')}</div>` : ''}
    ${p.crew.length ? `<div class="section-label">Directing & writing</div><div class="grid">${p.crew.map(poster).join('')}</div>` : ''}
    ${!p.acting.length && !p.crew.length ? '<div class="empty">Nothing of theirs is in your library right now.</div>' : ''}</div>`;
}

// ---------------------------------------------------------------- franchise (collection) + external titles
function franchiseCard(it) {
  const k = remember(it);
  return `<div class="wide" data-open="${k}" tabindex="0" role="button"><div class="thumb">${it.backdrop ? `<img src="${esc(it.backdrop.replace('/w1280/', '/w780/'))}" alt="" loading="lazy" onerror="this.remove()">` : ''}
    <div class="franchise-name">${esc(it.title)}</div></div><div class="w-sub">${esc(it.sub || '')}</div></div>`;
}
async function openCollection(id) {
  openModal(`<div class="sheet"><div class="sheet-hero skel"></div></div>`);
  const c = await api(`/api/collection/${id}`);
  if (modal.hidden) return;
  const avail = c.items.filter((x) => !x.missing);
  const next = avail.find((x) => !x.watched) || avail[0];
  openModal(`<div class="sheet"><button class="close" aria-label="Close">${I.close}</button>
    <div class="sheet-hero">${c.backdrop ? `<img src="${esc(c.backdrop)}" alt="">` : ''}<div class="hero-in"><h2>${esc(c.title)}</h2>
      <div class="btns">${next ? `<button class="btn btn-primary" id="cPlay">${I.play}${avail.indexOf(next) > 0 ? `Continue: ${esc(next.title)}` : 'Play in order'}</button>` : ''}</div></div></div>
    <div class="sheet-section" style="padding-top:10px"><p class="overview" style="margin:0 0 18px;color:var(--muted)">${esc(c.overview)}</p>
      <div class="coll-list">${c.items.map((x, i) => `<div class="coll-item ${x.missing ? 'missing' : ''}" ${x.missing ? '' : `data-open="${remember(x)}" tabindex="0" role="button"`}>
        <span class="n">${i + 1}</span>${x.poster ? `<img src="${esc(x.poster)}" alt="" loading="lazy">` : '<span class="ph"></span>'}
        <div><b>${esc(x.title)}</b><small>${x.year || ''}${x.watched ? ' · watched' : ''}${x.missing ? ' · not in your library' : ''}</small></div></div>`).join('')}</div></div></div>`, { keepHistory: true });
  if ($('#cPlay')) $('#cPlay').onclick = () => quickPlay(next);
}
async function openExternal(it) {
  openModal(`<div class="sheet"><div class="sheet-hero skel"></div></div>`);
  const d = await api(`/api/external/${it.mt}/${it.tmdb}`).catch(() => it);
  if (modal.hidden) return;
  const when = d.release ? new Date(d.release) : null; const future = when && when > Date.now();
  openModal(`<div class="sheet"><button class="close" aria-label="Close">${I.close}</button>
    <div class="sheet-hero">${d.backdrop ? `<img src="${esc(d.backdrop)}" alt="">` : ''}<div class="hero-in"><h2>${esc(d.title)}</h2>
      <div class="btns"><button class="btn ${d.wanted ? 'btn-ghost' : 'btn-accent'}" id="xWant">${d.wanted ? `${I.bellOn}Alert set` : `${I.bell}Notify me when it's available`}</button>
      ${d.trailer ? `<button class="btn btn-ghost" id="xTrailer">${I.film}Trailer</button>` : ''}</div></div></div>
    <div class="sheet-body"><div><div class="meta"><span class="pill">Not in your library${future ? ' yet' : ''}</span>${when ? `<span>${future ? 'Out ' : ''}${when.toLocaleDateString([], { day: 'numeric', month: 'short', year: 'numeric' })}</span>` : ''}${d.rating ? `<span class="star">★ ${(+d.rating).toFixed(1)}</span>` : ''}</div>
      <p class="overview">${esc(d.overview || '')}</p></div>
      <div class="facts">${d.cast?.length ? `<div><b>Cast:</b> ${esc(d.cast.join(', '))}</div>` : ''}${d.genres?.length ? `<div><b>Genres:</b> ${esc(d.genres.join(', '))}</div>` : ''}
      <div>We'll check every catalogue refresh and alert you the moment it appears.</div></div></div></div>`, { keepHistory: true });
  $('#xWant').onclick = async () => {
    d.wanted = !d.wanted; await post('/api/wanted', { mt: it.mt, tmdb: it.tmdb, on: d.wanted, title: d.title, poster: d.poster, year: d.year });
    $('#xWant').className = `btn ${d.wanted ? 'btn-ghost' : 'btn-accent'}`; $('#xWant').innerHTML = d.wanted ? `${I.bellOn}Alert set` : `${I.bell}Notify me when it's available`;
    toast(d.wanted ? `We'll alert you when ${d.title} lands` : 'Alert removed');
  };
  if ($('#xTrailer')) $('#xTrailer').onclick = () => { const h = $('.sheet-hero', modal); const f = document.createElement('div'); f.className = 'trailer-frame'; f.innerHTML = `<iframe src="https://www.youtube-nocookie.com/embed/${encodeURIComponent(d.trailer)}?autoplay=1&rel=0" allow="autoplay; fullscreen" allowfullscreen></iframe>`; h.appendChild(f); };
}

// ---------------------------------------------------------------- hover preview card
const Peek = (() => {
  let el = null; let timer = null; let hideT = null; let curKey = null; const cache = new Map();
  const hide = () => { clearTimeout(timer); hideT = setTimeout(() => { el?.remove(); el = null; curKey = null; }, 120); };
  async function show(card) {
    const key = card.dataset.open; const it = ITEMS.get(key); if (!it || !['m', 's'].includes(it.k)) return;
    let d = cache.get(key);
    if (!d) { d = await api(`/api/peek/${it.k}/${it.id}`).catch(() => null); if (d) cache.set(key, d); }
    if (!d?.title || !card.matches(':hover') || !$('#player').hidden || !modal.hidden) return;
    el?.remove(); curKey = key;
    const r = card.getBoundingClientRect(); const w = Math.max(320, r.width * 2.1);
    let left = r.left + r.width / 2 - w / 2; left = Math.max(12, Math.min(innerWidth - w - 12, left));
    const top = Math.max(76, r.top - 24);
    el = document.createElement('div'); el.className = 'peek'; el.style.cssText = `left:${left}px;top:${top}px;width:${w}px`;
    const meta = [d.rating ? `<span class="star">★ ${(+d.rating).toFixed(1)}</span>` : '', d.year || '', d.runtime && it.k === 'm' ? runtime(d.runtime) : '', d.seasons ? `${d.seasons} season${d.seasons > 1 ? 's' : ''}` : '', d.uhd ? '<span class="pill uhd">4K</span>' : '', d.watched ? '<span class="pill">Watched</span>' : ''].filter(Boolean).join(' · ');
    const k2 = remember({ ...it, ...d, k: it.k, id: d.id || it.id });
    el.innerHTML = `<div class="peek-img">${d.backdrop || d.poster ? `<img src="${esc((d.backdrop || d.poster).replace('/w1280/', '/w780/'))}" alt="">` : ''}</div>
      <div class="peek-body"><div class="peek-title">${esc(d.title)}</div><div class="peek-meta">${meta}</div>
      ${d.genres?.length ? `<div class="peek-genres">${d.genres.map(esc).join(' • ')}</div>` : ''}
      <p>${esc(d.overview || '')}</p>
      <div class="peek-btns"><button class="btn btn-primary btn-sm" data-quickplay="${k2}">${I.play}Play</button><button class="btn btn-ghost btn-sm" data-open="${k2}">More info</button>
      ${d.tmdb ? `<button class="pbtn sm" data-rate="1" data-key="${k2}" title="Like">${I.up}</button><button class="pbtn sm" data-rate="-1" data-key="${k2}" title="Not for me">${I.down}</button>` : ''}</div></div>`;
    el.onmouseenter = () => clearTimeout(hideT); el.onmouseleave = hide;
    el.addEventListener('click', () => setTimeout(() => { el?.remove(); el = null; }, 0));
    document.body.appendChild(el);
    requestAnimationFrame(() => el?.classList.add('on'));
  }
  document.addEventListener('mouseover', (e) => {
    const card = e.target.closest('.card[data-open]');
    if (!card) return;
    clearTimeout(hideT);
    if (card.dataset.open === curKey) return;
    clearTimeout(timer); timer = setTimeout(() => show(card), 650);
  });
  document.addEventListener('mouseout', (e) => { const card = e.target.closest('.card[data-open]'); if (card && !card.contains(e.relatedTarget)) hide(); });
  window.addEventListener('scroll', () => { el?.remove(); el = null; curKey = null; clearTimeout(timer); }, { passive: true });
  return {
    hide: () => { el?.remove(); el = null; curKey = null; clearTimeout(timer); },
    prune: () => { if (cache.size > 120) { const drop = cache.size - 80; let i = 0; for (const k of cache.keys()) { cache.delete(k); if (++i >= drop) break; } } },
  };
})();

// ---------------------------------------------------------------- watch-night concierge
async function openConcierge(presetKind = '') {
  const raw = (presetKind || '').toLowerCase();
  const hashTab = (location.hash.slice(2) || '').split(/[/?]/)[0];
  const state = {
    kind: (raw === 'tv' || raw === 'series') ? 'tv'
      : raw === 'anime' ? 'anime'
      : (raw === 'docs' || raw === 'doc' || raw === 'documentary') ? 'docs'
      : (raw === 'any' || raw === 'all') ? 'any'
      : (raw === 'movie' || raw === 'movies') ? 'movie'
      : hashTab === 'docs' ? 'docs'
      : hashTab === 'anime' ? 'anime'
      : hashTab === 'series' ? 'tv'
      : 'movie',
    mood: 'any',
    maxMin: 0,
    minRating: 7.0,
    uhd: false,
    unwatched: true,
    exclude: [],
  };
  const KINDS = [['movie', 'Movies'], ['tv', 'Series'], ['docs', 'Documentaries'], ['anime', 'Anime'], ['any', 'Anything']];
  const MOODS_STD = [
    ['any', 'Any mood'], ['action', 'Action'], ['scifi', 'Sci-Fi'], ['thriller', 'Thriller'],
    ['comedy', 'Comedy'], ['crime', 'Crime'], ['horror', 'Horror'], ['drama', 'Drama'],
    ['feelgood', 'Feel-good'], ['mindbender', 'Mind-bender'],
  ];
  const MOODS_DOC = [
    ['any', 'Any topic'], ['crime', 'True Crime'], ['nature', 'Nature & Wildlife'], ['history', 'History & War'],
    ['science', 'Science & Space'], ['sports', 'Sports'], ['music', 'Music & Arts'],
    ['bio', 'Biography'], ['society', 'Society & Culture'], ['food', 'Food & Travel'],
  ];
  const LENS = [[0, 'Any length'], [95, 'Under 95m'], [125, 'Under 2h']];
  const RATINGS = [[7.0, '7.0+'], [7.5, '7.5+'], [8.0, '8.0+']];
  let picks = [];
  let reqId = 0;

  const pills = (group, list, cur) => list.map(([v, l]) =>
    `<button type="button" class="chip ${String(cur) === String(v) ? 'on' : ''}" data-cg="${group}" data-v="${v}">${esc(l)}</button>`
  ).join('');

  openModal(`<div class="sheet concierge">
    <button class="close" aria-label="Close">${I.close}</button>
    <div class="cg-head">
      <div><h2>🎲 Pick for me</h2><p class="muted">Watch-night concierge · 3 instant picks from your library</p></div>
      <span class="pill" id="cgCount">…</span>
    </div>
    <div class="cg-filters">
      <div class="cg-row"><span class="cg-lbl">Type</span><div class="cg-pills">${pills('kind', KINDS, state.kind)}</div></div>
      <div class="cg-row"><span class="cg-lbl" id="cgMoodLbl">${state.kind === 'docs' ? 'Topic' : 'Mood'}</span><div class="cg-pills" id="cgMoods">${pills('mood', state.kind === 'docs' ? MOODS_DOC : MOODS_STD, state.mood)}</div></div>
      <div class="cg-row"><span class="cg-lbl">Length</span><div class="cg-pills">${pills('maxMin', LENS, state.maxMin)}</div>
        <span class="cg-lbl" style="margin-left:10px">Score</span><div class="cg-pills">${pills('minRating', RATINGS, state.minRating)}</div>
      </div>
      <div class="cg-row cg-checks">
        <label class="tog"><input type="checkbox" id="cgUhd" ${state.uhd ? 'checked' : ''}> 4K only</label>
        <label class="tog"><input type="checkbox" id="cgUnwatched" ${state.unwatched ? 'checked' : ''}> Unwatched only</label>
      </div>
    </div>
    <div class="cg-grid" id="cgPicks"><div class="loading" style="min-height:220px"><div class="spinner"></div></div></div>
    <div class="cg-foot">
      <button class="btn btn-primary" id="cgRoll">🎲 Roll 3 new picks</button>
      <button class="btn btn-ghost btn-sm" id="cgReset" hidden>Reset filters</button>
    </div>
  </div>`);

  const renderPicks = (data) => {
    picks = Array.isArray(data?.picks) ? data.picks : [];
    const total = data?.totalMatches ?? 0;
    const cnt = $('#cgCount', modal);
    if (cnt) cnt.textContent = total ? `${total.toLocaleString()} match${total === 1 ? '' : 'es'}` : 'No matches';
    const box = $('#cgPicks', modal);
    if (!box) return;
    if (!picks.length) {
      box.innerHTML = `<div class="empty cg-empty">No titles match those exact filters. Try relaxing the mood, length, or 4K toggle.</div>`;
      if ($('#cgReset', modal)) $('#cgReset', modal).hidden = false;
      return;
    }
    if ($('#cgReset', modal)) $('#cgReset', modal).hidden = true;
    box.innerHTML = picks.map((p, idx) => {
      const img = p.backdrop || p.poster || '';
      const meta = [
        p.rating ? `<span class="star">★ ${(+p.rating).toFixed(1)}</span>` : '',
        p.year || '',
        p.k === 'm' && p.runtime ? runtime(p.runtime) : (p.seasons ? `${p.seasons} season${p.seasons === 1 ? '' : 's'}` : ''),
        p.scores?.imdb ? `<span class="cg-sc">IMDb ${esc(p.scores.imdb)}</span>` : '',
        p.scores?.rt ? `<span class="cg-sc">${parseInt(p.scores.rt, 10) >= 60 ? '🍅' : '🤢'} ${esc(p.scores.rt)}</span>` : '',
      ].filter(Boolean).join(' · ');
      const badges = [
        p.uhd ? '<span class="pill uhd">4K</span>' : '',
        p.isDoc ? '<span class="pill doc">DOC</span>' : '',
        p.isAnime && p.dubStatus === 'yes' ? '<span class="pill dub">DUB</span>' : '',
        p.k === 's' ? '<span class="pill">Series</span>' : '',
      ].filter(Boolean).join('');
      const topics = Array.isArray(p.docTopics) && p.docTopics.length ? p.docTopics : (Array.isArray(p.genres) ? p.genres : []);
      const genres = topics.length ? `<div class="cg-genres">${topics.slice(0, 3).map(esc).join(' • ')}</div>` : '';
      return `<article class="cg-card">
        <div class="cg-thumb">${img ? `<img src="${esc(img.replace('/w1280/', '/w780/'))}" alt="" loading="lazy">` : ''}<div class="cg-badges">${badges}</div></div>
        <div class="cg-body">
          <h3>${esc(p.title)}</h3>
          <div class="cg-meta">${meta}</div>
          ${genres}
          <p class="cg-ov">${esc(p.overview || 'No synopsis available.')}</p>
          <div class="cg-acts">
            <button class="btn btn-primary btn-sm" data-cg-play="${idx}">${I.play}${p.k === 's' ? 'Episodes' : 'Play'}</button>
            <button class="btn btn-ghost btn-sm" data-cg-info="${idx}">Details</button>
          </div>
        </div>
      </article>`;
    }).join('');
    $$('[data-cg-play]', box).forEach((b) => (b.onclick = () => {
      const p = picks[+b.dataset.cgPlay]; if (!p) return;
      if (p.k === 's') openDetail(p); else quickPlay(p);
    }));
    $$('[data-cg-info]', box).forEach((b) => (b.onclick = () => {
      const p = picks[+b.dataset.cgInfo]; if (p) openDetail(p);
    }));
  };

  const roll = async (isReroll = false) => {
    const my = ++reqId;
    const rollBtn = $('#cgRoll', modal);
    if (rollBtn) { rollBtn.disabled = true; rollBtn.textContent = 'Picking…'; }
    try {
      let data = await post('/api/concierge', {
        kind: state.kind, mood: state.mood, maxMin: state.maxMin, minRating: state.minRating,
        uhd: state.uhd, unwatched: state.unwatched, exclude: state.exclude,
      });
      if (my !== reqId || modal.hidden) return;
      if (isReroll && (!data.picks || !data.picks.length) && state.exclude.length > 0 && (data.totalMatches || 0) > 0) {
        state.exclude = [];
        data = await post('/api/concierge', {
          kind: state.kind, mood: state.mood, maxMin: state.maxMin, minRating: state.minRating,
          uhd: state.uhd, unwatched: state.unwatched, exclude: [],
        });
        if (my !== reqId || modal.hidden) return;
        toast('Cycled through all matches — starting fresh');
      }
      renderPicks(data);
    } catch (e) {
      if (my === reqId && !modal.hidden) toast(e.message || 'Could not load picks');
    } finally {
      if (my === reqId && rollBtn && rollBtn.isConnected) {
        rollBtn.disabled = false; rollBtn.textContent = '🎲 Roll 3 new picks';
      }
    }
  };

  const wireCgPills = () => {
    $$('[data-cg]', modal).forEach((btn) => (btn.onclick = () => {
      const g = btn.dataset.cg; const v = btn.dataset.v;
      if (g === 'maxMin') state.maxMin = +v;
      else if (g === 'minRating') state.minRating = +v;
      else state[g] = v;
      $$(`[data-cg="${g}"]`, modal).forEach((b) => b.classList.toggle('on', b === btn));
      if (g === 'kind') {
        state.mood = 'any';
        const isD = state.kind === 'docs';
        if ($('#cgMoodLbl', modal)) $('#cgMoodLbl', modal).textContent = isD ? 'Topic' : 'Mood';
        if ($('#cgMoods', modal)) $('#cgMoods', modal).innerHTML = pills('mood', isD ? MOODS_DOC : MOODS_STD, 'any');
        wireCgPills();
      }
      state.exclude = [];
      roll(false);
    }));
  };
  wireCgPills();
  $('#cgUhd', modal).onchange = (e) => { state.uhd = e.target.checked; state.exclude = []; roll(false); };
  $('#cgUnwatched', modal).onchange = (e) => { state.unwatched = e.target.checked; state.exclude = []; roll(false); };
  $('#cgRoll', modal).onclick = () => {
    for (const p of picks) { if (p.key && !state.exclude.includes(p.key)) state.exclude.push(p.key); }
    roll(true);
  };
  $('#cgReset', modal).onclick = () => {
    state.mood = 'any'; state.maxMin = 0; state.minRating = 7.0; state.uhd = false; state.unwatched = true; state.exclude = [];
    $$('[data-cg="mood"]', modal).forEach((b) => b.classList.toggle('on', b.dataset.v === 'any'));
    $$('[data-cg="maxMin"]', modal).forEach((b) => b.classList.toggle('on', b.dataset.v === '0'));
    $$('[data-cg="minRating"]', modal).forEach((b) => b.classList.toggle('on', b.dataset.v === '7'));
    $('#cgUhd', modal).checked = false;
    $('#cgUnwatched', modal).checked = true;
    roll(false);
  };
  roll(false);
}

// ---------------------------------------------------------------- router
let statusBooted = false;
let statusAt = 0;
function updateSubPill() {
  const pill = $('#subPill');
  if (!pill) return;
  const dl = ACCOUNT?.daysLeft;
  pill.hidden = !(dl != null && dl <= 14);
  pill.textContent = dl > 0 ? `${dl} day${dl === 1 ? '' : 's'} left` : 'Expired';
}
function refreshStatusBg() {
  const now = Date.now();
  if (now - statusAt >= 25000 || !statusBooted) {
    statusAt = now;
    api('/api/status').then((s) => { STATUS = s; statusBooted = true; }).catch(() => {});
  }
  if (!ACCOUNT || now - ACCOUNT._t >= 600e3) {
    api('/api/account').then((a) => { ACCOUNT = { ...a, _t: Date.now() }; updateSubPill(); }).catch(() => {});
  } else {
    updateSubPill();
  }
}
async function route() {
  runRouteCleanups();
  stopHero();
  const myRoute = ++routeGen;
  routeAbort = new AbortController();
  if (!modal.hidden) closeModal(true); // navigating (e.g. clicking a cast name) closes the detail sheet
  if ($('#cmdk') && !$('#cmdk').hidden) Cmdk.close();
  if ($('#player').hidden) { ITEMS.clear(); FIGHT_ITEMS.clear(); FIXTURES.clear(); }
  Peek.prune();
  const [p, qs] = (location.hash.slice(2) || 'home').split('?');
  const tab = p.split('/')[0];
  $$('.tabs a').forEach((a) => a.classList.toggle('active', a.dataset.tab === tab));
  const hubTabs = new Set(['radar', 'marathons', 'dna', 'specials', 'tonight']);
  $('#hubsBtn')?.classList.toggle('active', hubTabs.has(tab));
  if ($('#hubsMenu')) $('#hubsMenu').hidden = true;
  if ($('#hubsBtn')) $('#hubsBtn').setAttribute('aria-expanded', 'false');
  if (tab !== 'search') $('#searchInput').value = '';
  if (!statusBooted) {
    STATUS = await api('/api/status').catch(() => ({}));
    statusBooted = true; statusAt = Date.now();
    if (myRoute !== routeGen) return;
    if (!ACCOUNT) api('/api/account').then((a) => { ACCOUNT = { ...a, _t: Date.now() }; updateSubPill(); }).catch(() => {});
  } else {
    refreshStatusBg();
  }
  if (!STATUS.ready && !document.querySelector('.loading')) loadingScreen();
  window.scrollTo(0, 0); Peek.hide();
  const pages = {
    browse: () => pageBrowseAll(qs), person: () => pagePerson(p.split('/')[1]), home: pageHome,
    series: () => pageBrowse('series'), movies: () => pageBrowse('movies'), docs: pageDocs, anime: pageAnime,
    '4k': () => pageShowcase(qs), fights: () => pageFights(qs), radar: pageRadar,
    marathons: () => pageMarathons(p.split('/')[1], qs), dna: pageDna, specials: () => pageSpecials(qs),
    sports: pageSports, live: pageLive, guide: pageGuide, tonight: pageTonight, mylist: pageMyList,
    search: () => pageSearch(new URLSearchParams(qs).get('q') || ''),
  };
  await guard(pages[tab] || pageHome);
  if (myRoute !== routeGen) return;
  const y = SCROLL.get(location.hash); if (y) requestAnimationFrame(() => window.scrollTo(0, y));
  if (tab === 'home' && !route.prefetched) {
    route.prefetched = true;
    setTimeout(() => {
      apiCached('/api/movies', 120000).catch(() => {});
      apiCached('/api/series', 120000).catch(() => {});
      apiCached('/api/live/groups', 120000).catch(() => {});
    }, 2500);
  }
  if (typeof pushRemoteState === 'function') pushRemoteState();
}
const SCROLL = new Map();
window.addEventListener('hashchange', (e) => { try { SCROLL.set(new URL(e.oldURL).hash, LAST_Y); } catch {} });
let LAST_Y = 0; window.addEventListener('scroll', () => { LAST_Y = scrollY; }, { passive: true });
window.addEventListener('hashchange', route);
window.addEventListener('scroll', () => $('#nav').classList.toggle('solid', scrollY > 30), { passive: true });
$('#searchInput').addEventListener('input', debounce((e) => {
  const q = e.target.value.trim();
  if (q) {
    const next = `#/search?q=${encodeURIComponent(q)}`;
    if (location.hash.startsWith('#/search')) {
      history.replaceState(history.state, '', next);
      pageSearch(q);
    } else {
      location.hash = next;
    }
  } else if (location.hash.startsWith('#/search')) {
    location.hash = '#/home';
  }
}, 280));
$('#settingsBtn').onclick = openSettings;
(() => {
  const btn = $('#hubsBtn'); const menu = $('#hubsMenu');
  if (!btn || !menu) return;
  btn.onclick = (e) => {
    e.stopPropagation();
    const open = menu.hidden;
    menu.hidden = !open;
    btn.setAttribute('aria-expanded', open ? 'true' : 'false');
  };
  document.addEventListener('click', (e) => {
    if (!menu.hidden && !e.target.closest('#hubsDd')) { menu.hidden = true; btn.setAttribute('aria-expanded', 'false'); }
  });
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && !menu.hidden) { menu.hidden = true; btn.setAttribute('aria-expanded', 'false'); }
  });
})();

// ---------------------------------------------------------------- global clicks
document.addEventListener('click', (e) => {
  const cwManage = e.target.closest('[data-cw-manage]');
  if (cwManage) { e.stopPropagation(); return openManageCw(); }
  const cwPin = e.target.closest('[data-cw-pin]');
  if (cwPin) {
    e.stopPropagation();
    const key = cwPin.dataset.cwPin; const on = cwPin.dataset.pinned !== '1';
    post('/api/continue/pin', { key, on }).then((r) => {
      if (Array.isArray(r.items)) refreshCwRows(r.items);
      toast(on ? 'Pinned to front of Continue Watching' : 'Unpinned');
    }).catch((err) => toast(err.message));
    return;
  }
  const cwDone = e.target.closest('[data-cw-done]');
  if (cwDone) {
    e.stopPropagation();
    const it = ITEMS.get(cwDone.dataset.cwDone); if (!it) return;
    post('/api/continue/done', { key: it.key || (it.k === 'e' ? `e:${it.id}` : `m:${it.id}`), dur: it.dur, tmdb: it.tmdb, item: it }).then((r) => {
      if (Array.isArray(r.items)) refreshCwRows(r.items);
      toast(it.k === 'e' ? 'Marked episode watched · moved to next' : 'Marked as watched');
    }).catch((err) => toast(err.message));
    return;
  }
  const cwRestart = e.target.closest('[data-cw-restart]');
  if (cwRestart) { e.stopPropagation(); return restartCwItem(ITEMS.get(cwRestart.dataset.cwRestart)); }
  const cwInfo = e.target.closest('[data-cw-info]');
  if (cwInfo) { e.stopPropagation(); return openCwInfo(ITEMS.get(cwInfo.dataset.cwInfo)); }
  const cwRm = e.target.closest('[data-cw-remove]');
  if (cwRm) {
    e.stopPropagation();
    const key = cwRm.dataset.cwRemove;
    const card = cwRm.closest('.wide');
    const rowEl = card?.closest('.row');
    post('/api/continue/remove', { key }).then((r) => {
      if (Array.isArray(r.items)) refreshCwRows(r.items);
      else { card?.remove(); if (rowEl && !rowEl.querySelector('.wide')) rowEl.remove(); }
      toast('Removed from Continue Watching');
    }).catch((err) => toast(err.message));
    return;
  }
  const fCard = e.target.closest('[data-fight-card]');
  if (fCard) { e.stopPropagation(); const f = FIGHT_ITEMS.get(fCard.dataset.fightCard); if (f) return openFightCardModal(f); }
  const fPlay = e.target.closest('[data-fight-play]');
  if (fPlay) { e.stopPropagation(); const f = FIGHT_ITEMS.get(fPlay.dataset.fightPlay); if (f) return playFightReplay(f); }
  const r = e.target.closest('[data-rate]');
  if (r) { e.stopPropagation(); return rateItem(ITEMS.get(r.dataset.key), +r.dataset.rate); }
  const t = e.target.closest('[data-open],[data-quickplay],[data-resume],[data-live],[data-fav],[data-settings],[data-fixture],[data-surprise]');
  if (!t) return;
  if (t.dataset.fixture) return playFixture(FIXTURES.get(t.dataset.fixture));
  if (t.dataset.surprise !== undefined) return openConcierge(t.dataset.surprise);
  if (t.dataset.fav) { e.stopPropagation(); return toggleFav(t); }
  if (t.dataset.settings !== undefined) return openSettings();
  if (t.dataset.open) return openDetail(ITEMS.get(t.dataset.open));
  if (t.dataset.quickplay) return quickPlay(ITEMS.get(t.dataset.quickplay));
  if (t.dataset.resume) return resumeItem(ITEMS.get(t.dataset.resume));
  if (t.dataset.live) { const it = ITEMS.get(t.dataset.live); return playLive(it); }
});
document.addEventListener('keydown', (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k') {
    if ($('#player').hidden) { e.preventDefault(); Cmdk.toggle(); }
    return;
  }
  const cmdkEl = $('#cmdk');
  const cmdkOpen = !!(cmdkEl && !cmdkEl.hidden);
  if (e.key === 'Escape' && cmdkOpen) { e.preventDefault(); Cmdk.close(); return; }
  if (cmdkOpen) return;
  if ((e.key === 'Enter' || e.key === ' ') && e.target.matches('.card,.wide,.event,.ch,.rc,.fcard,.ep,.ld-item,.radar-card,.mar-step,.coll-item') && !$('#player:not([hidden])')) { e.preventDefault(); e.target.click(); }
  if (e.key === 'Escape' && !$('#modal').hidden && $('#player').hidden) closeModal();
  if (e.key === '/' && document.activeElement.tagName !== 'INPUT' && document.activeElement.tagName !== 'TEXTAREA' && $('#player').hidden) { e.preventDefault(); $('#searchInput').focus(); }
});

async function toggleFav(btn) {
  const it = ITEMS.get(btn.dataset.fav); const on = !btn.classList.contains('on');
  await post('/api/fav', { key: `l:${it.id}`, on, item: { k: 'l', id: it.id, title: it.title, logo: it.logo, epg: it.epg } });
  invalidateCache('/api/live/groups', '/api/mylist');
  btn.classList.toggle('on', on); btn.innerHTML = on ? I.star : I.starO;
  toast(on ? 'Added to favourites' : 'Removed from favourites');
}

// ---------------------------------------------------------------- details modal
const modal = $('#modal');
let modalPrevFocus = null;
let modalOnClose = null;
function openModal(html, opts = {}) {
  if (modal.hidden) modalPrevFocus = document.activeElement;
  modalOnClose = typeof opts.onClose === 'function' ? opts.onClose : null;
  if (!opts.noHistory && !history.state?.modal) history.pushState({ modal: true }, '', location.href);
  Peek.hide();
  modal.innerHTML = html; modal.hidden = false; document.body.style.overflow = 'hidden'; modal.scrollTop = 0;
  modal.onclick = (e) => { if (e.target === modal) closeModal(); };
  $$('.close', modal).forEach((b) => (b.onclick = closeModal));
}
function closeModal(fromPop) {
  if (modal.hidden) return;
  clearTimeout(detailTrailerT);
  const cb = modalOnClose; modalOnClose = null;
  const prev = modalPrevFocus; modalPrevFocus = null;
  modal.hidden = true; modal.innerHTML = ''; document.body.style.overflow = $('#cmdk')?.hidden !== false ? '' : 'hidden';
  if (fromPop !== true && history.state?.modal) history.back();
  try { cb?.(); } catch {}
  if (prev && prev.isConnected && $('#player').hidden && $('#cmdk')?.hidden !== false) {
    try { prev.focus({ preventScroll: true }); } catch {}
  }
}

// ---------------------------------------------------------------- Ctrl+K Command Palette
const Cmdk = (() => {
  const host = $('#cmdk');
  let prevFocus = null;
  let selIdx = 0;
  let rows = [];
  let ctrl = null;
  let qTimer = null;

  const ACTIONS = [
    { id: 'resume', icon: '▶', title: 'Resume last watched', sub: 'Jump straight back into your latest title', tag: 'Action', kw: 'continue watching resume play last', run: async () => {
      const d = await api('/api/continue').catch(() => null);
      const first = d?.items?.[0];
      if (first) return resumeItem(first);
      toast('Nothing in Continue Watching yet');
    } },
    { id: 'pick', icon: '🎲', title: 'Pick for me', sub: 'Open Watch-Night Concierge', tag: 'Action', kw: 'concierge random surprise pick', run: () => openConcierge('') },
    { id: 'settings', icon: '⚙', title: 'Open Settings', sub: 'Display, mpv, phone remote, Trakt & library', tag: 'Action', kw: 'settings preferences config', run: () => openSettings() },
    { id: 'tvmode', icon: '📺', title: 'Toggle TV Mode', sub: 'Couch-friendly big cards & arrow navigation', tag: 'Action', kw: 'tv couch gamepad remote mode', run: () => setTvMode(!document.body.classList.contains('tv')) },
    { id: 'display', icon: '🖥', title: 'Restore OLED Display', sub: 'Reset Windows refresh rate & HDR immediately', tag: 'Action', kw: 'display hdr hz oled restore screen', run: () => post('/api/display/restore', {}).then(() => toast('OLED display restored')).catch((e) => toast(e.message)) },
    { id: 'go-home', icon: '🏠', title: 'Home', sub: 'Trending rows & Continue Watching', tag: 'Jump', kw: 'home', run: () => { location.hash = '#/home'; } },
    { id: 'go-browse', icon: '🧭', title: 'Browse & Explore', sub: 'Deep multi-facet filter across library', tag: 'Jump', kw: 'browse explore filter', run: () => { location.hash = '#/browse'; } },
    { id: 'go-movies', icon: '🎬', title: 'Movies', sub: 'Curated movies & trending', tag: 'Jump', kw: 'movies films', run: () => { location.hash = '#/movies'; } },
    { id: 'go-series', icon: '📺', title: 'Series', sub: 'TV shows & ongoing seasons', tag: 'Jump', kw: 'series shows tv', run: () => { location.hash = '#/series'; } },
    { id: 'go-anime', icon: '✨', title: 'Anime Hub', sub: 'English dubbed anime with auto-skip', tag: 'Jump', kw: 'anime dub', run: () => { location.hash = '#/anime'; } },
    { id: 'go-docs', icon: '🌍', title: 'Documentaries', sub: 'Acclaimed documentaries & docuseries', tag: 'Jump', kw: 'docs documentaries', run: () => { location.hash = '#/docs'; } },
    { id: 'go-4k', icon: '💎', title: '4K OLED Showcase', sub: '4K Dolby Vision, HDR & Blu-Ray masters', tag: 'Jump', kw: '4k uhd hdr dv showcase', run: () => { location.hash = '#/4k'; } },
    { id: 'go-fights', icon: '🥊', title: 'UFC & Sports Replays', sub: 'Spoiler-free PPV & Fight Night replays', tag: 'Jump', kw: 'ufc fights boxing replays', run: () => { location.hash = '#/fights'; } },
    { id: 'go-sports', icon: '⚽', title: 'Live Sports', sub: 'Live games, scores & team feeds', tag: 'Jump', kw: 'sports live games scores', run: () => { location.hash = '#/sports'; } },
    { id: 'go-live', icon: '📡', title: 'Live TV', sub: 'Browse live channels by region & group', tag: 'Jump', kw: 'live tv channels', run: () => { location.hash = '#/live'; } },
    { id: 'go-guide', icon: '🗓', title: 'TV Guide', sub: 'Programme grid & catch-up', tag: 'Jump', kw: 'guide epg catchup schedule', run: () => { location.hash = '#/guide'; } },
    { id: 'go-radar', icon: '📅', title: 'Release Radar', sub: 'Upcoming episodes, digital drops & UFC schedule', tag: 'Jump', kw: 'radar upcoming calendar', run: () => { location.hash = '#/radar'; } },
    { id: 'go-marathons', icon: '🎞', title: 'Marathons & Watch Order', sub: 'Franchise binge guides in story or release order', tag: 'Jump', kw: 'marathons franchises watch order', run: () => { location.hash = '#/marathons'; } },
    { id: 'go-dna', icon: '🧬', title: 'Cinema DNA', sub: 'Your watch stats & favorite filmmakers', tag: 'Jump', kw: 'dna stats taste profile', run: () => { location.hash = '#/dna'; } },
    { id: 'go-specials', icon: '🎤', title: 'Stand-Up, Concerts & Prestige', sub: 'Comedy specials, concerts, IMDb 250 & World Cinema', tag: 'Jump', kw: 'standup comedy concerts imdb prestige', run: () => { location.hash = '#/specials'; } },
    { id: 'go-tonight', icon: '🌙', title: 'Tonight on TV', sub: 'Live TV highlights & games tonight', tag: 'Jump', kw: 'tonight tv', run: () => { location.hash = '#/tonight'; } },
    { id: 'go-mylist', icon: '★', title: 'My List', sub: 'Saved movies, series & favourite channels', tag: 'Jump', kw: 'mylist saved favourites', run: () => { location.hash = '#/mylist'; } },
  ];

  function paint() {
    const list = $('#cmdkList', host); if (!list) return;
    if (!rows.length) {
      list.innerHTML = '<div class="cmdk-empty">No matching titles or actions.</div>';
      return;
    }
    selIdx = Math.max(0, Math.min(rows.length - 1, selIdx));
    let lastSec = ''; let html = '';
    rows.forEach((r, i) => {
      if (r.sec !== lastSec) { lastSec = r.sec; html += `<div class="cmdk-sec">${esc(r.sec)}</div>`; }
      const ico = r.img
        ? `<span class="cmdk-ico ${r.contain ? 'contain' : ''}"><img src="${esc(r.img)}" alt="" onerror="this.remove()"></span>`
        : `<span class="cmdk-ico">${esc(r.icon || '•')}</span>`;
      html += `<button type="button" class="cmdk-item ${i === selIdx ? 'on' : ''}" data-ci="${i}">
        ${ico}
        <span class="cmdk-main"><div class="cmdk-t">${esc(r.title)}</div>${r.sub ? `<div class="cmdk-s">${esc(r.sub)}</div>` : ''}</span>
        ${r.tag ? `<span class="cmdk-tag">${esc(r.tag)}</span>` : ''}
      </button>`;
    });
    list.innerHTML = html;
    $$('[data-ci]', list).forEach((b) => {
      b.onmouseenter = () => {
        selIdx = +b.dataset.ci;
        $$('.cmdk-item.on', list).forEach((x) => x.classList.remove('on'));
        b.classList.add('on');
      };
      b.onclick = (e) => runItem(+b.dataset.ci, e.shiftKey);
    });
    $('.cmdk-item.on', list)?.scrollIntoView({ block: 'nearest' });
  }

  function runItem(idx, shiftKey) {
    const r = rows[idx]; if (!r) return;
    close();
    r.run(shiftKey);
  }

  function setDefaults(q = '') {
    const ql = q.toLowerCase();
    const matched = ACTIONS.filter((a) => !ql || a.title.toLowerCase().includes(ql) || a.sub.toLowerCase().includes(ql) || a.kw.includes(ql));
    rows = matched.slice(0, ql ? 6 : 14).map((a) => ({ ...a, sec: a.tag === 'Action' ? 'Quick Actions' : 'Jump To' }));
    if (ql.length >= 2) {
      rows.push({ sec: 'Search', icon: '🔍', title: `Search everything for “${q}”`, sub: 'Open full faceted search results', tag: 'Enter', run: () => { location.hash = `#/search?q=${encodeURIComponent(q)}`; } });
    }
    selIdx = 0;
    paint();
  }

  async function queryLive(q) {
    if (ctrl) { try { ctrl.abort(); } catch {} }
    ctrl = new AbortController();
    const myCtrl = ctrl;
    try {
      const d = await api(`/api/search?q=${encodeURIComponent(q)}`, { signal: myCtrl.signal });
      if (myCtrl !== ctrl || host.hidden) return;
      const ql = q.toLowerCase();
      const out = [];
      for (const a of ACTIONS.filter((x) => x.title.toLowerCase().includes(ql) || x.kw.includes(ql)).slice(0, 3)) {
        out.push({ ...a, sec: a.tag === 'Action' ? 'Quick Actions' : 'Jump To' });
      }
      for (const ch of (d.live || []).slice(0, 5)) {
        out.push({
          sec: 'Live TV', img: ch.logo || '', contain: true, icon: '📡',
          title: ch.title, sub: ch.now ? `${ch.now.title} · until ${fmtClock(ch.now.e)}` : (ch.catName || 'Live channel'),
          tag: 'Live',
          run: () => playLive({ id: ch.id, title: ch.title, logo: ch.logo, epg: ch.epg }),
        });
      }
      for (const m of (d.movies || []).slice(0, 6)) {
        const sub = [m.year, m.rating ? `★ ${(+m.rating).toFixed(1)}` : '', m.uhd ? '4K' : ''].filter(Boolean).join(' · ');
        out.push({
          sec: 'Movies', img: m.poster || '', icon: '🎬',
          title: m.title, sub, tag: 'Movie',
          run: (shift) => (shift ? quickPlay(m) : openDetail(m)),
        });
      }
      for (const s of (d.series || []).slice(0, 6)) {
        const sub = [s.year, s.rating ? `★ ${(+s.rating).toFixed(1)}` : '', s.seasons ? `${s.seasons}S` : ''].filter(Boolean).join(' · ');
        out.push({
          sec: 'Series', img: s.poster || '', icon: '📺',
          title: s.title, sub, tag: 'Series',
          run: () => openDetail(s),
        });
      }
      out.push({ sec: 'Search', icon: '🔍', title: `View all results for “${q}”`, sub: 'Movies, series, documentaries, anime, people & live TV', tag: 'Search', run: () => { location.hash = `#/search?q=${encodeURIComponent(q)}`; } });
      rows = out; selIdx = 0; paint();
    } catch (e) {
      if (isAbort(e)) return;
    }
  }

  function open() {
    if (!host || !$('#player').hidden) return;
    if (!host.hidden) return;
    prevFocus = document.activeElement;
    Peek.hide();
    host.innerHTML = `<div class="cmdk-box" role="dialog" aria-modal="true" aria-label="Command palette">
      <div class="cmdk-head">
        <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="11" cy="11" r="7" fill="none" stroke="currentColor" stroke-width="2"/><path d="M20 20l-4-4" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>
        <input id="cmdkIn" type="search" autocomplete="off" placeholder="Search movies, series, live channels, or jump anywhere…">
        <kbd>ESC</kbd>
      </div>
      <div class="cmdk-list" id="cmdkList"></div>
      <div class="cmdk-foot">
        <span><kbd>↑</kbd><kbd>↓</kbd> Navigate</span>
        <span><kbd>Enter</kbd> Open</span>
        <span><kbd>Shift</kbd>+<kbd>Enter</kbd> Play movie</span>
        <span><kbd>Ctrl</kbd>+<kbd>K</kbd> Toggle</span>
      </div>
    </div>`;
    host.hidden = false;
    document.body.style.overflow = 'hidden';
    host.onclick = (e) => { if (e.target === host) close(); };
    const inp = $('#cmdkIn', host);
    setDefaults('');
    inp.oninput = () => {
      const q = inp.value.trim();
      clearTimeout(qTimer);
      if (q.length < 2) {
        if (ctrl) { try { ctrl.abort(); } catch {} ctrl = null; }
        setDefaults(q);
        return;
      }
      setDefaults(q);
      qTimer = setTimeout(() => queryLive(q), 130);
    };
    inp.onkeydown = (e) => {
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        if (rows.length) { selIdx = (selIdx + 1) % rows.length; paint(); }
      } else if (e.key === 'ArrowUp') {
        e.preventDefault();
        if (rows.length) { selIdx = (selIdx - 1 + rows.length) % rows.length; paint(); }
      } else if (e.key === 'Enter') {
        e.preventDefault();
        if (rows.length) runItem(selIdx, e.shiftKey);
      } else if (e.key === 'Escape') {
        e.preventDefault();
        close();
      }
    };
    setTimeout(() => inp?.focus(), 10);
  }

  function close() {
    if (!host || host.hidden) return;
    clearTimeout(qTimer);
    if (ctrl) { try { ctrl.abort(); } catch {} ctrl = null; }
    host.hidden = true; host.innerHTML = '';
    document.body.style.overflow = modal.hidden && $('#player').hidden ? '' : 'hidden';
    const prev = prevFocus; prevFocus = null;
    if (prev && prev.isConnected && $('#player').hidden) { try { prev.focus({ preventScroll: true }); } catch {} }
  }

  function toggle() { if (!host) return; if (host.hidden) open(); else close(); }
  return { open, close, toggle };
})();
window.addEventListener('popstate', () => {
  if (PLAYER_STATE.ignorePop) { PLAYER_STATE.ignorePop = false; return; }
  if (!$('#player').hidden && PLAYER_STATE.open) return Player.close();
  if (!modal.hidden) closeModal(true);
});
const PLAYER_STATE = { open: false };

async function openDetail(it) {
  if (!it) return;
  if (it.k === 'e') return resumeItem(it);
  if (it.k === 'x') return openExternal(it);
  if (it.k === 'c') return openCollection(it.id);
  openModal(`<div class="sheet"><div class="sheet-hero skel"></div><div style="padding:30px 40px"><div class="skel" style="height:18px;width:60%;border-radius:6px"></div></div></div>`);
  let d;
  try { d = await api(it.k === 's' ? `/api/series/${it.id}` : `/api/movie/${it.id}`); }
  catch (e) { closeModal(); return toast(`Couldn't load details: ${e.message}`); }
  if (modal.hidden) return;
  const listKey = `${d.k}:${d.id}`;
  const card = { k: d.k, id: d.id, tmdb: d.tmdb, title: d.title, year: d.year, poster: d.poster, backdrop: d.backdrop, rating: d.rating, uhd: d.uhd };
  const isSeries = d.kind === 'series';
  const nu = d.nextUp;
  const canResume = isSeries ? !!(nu && nu.resume > 30 && nu.progress < 0.94) : (d.resume > 30 && (!d.duration || d.resume / d.duration < 0.94));
  let playLabel = 'Play';
  if (isSeries && nu) playLabel = canResume ? `Resume S${nu.season}:E${nu.n}` : (nu.season === d.seasons[0]?.n && nu.n === d.seasons[0]?.episodes[0]?.n ? 'Play S1:E1' : `Play S${nu.season}:E${nu.n}`);
  if (!isSeries && canResume) playLabel = `Resume from ${fmtTime(d.resume)}`;
  const qPill = d.camOnly ? '<span class="pill cam">CAM Rip</span>'
    : d.qualityLabel === '4K DV' ? '<span class="pill dv">4K Dolby Vision</span>'
    : d.qualityLabel === '4K HDR' ? '<span class="pill dv">4K HDR</span>'
    : d.uhd ? '<span class="pill uhd">4K</span>'
    : d.qualityLabel === 'Blu-Ray' ? '<span class="pill br">Blu-Ray Master</span>' : '';
  const meta = [d.rating ? `<span class="star" title="${d.votes ? `${d.votes.toLocaleString()} TMDB votes` : ''}">★ ${(+d.rating).toFixed(1)}${d.votes ? ` <small class="muted">(${fmtVotes(d.votes)})</small>` : ''}</span>` : '', d.year ? `<span>${d.year}</span>` : '', d.cert ? `<span class="pill">${esc(d.cert)}</span>` : '',
    d.runtime ? `<span>${runtime(d.runtime)}</span>` : '', isSeries ? `<span>${d.seasons.length} Season${d.seasons.length === 1 ? '' : 's'}</span>` : '', qPill,
    d.isDoc ? `<span class="pill doc">Documentary${d.docTopics?.length ? ` · ${esc(d.docTopics.slice(0, 2).join(', '))}` : ''}</span>` : '',
    d.isAnime ? `<span class="pill dub">${d.dubbedCount ? `English Dub · ${d.dubbedCount} verified` : 'Anime · Auto-Skip Ready'}</span>` : ''].join('');
  const airBanner = isSeries && d.airingSeason ? (() => {
    const a = d.airingSeason;
    const latest = a.nextEp ? `Next: Ep ${a.nextEp}${a.nextTitle ? ` “${a.nextTitle}”` : ''}${a.nextDate ? ` (${a.nextDate})` : ''}`
      : a.lastEp ? `Latest: Ep ${a.lastEp}${a.lastTitle ? ` “${a.lastTitle}”` : ''}${a.lastDate ? ` (${a.lastDate})` : ''}` : '';
    const head = a.airedEps > 0 ? `${a.name} is airing now (${a.airedEps} eps aired)` : `${a.name} is airing now`;
    return `<div class="airing-banner"><span><b>${esc(head)}</b>${latest ? ` · ${esc(latest)}` : ''} — not yet added to English catalogue.</span>${!d.following ? '<button class="btn btn-accent btn-sm" id="dAirFollow">🔔 Follow for new episodes</button>' : '<span class="muted">✓ Following for alerts</span>'}</div>`;
  })() : '';

  openModal(`<div class="sheet">
    <button class="close" aria-label="Close">${I.close}</button>
    <div class="sheet-hero">${d.backdrop || d.poster ? `<img src="${esc(d.backdrop || d.poster)}" alt="">` : ''}
      <div class="hero-in"><h2>${esc(d.title)}</h2>
        <div class="btns"><button class="btn btn-primary" id="dPlay">${I.play}${esc(playLabel)}</button>
        ${canResume ? '<button class="btn btn-ghost" id="dStartOver">⟲ Start from beginning</button>' : ''}
        ${d.trailer ? `<button class="btn btn-ghost" id="dTrailer">${I.film}Trailer</button>` : ''}
        <button class="btn btn-round" id="dList" title="My List" aria-label="My List">${d.inList ? I.check : I.plus}</button>
        ${d.tmdb ? `<button class="btn btn-round ${d.myRating > 0 ? 'on' : ''}" id="dUp" title="I like this" aria-label="Like">${I.up}</button><button class="btn btn-round ${d.myRating < 0 ? 'on' : ''}" id="dDown" title="Not for me" aria-label="Not for me">${I.down}</button>` : ''}
        ${d.tmdb ? `<button class="btn btn-round ${d.watched ? 'on' : ''}" id="dWatched" title="${d.watched ? 'Watched' : 'Mark as watched'}" aria-label="Watched">${I.eye}</button>` : ''}
        ${isSeries ? `<button class="btn btn-round ${d.following ? 'on' : ''}" id="dFollow" title="${d.following ? 'Following: you get new-episode alerts' : 'Follow for new-episode alerts'}" aria-label="Follow">${d.following ? I.bellOn : I.bell}</button>` : ''}</div></div></div>
    <div class="sheet-body"><div>
        <div class="meta">${meta}</div>
        ${d.camOnly ? `<div class="cam-banner"><span><b>CAM / Telesync Rip:</b> Only a lower-quality theatre rip is currently available.</span>${d.tmdb ? '<button class="btn btn-accent btn-sm" id="dCamWatch">🔔 Notify when clean HD / 4K drops</button>' : ''}</div>` : ''}
        ${d.scores ? `<div class="scores">${d.scores.imdb ? `<span class="score imdb" title="${esc(d.scores.votes)} votes"><b>IMDb</b> ${esc(d.scores.imdb)}</span>` : ''}${d.scores.rt ? `<span class="score rt"><b>${parseInt(d.scores.rt) >= 60 ? '🍅' : '🤢'}</b> ${esc(d.scores.rt)}</span>` : ''}${d.scores.mc ? `<span class="score mc"><b>MC</b> ${esc(d.scores.mc)}</span>` : ''}${d.scores.awards && /won|win/i.test(d.scores.awards) ? `<span class="score aw">🏆 ${esc(d.scores.awards.split('.')[0])}</span>` : ''}</div>` : ''}
        ${d.tagline ? `<p class="tagline">${esc(d.tagline)}</p>` : ''}
        <p class="overview">${esc(d.overview || 'No description available.')}</p></div>
      <div class="facts">
        ${d.castIds?.length ? `<div><b>Cast:</b> ${d.castIds.slice(0, 8).map((c) => `<a class="cast-link" href="#/person/${c.id}">${esc(c.name)}</a>`).join(', ')}</div>` : d.cast?.length ? `<div><b>Cast:</b> ${esc(d.cast.join(', '))}</div>` : ''}
        ${d.genres?.length ? `<div><b>Genres:</b> ${esc(d.genres.join(', '))}</div>` : ''}
        ${d.director ? `<div><b>Director:</b> ${esc(d.director)}</div>` : ''}
        ${d.docTopics?.length ? `<div><b>Topics:</b> ${esc(d.docTopics.join(', '))}</div>` : ''}
        ${d.platforms?.length ? `<div><b>Available on:</b> ${esc(d.platforms.join(', '))}</div>` : ''}
        ${(d.countries?.length || d.lang) ? `<div><b>Origin:</b> ${esc([...(d.countries || []).map(countryName), d.lang ? (langName(d.lang) || d.lang.toUpperCase()) : ''].filter(Boolean).join(' · '))}</div>` : ''}
        ${d.network ? `<div><b>Network:</b> ${esc(d.network)}${d.status ? ` · ${esc(d.status)}` : ''}</div>` : ''}
        ${!isSeries && d.variants?.length > 1 ? `<div><b>Versions:</b><div class="versions">${d.variants.map((v, i) => `<button class="chip ${i ? '' : 'on'}" data-variant="${v.id}" title="${esc(v.cat)}">${esc(v.label)}</button>`).join('')}</div></div>` : ''}
      </div></div>
    ${isSeries ? `<div class="sheet-section"><h3><span>Episodes <button class="btn btn-ghost btn-sm" id="seasonWatched" style="margin-left:10px">Mark season watched</button></span> ${d.seasons.length > 1 ? `<select class="sel" id="seasonSel">${d.seasons.map((s) => `<option value="${s.n}">${esc(s.name)}</option>`).join('')}</select>` : ''}</h3>${airBanner}<div class="episodes" id="eps"></div></div>` : ''}
    ${d.similar?.length ? `<div class="sheet-section"><h3>More Like This</h3><div class="more-grid">${d.similar.slice(0, 12).map(poster).join('')}</div></div>` : ''}
  </div>`);

  let variant = d.best ?? d.id;
  $$('[data-variant]', modal).forEach((b) => (b.onclick = () => { variant = +b.dataset.variant; $$('[data-variant]', modal).forEach((x) => x.classList.toggle('on', x === b)); }));
  if ($('#dCamWatch')) $('#dCamWatch').onclick = async () => {
    await post('/api/wanted', { mt: isSeries ? 'tv' : 'movie', tmdb: d.tmdb, on: true, title: d.title, poster: d.poster || '', year: d.year || null }).catch(() => {});
    $('#dCamWatch').textContent = '✓ Watching for HD / 4K upgrade'; $('#dCamWatch').disabled = true;
    toast(`We'll alert you the moment ${d.title} upgrades to clean HD or 4K`);
  };
  $('#dList').onclick = async () => {
    d.inList = !d.inList; await post('/api/list', { key: listKey, on: d.inList, item: card });
    invalidateCache('/api/mylist', '/api/home');
    $('#dList').innerHTML = d.inList ? I.check : I.plus; toast(d.inList ? 'Added to My List' : 'Removed from My List');
  };
  const rateBtn = (v) => async () => {
    const nv = d.myRating === v ? 0 : v; await rateItem(card, nv); d.myRating = nv;
    $('#dUp').classList.toggle('on', nv > 0); $('#dDown').classList.toggle('on', nv < 0);
    if (nv < 0) closeModal();
  };
  if ($('#dUp')) { $('#dUp').onclick = rateBtn(1); $('#dDown').onclick = rateBtn(-1); }
  if ($('#dWatched')) $('#dWatched').onclick = async () => {
    d.watched = !d.watched; await post('/api/watched', { mt: isSeries ? 'tv' : 'movie', tmdb: d.tmdb, on: d.watched, title: d.title });
    invalidateCache('/api/home', '/api/movies', '/api/series', '/api/anime', '/api/docs', '/api/showcase', '/api/marathons');
    $('#dWatched').classList.toggle('on', d.watched); toast(d.watched ? 'Marked as watched. It won\u2019t show up in discovery rows' : 'Marked as unwatched');
  };
  if ($('#dFollow')) $('#dFollow').onclick = async () => {
    d.following = !d.following; await post('/api/follow', { sid: d.id, on: d.following, item: card });
    $('#dFollow').classList.toggle('on', d.following); $('#dFollow').innerHTML = d.following ? I.bellOn : I.bell;
    if ($('#dAirFollow')) { $('#dAirFollow').textContent = d.following ? '✓ Following' : '🔔 Follow for new episodes'; $('#dAirFollow').disabled = d.following; }
    toast(d.following ? `Following ${d.title}. You'll get an alert when new episodes land` : 'Stopped following');
  };
  if ($('#dAirFollow')) $('#dAirFollow').onclick = () => $('#dFollow')?.click();
  // Muted trailer plays in the header after a moment (Trailer button = with sound).
  clearTimeout(detailTrailerT);
  if (d.trailer) detailTrailerT = setTimeout(() => {
    const h = $('.sheet-hero', modal); if (!h || $('.trailer-frame', h) || !$('#player').hidden) return;
    const f = document.createElement('div'); f.className = 'trailer-frame muted';
    f.innerHTML = `<iframe src="https://www.youtube-nocookie.com/embed/${encodeURIComponent(d.trailer)}?autoplay=1&mute=1&controls=0&rel=0&modestbranding=1&iv_load_policy=3&start=3" allow="autoplay" tabindex="-1"></iframe>`;
    h.insertBefore(f, $('.hero-in', h)); setTimeout(() => f.classList.add('on'), 1500);
  }, 1800);
  if (d.trailer) $('#dTrailer').onclick = () => {
    $('.trailer-frame.muted', modal)?.remove();
    const h = $('.sheet-hero', modal); const f = document.createElement('div'); f.className = 'trailer-frame';
    f.innerHTML = `<iframe src="https://www.youtube-nocookie.com/embed/${encodeURIComponent(d.trailer)}?autoplay=1&rel=0" allow="autoplay; fullscreen" allowfullscreen></iframe>`;
    h.appendChild(f);
  };

  if (isSeries) {
    const flat = d.seasons.flatMap((s) => s.episodes.map((e) => ({ ...e, season: s.n })));
    let showSubOnly = false;
    let skipFiller = !!d.skipFiller;
    const drawEps = (sn) => {
      const s = d.seasons.find((x) => x.n === +sn) || d.seasons[0]; if (!s) { $('#eps').innerHTML = '<div class="empty">No episodes available.</div>'; return; }
      const dubbedEps = d.isAnime ? s.episodes.filter((e) => e.dub !== 'no') : s.episodes;
      const subOnlyEps = d.isAnime ? s.episodes.filter((e) => e.dub === 'no') : [];
      const shown = (showSubOnly ? s.episodes : dubbedEps).filter((e) => !(d.isAnime && skipFiller && e.filler === 'filler'));
      const rowHtml = (e) => `<div class="ep ${e.dub === 'no' ? 'subonly' : ''} ${e.filler === 'filler' ? 'is-filler' : ''}" data-ep="${e.id}" tabindex="0" role="button">
        <div class="n">${e.n}</div>
        <div class="still">${e.still || d.backdrop ? `<img src="${esc(e.still || d.backdrop)}" alt="" loading="lazy" onerror="this.remove()">` : ''}${e.progress ? `<div class="bar"><i style="width:${Math.round(e.progress * 100)}%"></i></div>` : ''}</div>
        <div><div class="t"><span>${e.isNew ? '<span class="new-badge inline">NEW</span> ' : ''}${d.isAnime && e.dub === 'yes' ? '<span class="badge-dub inline">DUB</span>' : ''}${d.isAnime && e.dub === 'no' ? '<span class="pill" style="margin-right:6px">Sub only</span>' : ''}${e.filler === 'filler' ? '<span class="badge-filler inline">FILLER</span>' : e.filler === 'mixed' ? '<span class="badge-mixed inline">MIXED</span>' : ''}${esc(e.title)}</span><small>${[e.aired ? e.aired.slice(0, 10) : '', e.duration ? runtime(Math.round(e.duration / 60)) : ''].filter(Boolean).join(' · ')}</small></div>
        <p>${esc(e.plot)}</p>
        ${d.isAnime && e.dub === 'no' ? `<div style="margin-top:6px"><button class="btn btn-ghost btn-sm ${e.dubWatching ? 'on' : ''}" data-dub-watch="${e.id}" data-sn="${s.n}">${e.dubWatching ? '🔔 Watching for English dub' : '🔔 Notify when dubbed'}</button></div>` : ''}</div></div>`;
      const unknownCnt = d.isAnime ? s.episodes.filter((e) => !e.dub || e.dub === 'unknown').length : 0;
      const fc = d.fillerSummary?.fillerCount || 0;
      const mc = d.fillerSummary?.mixedCount || 0;
      const fillerSummary = d.isAnime && fc > 0
        ? `<div class="subonly-toggle"><span>Filler guide: <b>${fc}</b> filler eps (${Math.round(d.fillerSummary.fillerPct || 0)}%)${mc ? ` · ${mc} mixed` : ''}</span><label class="tog" style="margin-left:auto"><input type="checkbox" id="optSkipFiller" ${skipFiller ? 'checked' : ''}> Skip filler episodes</label></div>`
        : '';
      const footerParts = [];
      if (subOnlyEps.length) footerParts.push(`<span>${subOnlyEps.length} episode${subOnlyEps.length === 1 ? '' : 's'} hidden (no English dub)</span><button class="btn btn-ghost btn-sm" id="toggleSubOnly">${showSubOnly ? 'Hide sub-only' : 'Show anyway'}</button>`);
      if (d.isAnime && unknownCnt > 0) footerParts.push(`<button class="btn btn-ghost btn-sm" id="checkSeasonDubs" style="margin-left:auto" title="Checks a few episodes when nothing is streaming">Verify English dubs (${unknownCnt} unchecked)</button>`);
      const footer = footerParts.length ? `<div class="subonly-toggle">${footerParts.join('')}</div>` : '';
      $('#eps').innerHTML = fillerSummary + (shown.length ? shown.map(rowHtml).join('') : '<div class="empty">No English-dubbed canon episodes available in this season yet.</div>') + footer;
      $$('[data-dub-watch]', modal).forEach((btn) => (btn.onclick = async (ev) => {
        ev.stopPropagation();
        const ep = flat.find((x) => x.id === +btn.dataset.dubWatch); if (!ep) return;
        const on = !ep.dubWatching;
        await post('/api/anime/dub-watch', {
          key: `e:${ep.id}`, kind: 'series', id: ep.id, sid: d.id, season: ep.season, epNum: ep.n,
          ext: ep.ext, title: d.title, sub: `S${ep.season}:E${ep.n} · ${ep.title}`, on,
        }).catch((err) => toast(err.message));
        ep.dubWatching = on;
        btn.classList.toggle('on', on);
        btn.textContent = on ? '🔔 Watching for English dub' : '🔔 Notify when dubbed';
        toast(on ? `We'll alert you the moment ${d.title} S${ep.season}:E${ep.n} gets an English dub` : 'Removed dub alert');
      }));
      if ($('#optSkipFiller')) $('#optSkipFiller').onchange = (ev) => {
        skipFiller = ev.target.checked; d.skipFiller = skipFiller;
        post('/api/prefs', { skipFiller }).catch(() => {});
        toast(skipFiller ? 'Skipping anime filler episodes' : 'Showing all episodes including filler');
        drawEps(sn);
      };
      $$('[data-ep]', modal).forEach((el) => (el.onclick = () => {
        const ep = flat.find((x) => x.id === +el.dataset.ep);
        playEpisode(d, flat, ep, ep.progress < 0.94 ? ep.resume : 0, { allowSubOnly: ep.dub === 'no' });
      }));
      if ($('#toggleSubOnly')) $('#toggleSubOnly').onclick = () => { showSubOnly = !showSubOnly; drawEps(sn); };
      if ($('#checkSeasonDubs')) $('#checkSeasonDubs').onclick = async (ev) => {
        const btn = ev.currentTarget; btn.disabled = true; btn.textContent = 'Checking dubs…';
        try {
          const r = await post(`/api/anime/check-series/${d.id}`, { season: s.n });
          if (r.checked > 0) {
            toast(`Checked ${r.checked} ep${r.checked === 1 ? '' : 's'}: ${r.dubbed} English dub, ${r.subOnly} sub-only`);
            return openDetail({ k: 's', id: d.id, title: d.title });
          }
          toast('Season dubs already checked');
          btn.remove();
        } catch (err) {
          toast(err.message || 'Could not check dubs right now');
          btn.disabled = false; btn.textContent = `Verify English dubs (${unknownCnt} unchecked)`;
        }
      };
    };
    const startSeason = nu?.season ?? d.seasons[0]?.n;
    $('#seasonWatched').onclick = async () => {
      const sn = +($('#seasonSel')?.value ?? startSeason); const s = d.seasons.find((x) => x.n === sn); if (!s) return;
      const allDone = s.episodes.every((e) => e.progress >= 0.94);
      await post('/api/episodes-watched', {
        ids: s.episodes.map((e) => e.id),
        episodes: s.episodes.map((e) => e.n),
        season: sn,
        tmdb: d.tmdb,
        on: !allDone,
        item: { k: 'e', sid: d.id, title: d.title, poster: d.poster, backdrop: d.backdrop },
      });
      s.episodes.forEach((e) => (e.progress = allDone ? 0 : 1)); drawEps(sn);
      toast(allDone ? `${s.name} marked unwatched` : `${s.name} marked watched`);
    };
    if ($('#seasonSel')) { $('#seasonSel').value = startSeason; $('#seasonSel').onchange = (e) => drawEps(e.target.value); }
    drawEps(startSeason);
    $('#dPlay').onclick = () => nu ? playEpisode(d, flat, nu, nu.progress < 0.94 ? nu.resume : 0) : toast('No episodes available');
    if ($('#dStartOver')) $('#dStartOver').onclick = () => nu ? playEpisode(d, flat, nu, 0) : toast('No episodes available');
  } else {
    $('#dPlay').onclick = () => playMovie(d, variant, canResume ? d.resume : 0);
    if ($('#dStartOver')) $('#dStartOver').onclick = () => playMovie(d, variant, 0);
  }
}

async function quickPlay(it) {
  if (it.k === 's') return openDetail(it);
  try { const d = await api(`/api/movie/${it.id}`); playMovie(d, d.best ?? d.id, d.resume); } catch (e) { toast(e.message); }
}
async function resumeItem(it) {
  if (it.k === 'm') { const d = await api(`/api/movie/${it.id}`); return playMovie(d, d.best ?? d.id, it.resume ?? d.resume); }
  if (it.k === 'e') {
    const d = await api(`/api/series/${it.sid}`);
    const flat = d.seasons.flatMap((s) => s.episodes.map((e) => ({ ...e, season: s.n })));
    const ep = flat.find((x) => x.id === it.id); if (!ep) return openDetail({ k: 's', id: it.sid });
    return playEpisode(d, flat, ep, it.resume ?? ep.resume);
  }
  if (it.k === 'cu') {
    const feeds = it.feeds || [];
    return Player.open({
      kind: 'catchup', id: it.id, title: it.title, sub: it.sub || '',
      catchup: { cs: it.cs, dur: it.dur }, resume: it.resume || 0, duration: it.duration || (it.dur * 60),
      progressKey: it.key || `cu:${it.rowId || it.id}:${it.cs}`,
      alternates: feeds.filter((f) => f.id !== it.id),
      item: it,
    });
  }
  openDetail(it);
}

// ---------------------------------------------------------------- playback entry points
function playMovie(d, id, resume) {
  closeModal(true); // the player takes over the detail sheet's history entry (no history.back race)
  const v = (d.variants || []).find((x) => x.id === id) || {};
  Player.open({
    kind: 'movie', id, ext: v.ext || '', title: d.title, sub: [d.year, d.runtime ? runtime(d.runtime) : ''].filter(Boolean).join(' · '),
    resume: resume || 0, duration: d.duration || 0, isAnime: !!d.isAnime,
    progressKey: `m:${d.id}`, tmdb: d.tmdb, imdb: d.imdb || '', extSub: d.extSub || null, historyKind: 'm', historyTitle: d.title,
    item: { k: 'm', id: d.id, title: d.title, year: d.year, poster: d.poster, backdrop: d.backdrop },
    label: v.label, cat: v.cat || '', catId: v.catId || '', subMarket: !!v.subMarket,
    alternates: (d.variants || []).filter((x) => x.id !== id),
  });
}
function playEpisode(d, flat, ep, resume, extra = {}) {
  closeModal(true);
  const idx = flat.findIndex((x) => x.id === ep.id);
  const nxt = flat.slice(idx + 1).find((x) => (!d.isAnime || x.dub !== 'no') && (!d.isAnime || !d.skipFiller || x.filler !== 'filler')) || null;
  const prv = idx > 0 ? [...flat.slice(0, idx)].reverse().find((x) => (!d.isAnime || x.dub !== 'no') && (!d.isAnime || !d.skipFiller || x.filler !== 'filler')) || null : null;
  Player.open({
    kind: 'series', id: ep.id, ext: ep.ext, sid: d.id, season: ep.season, epn: ep.n, rawN: ep.rawN ?? ep.n, abs: ep.abs || 0, title: d.title, sub: `S${ep.season}:E${ep.n} · ${ep.title}`,
    resume: resume || 0, duration: ep.duration || 0, isAnime: !!d.isAnime, allowSubOnly: !!extra.allowSubOnly,
    progressKey: `e:${ep.id}`, tmdb: d.tmdb, imdb: d.imdb || '', extSub: ep.extSub || null, historyKind: 's', historyTitle: d.title,
    item: { k: 'e', id: ep.id, sid: d.id, title: d.title, sub: `S${ep.season}:E${ep.n} · ${ep.title}`, backdrop: ep.still || d.backdrop, poster: d.poster },
    prev: prv ? { label: `S${prv.season}:E${prv.n} · ${prv.title}`, go: () => playEpisode(d, flat, prv, 0) } : null,
    next: nxt ? { id: nxt.id, ext: nxt.ext || '', sid: d.id, season: nxt.season, epn: nxt.n, rawN: nxt.rawN ?? nxt.n, label: `S${nxt.season}:E${nxt.n} · ${nxt.title}`, still: nxt.still, go: () => playEpisode(d, flat, nxt, nxt.progress < 0.94 ? nxt.resume : 0) } : null,
  });
}
let LAST_LIVE = null;
let CUR_LIVE = null;
try { LAST_LIVE = JSON.parse(sessionStorage.getItem('tv.lastLive') || 'null'); } catch {}
function rememberLiveChannel(it) {
  if (!it?.id) return;
  const entry = { id: +it.id, title: it.title || '', epg: it.epg || '', logo: it.logo || '', sub: it.sub || '' };
  if (CUR_LIVE && CUR_LIVE.id !== entry.id) {
    LAST_LIVE = CUR_LIVE;
    try { sessionStorage.setItem('tv.lastLive', JSON.stringify(LAST_LIVE)); } catch {}
  }
  CUR_LIVE = entry;
}
function playLive(it) {
  if (!it?.id) return;
  rememberLiveChannel(it);
  const list = CHANNEL_CONTEXT.length && CHANNEL_CONTEXT.some((c) => c.id === it.id) ? CHANNEL_CONTEXT : [it];
  const feeds = (it.feeds || []).filter((f) => f.id !== it.id).map((f) => ({ id: f.id, label: f.label || f.channel, channel: f.channel || f.label }));
  Player.open({ kind: 'live', id: +it.id, title: it.title || 'Live', sub: it.sub || '', logo: it.logo || '', live: true, epg: it.epg || '', channels: list,
    label: (it.feeds || []).find((f) => f.id === it.id)?.label || 'Main', alternates: feeds });
}

// ---------------------------------------------------------------- player
let SKIPS = [];
const AUTOPLAY = { chain: 0 }; // episodes auto-played in a row without the viewer touching anything
['keydown', 'click', 'touchstart'].forEach((ev) => document.addEventListener(ev, (e) => { if (e.isTrusted) AUTOPLAY.chain = 0; }, true));
// does the sound output have surround channels? (stereo outputs get ffmpeg's stereo mix instead of 5.1)
let SURROUND_OUT = false;
try { const c = new AudioContext(); SURROUND_OUT = c.destination.maxChannelCount >= 6; c.close(); } catch {}
// this window's id: the server only lets a window stop its own stream, and asks before taking another window's
const PAGE_ID = Math.random().toString(36).slice(2, 10);
const Player = (() => {
  const el = $('#player');
  let o = null; let video; let offset = 0; let duration = 0; let audio = 0; let probe = null;
  let saveTimer; let idleTimer; let stallTimer; let subTimer; let started = false; let historySent = false; let nextShown = false;
  // Fallback chain: each attempt = {id, tx, alt}. On failure we move to the next one.
  let attempts = []; let ai = 0;
  // source health: startup time, stalls, watched time per stream id (feeds best-version ranking)
  let loadT = 0; let reportedStart = false; let stallAt = 0; let stalls = []; let stallTimer2; let watchedAt = 0; let watchedSecs = 0; let credits = null; let creditsEnd = null; let creditsMixed = false; let hasPostCredits = false; let skipped = new Set(); let userDismissed = new Set(); let creditsDismissed = false; let lastAutoSkip = null;
  let subSel = -1; let subTrack = null; let subSeen = 0;
  // resume-in-place budget (network drops), one failure report per version per title, load generation
  let retries = 0; let retryAt = 0; let lastFreeze = 0; let failed = new Set(); let loadGen = 0; let openGen = 0;
  let hitches = 0; let lastQ = null;
  // picture: display matching, stats overlay, OLED care, pro player
  let pro = null; let proPoll; let statsTimer; let shiftTimer; let saverTimer; let lastBytes = null;
  // live TV: TS ring buffer pause/rewind + now/next banner
  let liveBack = 0; let liveBufSecs = 0; let liveBehind = 0; let livePausedAt = 0; let livePollTimer; let liveEpgTimer; let liveBannerTimer;
  // MSE engine state (VOD fast seeks + seamless version switching)
  let mseActive = false; let mseTarget = 0; let mseSessionStart = 0; let mseFailed = false;
  const mseEnabled = () => typeof MediaSource !== 'undefined' && localGet('mse', '1') === '1' && !mseFailed;
  // Web player UX state: speed, aspect/zoom, subtitle size, HUDs, drag-scrub, instant seek, wheel volume
  const SPEEDS = [0.5, 0.75, 1, 1.25, 1.5, 1.75, 2];
  const ZOOMS = [['fit', 'Fit'], ['fill', 'Fill (Crop)'], ['zoom', 'Zoom 110%']];
  const SUB_SIZES = [['sm', 'Small'], ['md', 'Normal'], ['lg', 'Large']];
  let playRate = 1; let zoomMode = 'fit'; let subSize = localGet('subsize', 'md');
  let showRem = localGet('timeRem', '0') === '1';
  let draggingSeek = false; let hoveringCtrl = false;
  let pendingSeekTarget = null; let pendingSeekTimer = null; let pendingLiveBack = null;
  let hudSeekDir = 0; let hudSeekAccum = 0; let hudSeekTimer = null;
  let osdTimer = null; let volOsdTimer = null; let clickTimer = null;
  let wheelAcc = 0; let wheelResetTimer = null; let lastPosStateAt = 0;
  let wakeControls = () => {};
  let prespoolSent = false;
  let dvrRec = null; let dvrTimer = null;
  let aiSubBusy = false; let aiSubPoll = null;
  let castVolTimer = null;

  const focusPlayer = () => { if (!el.hidden) { try { el.focus({ preventScroll: true }); } catch {} } };
  const volIcon = (v, muted) => (muted || v <= 0.001 ? I.mute : v < 0.5 ? I.volLow : I.vol);
  function showOsd(text, iconHtml = '') {
    const box = $('.p-osd-center', el); if (!box) return;
    box.innerHTML = (iconHtml ? `<span class="osd-ico">${iconHtml}</span>` : '') + (text ? `<span>${esc(text)}</span>` : '');
    box.classList.toggle('icon-only', !text && !!iconHtml);
    box.classList.remove('on'); void box.offsetWidth; box.classList.add('on');
    clearTimeout(osdTimer);
    osdTimer = setTimeout(() => box.classList.remove('on'), text ? 1050 : 650);
  }
  function showVolOsd() {
    const box = $('.p-osd-vol', el); if (!box || (!video && !casting)) return;
    const rawV = casting ? (casting.vol ?? 1) : video.volume;
    const isMuted = casting ? !!casting.muted : video.muted;
    const muted = isMuted || rawV <= 0.001;
    const pct = muted ? 0 : Math.round(rawV * 100);
    box.innerHTML = `<span class="osd-ico">${volIcon(rawV, isMuted)}</span><div class="osd-vbar"><i style="width:${pct}%"></i></div><span class="osd-vpct">${muted ? 'Muted' : `${pct}%`}</span>`;
    box.classList.add('on');
    clearTimeout(volOsdTimer);
    volOsdTimer = setTimeout(() => box.classList.remove('on'), 1150);
  }
  function showSeekOsd(dir, accumSecs, targetSec, customSub) {
    const side = dir < 0 ? 'left' : 'right';
    const other = dir < 0 ? 'right' : 'left';
    const box = $(`.p-osd-seek.${side}`, el); if (!box) return;
    $(`.p-osd-seek.${other}`, el)?.classList.remove('on');
    const abs = Math.round(Math.abs(accumSecs));
    const sub = o?.spoilerFree ? '' : customSub ? customSub : (targetSec != null && duration ? `${fmtTime(targetSec)} / ${fmtTime(duration)}` : '');
    box.innerHTML = `<div class="osd-sval">${dir < 0 ? `« −${abs}s` : `+${abs}s »`}</div>${sub ? `<div class="osd-ssub">${esc(sub)}</div>` : ''}`;
    box.classList.remove('on'); void box.offsetWidth; box.classList.add('on');
    clearTimeout(box._t);
    box._t = setTimeout(() => box.classList.remove('on'), 850);
  }
  function syncVolUi() {
    if (!video && !casting) return;
    const rawV = casting ? (casting.vol ?? 1) : video.volume;
    const isMuted = casting ? !!casting.muted : video.muted;
    const eff = isMuted ? 0 : rawV;
    const mb = $('#pMute', el); if (mb) mb.innerHTML = volIcon(rawV, isMuted);
    const sl = $('#pVol', el);
    if (sl) {
      sl.value = String(Math.round(eff * 100) / 100);
      sl.style.setProperty('--vol-pct', `${Math.round(eff * 100)}%`);
    }
  }
  function sendCastVol(v, immediate = false) {
    clearTimeout(castVolTimer);
    if (immediate) { castCtl('volume', { v }); return; }
    castVolTimer = setTimeout(() => { if (casting) castCtl('volume', { v }); }, 140);
  }
  function setVolume(v, { hud = false, unmute = true, immediate = false } = {}) {
    const clamped = Math.max(0, Math.min(1, Math.round(v * 100) / 100));
    if (casting) {
      casting.vol = clamped;
      casting.muted = clamped <= 0.001 ? true : (unmute ? false : !!casting.muted);
      if (clamped > 0) casting.lastVol = clamped;
      syncVolUi();
      if (hud) showVolOsd();
      sendCastVol(casting.muted ? 0 : clamped, immediate || hud);
      return;
    }
    if (!video) return;
    video.volume = clamped;
    if (clamped > 0 && unmute) video.muted = false;
    else if (clamped <= 0.001) video.muted = true;
    localSet('vol', String(clamped));
    localSet('muted', video.muted ? '1' : '0');
    if (clamped > 0) localSet('volLast', String(clamped));
    syncVolUi();
    if (hud) showVolOsd();
  }
  function stepVolume(delta) {
    if (!video && !casting) return;
    const curV = casting ? (casting.muted ? 0 : (casting.vol ?? 1)) : (video.muted ? 0 : video.volume);
    const next = Math.max(0, Math.min(1, Math.round((curV + delta) * 20) / 20));
    if (next > 0 && !casting && video) video.muted = false;
    setVolume(next, { hud: true, unmute: next > 0, immediate: true });
  }
  function toggleMute(hud = true) {
    if (casting) {
      if (casting.muted || (casting.vol ?? 1) <= 0.001) {
        casting.muted = false;
        if ((casting.vol ?? 0) <= 0.001) casting.vol = casting.lastVol || 0.5;
      } else {
        casting.lastVol = casting.vol ?? 0.5;
        casting.muted = true;
      }
      syncVolUi();
      if (hud) showVolOsd();
      sendCastVol(casting.muted ? 0 : casting.vol, true);
      return;
    }
    if (!video) return;
    if (video.muted || video.volume <= 0.001) {
      video.muted = false;
      if (video.volume <= 0.001) {
        video.volume = Math.max(0.25, +localGet('volLast', 0.5) || 0.5);
        localSet('vol', String(video.volume));
      }
    } else {
      localSet('volLast', String(video.volume));
      video.muted = true;
    }
    localSet('muted', video.muted ? '1' : '0');
    syncVolUi();
    if (hud) showVolOsd();
  }
  function setSpeed(r, hud = true) {
    if (!o || o.live || !video) return;
    playRate = SPEEDS.reduce((b, x) => (Math.abs(x - r) < Math.abs(b - r) ? x : b), 1);
    try { video.playbackRate = playRate; } catch {}
    const btn = $('#pTune', el);
    if (btn) {
      btn.classList.toggle('on', playRate !== 1 || zoomMode !== 'fit');
      const lb = $('.tune-lbl', btn); if (lb) lb.textContent = playRate === 1 ? '1×' : `${playRate}×`;
    }
    $$('.p-menu [data-spd]', el).forEach((b) => b.classList.toggle('on', +b.dataset.spd === playRate));
    if (hud) showOsd(`Speed ${playRate}×`);
    tick();
  }
  function stepSpeed(dir) {
    if (!o || o.live) return;
    const i = SPEEDS.indexOf(playRate);
    setSpeed(SPEEDS[Math.max(0, Math.min(SPEEDS.length - 1, (i < 0 ? 2 : i) + dir))], true);
  }
  function setZoom(mode, hud = true) {
    zoomMode = mode;
    el.classList.toggle('zoom-fill', mode === 'fill');
    el.classList.toggle('zoom-110', mode === 'zoom');
    const btn = $('#pTune', el);
    if (btn) btn.classList.toggle('on', (!o?.live && playRate !== 1) || zoomMode !== 'fit');
    $$('.p-menu [data-zm]', el).forEach((b) => b.classList.toggle('on', b.dataset.zm === mode));
    const lbl = ZOOMS.find((z) => z[0] === mode)?.[1] || 'Fit';
    if (hud) showOsd(`Picture: ${lbl}`);
  }
  function cycleZoom() {
    const i = ZOOMS.findIndex((z) => z[0] === zoomMode);
    setZoom(ZOOMS[(i + 1) % ZOOMS.length][0], true);
  }
  function setSubSize(sz, hud = true) {
    subSize = sz;
    localSet('subsize', sz);
    el.classList.toggle('sub-sm', sz === 'sm');
    el.classList.toggle('sub-lg', sz === 'lg');
    $$('.p-menu [data-ssz]', el).forEach((b) => b.classList.toggle('on', b.dataset.ssz === sz));
    const lbl = SUB_SIZES.find((s) => s[0] === sz)?.[1] || 'Normal';
    if (hud) showOsd(`Subtitles: ${lbl}`);
  }
  async function togglePip() {
    if (!video || !document.pictureInPictureEnabled) return showOsd('Picture-in-Picture unavailable');
    try {
      if (document.pictureInPictureElement) await document.exitPictureInPicture();
      else await video.requestPictureInPicture();
    } catch (e) { showOsd(e?.message || 'Picture-in-Picture failed'); }
  }
  function hasOpenPopover() {
    return !!$('.p-menu, .cast-menu, .extsub-menu, .dvr-menu, .p-help', el) || !!($('#pDrawer', el) && !$('#pDrawer', el).hidden);
  }
  function closePopovers() {
    let closed = false;
    $$('.p-menu, .cast-menu, .extsub-menu, .dvr-menu, .p-help', el).forEach((n) => { n.remove(); closed = true; });
    const dr = $('#pDrawer', el);
    if (dr && !dr.hidden) { toggleMiniGuide(false); closed = true; }
    if (closed) { focusPlayer(); wakeControls(); }
    return closed;
  }
  function toggleSettingsMenu(force) {
    const old = $('.p-menu', el);
    if (old && force !== true) { old.remove(); focusPlayer(); wakeControls(); return; }
    if (old) return;
    closePopovers();
    const isSeries = !o?.live && o?.kind === 'series';
    const asIntro = localGet('autoskip', '1') === '1';
    const asRecap = localGet('autoSkipRecap', '1') === '1';
    const asEnd = localGet('autoSkipCredits', '1') === '1';
    const m = document.createElement('div'); m.className = 'cast-menu p-menu';
    m.innerHTML = `${!o?.live ? `<div class="pm-sec"><div class="pm-lbl">Playback speed <span>&lt; / &gt;</span></div><div class="pm-pills">${SPEEDS.map((s) => `<button type="button" data-spd="${s}" class="${s === playRate ? 'on' : ''}">${s}×</button>`).join('')}</div></div>` : ''}
      <div class="pm-sec"><div class="pm-lbl">Picture fit <span>Z</span></div><div class="pm-pills">${ZOOMS.map(([k, l]) => `<button type="button" data-zm="${k}" class="${k === zoomMode ? 'on' : ''}">${l}</button>`).join('')}</div></div>
      ${!o?.live ? `<div class="pm-sec"><div class="pm-lbl">Subtitle size</div><div class="pm-pills">${SUB_SIZES.map(([k, l]) => `<button type="button" data-ssz="${k}" class="${k === subSize ? 'on' : ''}">${l}</button>`).join('')}</div></div>` : ''}
      ${isSeries ? `<div class="pm-sec"><div class="pm-lbl">Auto-skip <span>S / U undo</span></div><div class="pm-pills">
        <button type="button" data-as="autoskip" class="${asIntro ? 'on' : ''}">${o?.isAnime ? 'Opening' : 'Intro'}</button>
        <button type="button" data-as="autoSkipRecap" class="${asRecap ? 'on' : ''}">Recap</button>
        <button type="button" data-as="autoSkipCredits" class="${asEnd ? 'on' : ''}">${o?.isAnime ? 'Ending' : 'Credits'}</button>
      </div></div>` : ''}
      <button type="button" class="pm-link" id="pmHelp"><span>Keyboard shortcuts</span><kbd>?</kbd></button>`;
    el.appendChild(m);
    $$('[data-spd]', m).forEach((b) => (b.onclick = () => { setSpeed(+b.dataset.spd, true); focusPlayer(); }));
    $$('[data-zm]', m).forEach((b) => (b.onclick = () => { setZoom(b.dataset.zm, true); focusPlayer(); }));
    $$('[data-ssz]', m).forEach((b) => (b.onclick = () => { setSubSize(b.dataset.ssz, true); focusPlayer(); }));
    $$('[data-as]', m).forEach((b) => (b.onclick = () => {
      const key = b.dataset.as;
      const next = localGet(key, '1') !== '1';
      localSet(key, next ? '1' : '0');
      b.classList.toggle('on', next);
      post('/api/prefs', { [key]: next }).catch(() => {});
      const lbl = key === 'autoskip' ? (o?.isAnime ? 'Opening' : 'Intro') : key === 'autoSkipRecap' ? 'Recap' : (o?.isAnime ? 'Ending' : 'Credits');
      showOsd(`Auto-skip ${lbl}: ${next ? 'On' : 'Off'}`);
      focusPlayer();
    }));
    $('#pmHelp', m).onclick = () => { m.remove(); toggleShortcutsHelp(true); };
    wakeControls();
    setTimeout(() => document.addEventListener('click', function off(e) {
      if (!m.isConnected) return document.removeEventListener('click', off);
      if (!m.contains(e.target) && !e.target.closest('#pTune')) { m.remove(); document.removeEventListener('click', off); wakeControls(); }
    }), 0);
  }
  function toggleShortcutsHelp(force) {
    const old = $('.p-help', el);
    if (old && force !== true) { old.remove(); focusPlayer(); wakeControls(); return; }
    if (old) return;
    closePopovers();
    const h = document.createElement('div'); h.className = 'p-help';
    h.innerHTML = `<div class="ph-head"><b>Player Controls &amp; Shortcuts</b><button type="button" class="pbtn" id="phClose" aria-label="Close">${I.close}</button></div>
      <div class="ph-grid">
        <div class="ph-col">
          <div class="ph-sec">Playback &amp; Seeking</div>
          <div><kbd>Space</kbd> <kbd>K</kbd><span>Play / Pause</span></div>
          <div><kbd>←</kbd> <kbd>→</kbd><span>Skip −10s / +10s</span></div>
          <div><kbd>Shift</kbd>+<kbd>←</kbd>/<kbd>→</kbd><span>Skip −30s / +30s</span></div>
          <div><kbd>J</kbd> <kbd>L</kbd><span>Skip −10s / +10s (L: Last ch on Live)</span></div>
          <div><kbd>0</kbd>–<kbd>9</kbd><span>Jump to 0%–90%</span></div>
          <div><kbd>,</kbd> <kbd>.</kbd><span>Frame step when paused</span></div>
          <div><kbd>&lt;</kbd> <kbd>&gt;</kbd><span>Playback speed</span></div>
          <div><kbd>S</kbd> <kbd>U</kbd><span>Skip segment · Undo auto-skip</span></div>
          <div><kbd>N</kbd> <kbd>B</kbd><span>Next / Previous episode</span></div>
        </div>
        <div class="ph-col">
          <div class="ph-sec">Sound, Subtitles &amp; View</div>
          <div><kbd>Scroll</kbd> <kbd>↑</kbd>/<kbd>↓</kbd><span>Volume up / down</span></div>
          <div><kbd>M</kbd><span>Mute / Unmute</span></div>
          <div><kbd>F</kbd> <kbd>Dbl-click</kbd><span>Fullscreen</span></div>
          <div><kbd>Shift</kbd>+<kbd>P</kbd><span>Picture-in-Picture</span></div>
          <div><kbd>Z</kbd><span>Picture fit / crop / zoom</span></div>
          <div><kbd>C</kbd> <kbd>A</kbd> <kbd>V</kbd><span>Subtitles · Audio · Clear Voice</span></div>
          <div><kbd>R</kbd><span>Replay 8s with subtitles</span></div>
          <div><kbd>G</kbd>/<kbd>H</kbd> <kbd>[</kbd>/<kbd>]</kbd><span>Sub delay · Audio sync</span></div>
          <div><kbd>I</kbd> <kbd>P</kbd> <kbd>Esc</kbd><span>Stats · Pro player · Close</span></div>
        </div>
      </div>`;
    el.appendChild(h);
    $('#phClose', h).onclick = () => { h.remove(); focusPlayer(); wakeControls(); };
    wakeControls();
  }
  function stepFrame(dir) {
    if (!o || o.live || !video || !duration) return;
    if (!video.paused) video.pause();
    const step = 1 / (probe?.pic?.fps || 24);
    const t = Math.max(0, Math.min(duration - 0.1, pos() + dir * step));
    if (mseActive && Mse.isBuffered(t)) {
      video.currentTime = t;
      tick();
      showOsd(dir > 0 ? 'Frame +1' : 'Frame −1');
    } else if (!mseActive && video.readyState >= 2) {
      video.currentTime = Math.max(0, video.currentTime + dir * step);
      tick();
      showOsd(dir > 0 ? 'Frame +1' : 'Frame −1');
    }
  }
  function renderSeekMarkers() {
    const segs = $('#pSeek .segs', el); if (!segs || !o || o.live || o.spoilerFree || !duration) return;
    const parts = [];
    for (const sk of SKIPS) {
      const a = Math.max(0, Math.min(100, (sk.s / duration) * 100));
      const b = Math.max(a, Math.min(100, (sk.e / duration) * 100));
      const cls = sk.kind === 'ending' ? 'ending' : sk.kind === 'recap' ? 'recap' : 'intro';
      if (b - a > 0.2) parts.push(`<i class="seg ${cls}${sk.auto === false ? ' manual' : ''}" style="left:${a.toFixed(2)}%;width:${(b - a).toFixed(2)}%"></i>`);
    }
    if (credits && credits > duration * 0.55 && credits < duration) {
      const a = Math.max(0, Math.min(100, (credits / duration) * 100));
      const endPos = (hasPostCredits && creditsEnd && creditsEnd > credits + 5 && creditsEnd < duration - 12) ? creditsEnd : duration;
      const b = Math.max(a, Math.min(100, (endPos / duration) * 100));
      parts.push(`<i class="seg credits" style="left:${a.toFixed(2)}%;width:${(b - a).toFixed(2)}%"></i>`);
    }
    segs.innerHTML = parts.join('');
  }
  function segmentLabelAt(sec) {
    const sk = SKIPS.find((c) => sec >= c.s && sec <= c.e);
    if (sk) {
      if (sk.kind === 'ending') return sk.mixed ? 'Ending (Mixed)' : 'Ending';
      const t = sk.title || '';
      if (/recap/i.test(t) && /intro|opening/i.test(t)) return o?.isAnime ? 'Recap + Opening' : 'Recap + Intro';
      if (sk.kind === 'recap' || /recap|previously/i.test(t)) return 'Recap';
      const base = o?.isAnime ? 'Opening' : 'Intro';
      return sk.mixed ? `${base} (Mixed)` : base;
    }
    if (credits && duration && credits > duration * 0.55 && sec >= credits) {
      if (hasPostCredits && creditsEnd && sec >= creditsEnd) return 'Post-Credits / Preview';
      return o?.isAnime ? (SKIPS.some((c) => c.kind === 'ending') ? 'Final Credits' : 'Ending') : 'Credits';
    }
    const edSeg = SKIPS.find((c) => c.kind === 'ending');
    if (edSeg && sec > edSeg.e) return 'Post-Credits / Epilogue';
    return '';
  }
  function updateBufferedBar() {
    const bufEl = $('#pSeek .buf', el);
    if (!bufEl || !video || !o || o.live || !duration) return;
    try {
      const curT = video.currentTime || 0;
      let endT = 0;
      for (let i = 0; i < video.buffered.length; i++) {
        if (video.buffered.start(i) <= curT + 1.5 && video.buffered.end(i) > endT) endT = video.buffered.end(i);
      }
      const absEnd = mseActive ? endT : (endT > 0 ? offset + endT : 0);
      const pct = Math.max(0, Math.min(100, (absEnd / duration) * 100));
      bufEl.style.width = pct + '%';
    } catch {}
  }
  function renderTimeText(p) {
    const tm = $('#pTime', el); if (!tm || !o || o.live) return;
    if (duration) {
      const rem = Math.max(0, duration - p);
      const endTs = Date.now() + (rem / (playRate || 1)) * 1000;
      const main = showRem ? `${fmtTime(p)} / −${fmtTime(rem)}` : `${fmtTime(p)} / ${fmtTime(duration)}`;
      tm.innerHTML = `${main}<span class="ends-at"> · Ends ${fmtClock(endTs)}</span>`;
    } else {
      tm.textContent = fmtTime(p);
    }
  }

  const pos = () => {
    if (casting || pro) return offset;
    if (mseActive) return (video && Number.isFinite(video.currentTime) && (started || video.readyState >= 1)) ? video.currentTime : mseTarget;
    return offset + (video?.currentTime || 0);
  };
  const cur = (i = ai) => attempts[i] || { id: o.id };

  function src(extra = {}) {
    const a = cur();
    const tVal = extra.t !== undefined ? extra.t : offset;
    const q = new URLSearchParams({ t: Math.floor(tVal), a: audio });
    const ext = a.id === o.id ? o.ext : a.ext; if (ext) q.set('ext', ext);
    if (o.kind === 'series' && o.sid && a.id === o.id) q.set('sid', o.sid);
    if (o.catchup) { const snapped = Math.floor(tVal / 60) * 60; offset = snapped; q.set('t', snapped); q.set('cs', o.catchup.cs); q.set('dur', o.catchup.dur); }
    if (o.live && liveBack > 0.5) q.set('back', liveBack.toFixed(1));
    if (o.live && localGet('di.' + o.id, '0') === '1') q.set('di', '1');
    if (localGet('night', '0') === '1') q.set('nd', '1');
    if (a.tx) q.set('tx', '1'); if (a.alt) q.set('alt', '1');
    if (extra.mse) q.set('mse', '1');
    if (SURROUND_OUT) q.set('sc', '1');
    q.set('pg', PAGE_ID);
    const ad = +localGet('ad.' + (o.progressKey || o.id), 0) || 0; if (ad && !o.live) q.set('ad', ad);
    return `/stream/${o.kind}/${a.id}?${q}`;
  }

  // ---- MSE engine: streams fMP4 from /stream/...&mse=1 into MediaSource/SourceBuffer.
  // Enables 0ms in-buffer seeks, ~150ms in-spool seeks without tearing down <video>, and seamless
  // version switches via changeType(). Falls back transparently to classic <video src> if needed.
  const Mse = (() => {
    let ms = null; let sb = null; let url = null; let mime = null;
    let ctrl = null; let fetchGen = 0; let queue = Promise.resolve();
    const u32 = (b, o) => ((b[o] << 24) | (b[o + 1] << 16) | (b[o + 2] << 8) | b[o + 3]) >>> 0;
    const str4 = (b, o) => String.fromCharCode(b[o], b[o + 1], b[o + 2], b[o + 3]);
    const hex2 = (n) => n.toString(16).padStart(2, '0');
    function iterBoxes(b, start, end, fn) {
      let o = start;
      while (o + 8 <= end) {
        let sz = u32(b, o); let h = 8;
        if (sz === 1 && o + 16 <= end) { sz = u32(b, o + 8) * 4294967296 + u32(b, o + 12); h = 16; }
        else if (sz === 0) sz = end - o;
        if (sz < h || o + sz > end) break;
        fn(str4(b, o + 4), o + h, o + sz);
        o += sz;
      }
    }
    function parseCodecs(moov) {
      const codecs = [];
      const h = u32(moov, 0) === 1 ? 16 : 8;
      iterBoxes(moov, h, moov.length, (t, s, e) => {
        if (t !== 'trak') return;
        iterBoxes(moov, s, e, (t2, s2, e2) => {
          if (t2 !== 'mdia') return;
          iterBoxes(moov, s2, e2, (t3, s3, e3) => {
            if (t3 !== 'minf') return;
            iterBoxes(moov, s3, e3, (t4, s4, e4) => {
              if (t4 !== 'stbl') return;
              iterBoxes(moov, s4, e4, (t5, s5, e5) => {
                if (t5 !== 'stsd' || s5 + 8 > e5) return;
                iterBoxes(moov, s5 + 8, e5, (fmt, fs, fe) => {
                  if (fmt === 'avc1' || fmt === 'avc3') {
                    let c = `${fmt}.640028`;
                    if (fs + 78 < fe) iterBoxes(moov, fs + 78, fe, (bt, bs, be) => {
                      if (bt === 'avcC' && bs + 4 <= be) c = `${fmt}.${hex2(moov[bs + 1])}${hex2(moov[bs + 2])}${hex2(moov[bs + 3])}`;
                    });
                    codecs.push(c);
                  } else if (fmt === 'hvc1' || fmt === 'hev1') {
                    let c = `${fmt}.1.6.L153.B0`;
                    if (fs + 78 < fe) iterBoxes(moov, fs + 78, fe, (bt, bs, be) => {
                      if (bt === 'hvcC' && bs + 13 <= be) {
                        const b1 = moov[bs + 1];
                        const space = ['', 'A', 'B', 'C'][(b1 >> 6) & 3];
                        const tier = (b1 & 0x20) ? 'H' : 'L';
                        const prof = b1 & 0x1f;
                        const compat = u32(moov, bs + 2);
                        // Reverse bits for RFC 6381 general_profile_compatibility_flags
                        let rev = 0; for (let i = 0; i < 32; i++) if (compat & (1 << i)) rev |= (1 << (31 - i));
                        const lvl = moov[bs + 12];
                        let constr = '';
                        for (let i = 5; i >= 0; i--) { const v = moov[bs + 6 + i]; if (v || constr) constr = `.${v.toString(16).toUpperCase()}${constr}`; }
                        c = `${fmt}.${space}${prof}.${(rev >>> 0).toString(16).toUpperCase()}.${tier}${lvl}${constr || '.B0'}`;
                      }
                    });
                    codecs.push(c);
                  } else if (fmt === 'av01') {
                    let c = 'av01.0.08M.08';
                    if (fs + 78 < fe) iterBoxes(moov, fs + 78, fe, (bt, bs, be) => {
                      if (bt === 'av1C' && bs + 4 <= be) {
                        const prof = (moov[bs + 1] >> 5) & 7;
                        const lvl = String(moov[bs + 1] & 0x1f).padStart(2, '0');
                        const tier = (moov[bs + 2] & 0x80) ? 'H' : 'M';
                        const hi = (moov[bs + 2] >> 6) & 1; const tw = (moov[bs + 2] >> 5) & 1;
                        const bits = hi ? (tw ? '12' : '10') : '08';
                        c = `av01.${prof}.${lvl}${tier}.${bits}`;
                      }
                    });
                    codecs.push(c);
                  } else if (fmt === 'vp09') {
                    let c = 'vp09.00.41.08';
                    if (fs + 78 < fe) iterBoxes(moov, fs + 78, fe, (bt, bs, be) => {
                      if (bt === 'vpcC' && bs + 8 <= be) c = `vp09.${String(moov[bs + 4]).padStart(2, '0')}.${String(moov[bs + 5]).padStart(2, '0')}.${String(moov[bs + 6] >> 4).padStart(2, '0')}`;
                    });
                    codecs.push(c);
                  } else if (fmt === 'mp4a') {
                    codecs.push('mp4a.40.2');
                  }
                });
              });
            });
          });
        });
      });
      return codecs.length ? `video/mp4; codecs="${codecs.join(', ')}"` : null;
    }
    function concat(a, b) { const r = new Uint8Array(a.length + b.length); r.set(a, 0); r.set(b, a.length); return r; }
    function waitUpdate(buf) {
      if (!buf || !buf.updating) return Promise.resolve();
      return new Promise((res) => buf.addEventListener('updateend', res, { once: true }));
    }
    function enqueue(fn) {
      queue = queue.then(fn, fn);
      return queue;
    }
    function abortFetch() {
      fetchGen++;
      if (ctrl) { try { ctrl.abort(); } catch {} ctrl = null; }
    }
    function destroy() {
      abortFetch();
      mseActive = false;
      sb = null; mime = null;
      if (ms) { try { if (ms.readyState === 'open') ms.endOfStream(); } catch {} ms = null; }
      if (url) { try { URL.revokeObjectURL(url); } catch {} url = null; }
    }
    function isBuffered(t) {
      if (!video || !mseActive) return false;
      try {
        for (let i = 0; i < video.buffered.length; i++) {
          if (t >= video.buffered.start(i) + 0.1 && t <= video.buffered.end(i) - 0.5) return true;
        }
      } catch {}
      return false;
    }
    function bufferedAhead() {
      if (!video) return 0;
      const t = video.currentTime || 0;
      try {
        for (let i = 0; i < video.buffered.length; i++) {
          if (video.buffered.start(i) <= t + 1 && video.buffered.end(i) > t) return video.buffered.end(i) - t;
        }
      } catch {}
      return 0;
    }
    async function ensureOpen() {
      if (ms && ms.readyState === 'open') return;
      sb = null; mime = null;
      if (ms) { try { if (ms.readyState === 'open') ms.endOfStream(); } catch {} ms = null; }
      if (url) { try { URL.revokeObjectURL(url); } catch {} url = null; }
      ms = new MediaSource();
      url = URL.createObjectURL(ms);
      mseActive = true;
      await new Promise((resolve, reject) => {
        const onOpen = () => { cleanup(); resolve(); };
        const onClose = () => { cleanup(); reject(new Error('ms closed')); };
        const cleanup = () => { ms?.removeEventListener('sourceopen', onOpen); ms?.removeEventListener('sourceclose', onClose); };
        ms.addEventListener('sourceopen', onOpen);
        ms.addEventListener('sourceclose', onClose);
        video.src = url;
      });
    }
    async function appendSeg(bytes, fg, tsOffset, isInit, spliceAt) {
      if (fg !== fetchGen || !ms || ms.readyState !== 'open' || !sb) return;
      await waitUpdate(sb);
      if (fg !== fetchGen || !ms || ms.readyState !== 'open' || !sb) return;
      if (spliceAt != null && Number.isFinite(spliceAt)) {
        try {
          if (video.buffered.length) {
            const end = video.buffered.end(video.buffered.length - 1);
            if (end > spliceAt) { sb.remove(spliceAt, end + 1); await waitUpdate(sb); }
          }
        } catch {}
      }
      if (tsOffset != null) {
        try { sb.timestampOffset = tsOffset; } catch {}
      }
      try {
        sb.appendBuffer(bytes);
        await waitUpdate(sb);
      } catch (e) {
        if (e?.name === 'QuotaExceededError') {
          try {
            const ct = video.currentTime || 0;
            if (ct > 35) { sb.remove(0, ct - 30); await waitUpdate(sb); }
            sb.appendBuffer(bytes);
            await waitUpdate(sb);
          } catch {}
        } else if (!isInit) {
          throw e;
        }
      }
    }
    async function startFetch(targetSec, { splice = false, keepPaused = false } = {}) {
      const me = o; const lg = loadGen;
      abortFetch();
      const fg = ++fetchGen;
      mseTarget = targetSec;
      const myCtrl = new AbortController(); ctrl = myCtrl;
      let ftyp = null; let moov = null; let pendingMoof = null;
      let startSec = targetSec; let firstMoofDone = false; let initDone = false;
      // Frame-accurate seek landing: ffmpeg -noaccurate_seek starts at the keyframe BEFORE targetSec
      // (often 4-8s early in h264 anime!). Hold start until chunks buffer past targetSec (~40ms from spool)
      // so auto-skip & seeks land on the exact frame instead of playing 5s of leftover OP music.
      let exactSeekGoal = (!splice && targetSec > 0.5) ? targetSec : null;
      let playbackStarted = false;
      const exactStartMs = performance.now();
      let buf = new Uint8Array(0); let bufLen = 0;

      const appendBytes = (chunk) => {
        if (bufLen + chunk.length > buf.length) {
          const next = new Uint8Array(Math.max(buf.length * 2, bufLen + chunk.length, 256 * 1024));
          next.set(buf.subarray(0, bufLen), 0);
          buf = next;
        }
        buf.set(chunk, bufLen); bufLen += chunk.length;
      };
      const consume = (n) => {
        if (n >= bufLen) { bufLen = 0; return; }
        buf.copyWithin(0, n, bufLen); bufLen -= n;
      };

      try {
        await ensureOpen();
        if (fg !== fetchGen || o !== me) return;
        if (sb && !splice) {
          await enqueue(async () => {
            if (fg !== fetchGen || !sb || !ms || ms.readyState !== 'open') return;
            try { sb.abort(); } catch {}
            await waitUpdate(sb);
          });
        }
        const res = await fetch(src({ t: targetSec, mse: true }), { signal: myCtrl.signal });
        if (!res.ok || !res.body) throw new Error(`HTTP ${res.status}`);
        const reader = res.body.getReader();

        while (fg === fetchGen && o === me) {
          // Backpressure so browser buffer stays smooth without filling RAM
          while (fg === fetchGen && started && bufferedAhead() > 75) {
            await new Promise((r) => setTimeout(r, 400));
          }
          if (fg !== fetchGen || o !== me) break;
          const { value, done } = await reader.read();
          if (done) {
            if (!playbackStarted && video && fg === fetchGen && o === me) {
              playbackStarted = true; exactSeekGoal = null;
              try {
                if (video.buffered.length) {
                  const bs = video.buffered.start(video.buffered.length - 1);
                  const be = video.buffered.end(video.buffered.length - 1);
                  video.currentTime = Math.max(bs, Math.min(be - 0.05, Math.max(startSec, targetSec)));
                }
              } catch {}
              if (!keepPaused) video.play().catch(() => {}); else video.pause();
            }
            // Stream ended: if well before film end, reconnect seamlessly from buffered end
            const end = video?.buffered?.length ? video.buffered.end(video.buffered.length - 1) : pos();
            const realEnd = credits && credits > duration * 0.6 ? credits - 30 : duration - 90;
            if (duration && end < realEnd && fg === fetchGen && o === me && retries < 3) {
              retries++; retryAt = performance.now();
              return startFetch(Math.max(0, Math.floor(end)), { splice: true, keepPaused: video?.paused });
            }
            if (ms && ms.readyState === 'open' && fg === fetchGen) {
              await enqueue(async () => { await waitUpdate(sb); try { if (ms?.readyState === 'open') ms.endOfStream(); } catch {} });
            }
            break;
          }
          appendBytes(value);

          // Extract complete top-level MP4 boxes
          let off = 0;
          while (bufLen - off >= 8) {
            let sz = u32(buf, off); let h = 8;
            if (sz === 1) {
              if (bufLen - off < 16) break;
              sz = u32(buf, off + 8) * 4294967296 + u32(buf, off + 12); h = 16;
            }
            if (sz < h || bufLen - off < sz) break;
            const typ = str4(buf, off + 4);
            const box = buf.slice(off, off + sz);
            off += sz;

            if (typ === 'ftyp') {
              ftyp = box;
            } else if (typ === 'moov') {
              moov = box;
              const nextMime = parseCodecs(moov);
              if (!nextMime || !MediaSource.isTypeSupported(nextMime)) {
                throw Object.assign(new Error('unsupported-mse-codec'), { fallback: true });
              }
              const initSeg = ftyp ? concat(ftyp, moov) : moov;
              await enqueue(async () => {
                if (fg !== fetchGen || !ms || ms.readyState !== 'open') return;
                if (!sb) {
                  sb = ms.addSourceBuffer(nextMime);
                  sb.mode = 'segments';
                  mime = nextMime;
                } else if (nextMime !== mime) {
                  await waitUpdate(sb);
                  if (typeof sb.changeType === 'function') {
                    sb.changeType(nextMime);
                    mime = nextMime;
                  } else {
                    throw Object.assign(new Error('no-changetype'), { fallback: true });
                  }
                }
                await appendSeg(initSeg, fg, null, true, null);
                if (duration > 0 && ms && ms.readyState === 'open') {
                  try { await waitUpdate(sb); if (!Number.isFinite(ms.duration) || Math.abs(ms.duration - duration) > 2) ms.duration = duration; } catch {}
                }
                initDone = true;
              });
            } else if (typ === 'free' && sz >= 20 && str4(box, 8) === 'TVST') {
              const dv = new DataView(box.buffer, box.byteOffset + 12, 8);
              const val = dv.getFloat64(0, false);
              if (Number.isFinite(val)) {
                const s0 = Math.max(0, val);
                startSec = s0;
                mseSessionStart = s0;
                offset = 0; base = 0; baseKnown = true;
                if (!sync.deltas) sync.baseSec = s0;
                rebuildCues();
              }
            } else if (typ === 'moof') {
              pendingMoof = box;
            } else if (typ === 'mdat' && pendingMoof) {
              const seg = concat(pendingMoof, box);
              pendingMoof = null;
              const isFirst = !firstMoofDone;
              firstMoofDone = true;
              const tsOff = isFirst ? startSec : null;
              const spAt = (isFirst && splice) ? Math.max(startSec, (video?.currentTime || 0) + 0.5) : null;
              await enqueue(async () => {
                if (fg !== fetchGen || !initDone) return;
                await appendSeg(seg, fg, tsOff, false, spAt);
                if (!playbackStarted && video && fg === fetchGen && o === me) {
                  if (splice) {
                    playbackStarted = true;
                    if (!keepPaused) video.play().catch(() => {});
                    else video.pause();
                  } else {
                    const seekGoal = exactSeekGoal ?? Math.max(startSec, targetSec);
                    let bs = startSec; let be = startSec;
                    try {
                      if (video.buffered.length) {
                        for (let bi = video.buffered.length - 1; bi >= 0; bi--) {
                          if (video.buffered.start(bi) <= Math.max(startSec, seekGoal) + 2) {
                            bs = video.buffered.start(bi); be = video.buffered.end(bi); break;
                          }
                        }
                        if (be <= bs) {
                          bs = video.buffered.start(video.buffered.length - 1);
                          be = video.buffered.end(video.buffered.length - 1);
                        }
                      }
                    } catch {}
                    const reachedExact = exactSeekGoal === null || be >= exactSeekGoal + 0.15 || (performance.now() - exactStartMs > 1400);
                    if (reachedExact) {
                      playbackStarted = true;
                      exactSeekGoal = null;
                      try {
                        video.currentTime = be > bs ? Math.max(bs, Math.min(be - 0.05, seekGoal)) : seekGoal;
                      } catch {}
                      if (!keepPaused) video.play().catch(() => {});
                      else video.pause();
                    }
                  }
                }
                // Evict old buffer behind playhead
                if (sb && !sb.updating && video && video.buffered.length) {
                  const ct = video.currentTime || 0;
                  if (ct - video.buffered.start(0) > 100) {
                    try { sb.remove(0, ct - 45); await waitUpdate(sb); } catch {}
                  }
                }
              });
            }
          }
          if (off > 0) consume(off);
        }
      } catch (e) {
        if (e?.name === 'AbortError' || fg !== fetchGen || o !== me || lg !== loadGen) return;
        console.warn('[mse]', e);
        if (e?.fallback || !started) {
          destroy();
          if (e?.fallback) mseFailed = true;
          offset = targetSec;
          loadDirect();
          return;
        }
        if (!resumeHere('Stream hiccup, reconnecting…')) fail('mse');
      }
    }
    return { startFetch, destroy, isBuffered, abortFetch };
  })();

  function loadDirect(why) {
    Mse.destroy();
    started = false; showMsg(null); spin(true, why || 'Loading…'); loadT = performance.now(); reportedStart = false; stallAt = 0; clearTimeout(stallTimer2); loadGen++;
    resetSubs(); base = 0; baseKnown = false; if (!o.live) restoreSync();
    video.src = src(); video.play().catch(() => {});
    clearTimeout(stallTimer);
    stallTimer = setTimeout(() => { if (!started && !(video.paused && video.readyState >= 2)) fail('timeout'); }, o.live ? 20000 : 30000);
  }
  function load(opts = {}) {
    if (!o.live && !o.catchup && !casting && !pro && mseEnabled()) {
      const target = opts.target ?? offset;
      const keepPaused = !!opts.keepPaused;
      const splice = !!opts.splice;
      if (!splice) { started = false; spin(true, opts.why || (target > 5 ? `Loading ${fmtTime(target)}…` : 'Loading…')); }
      showMsg(null); loadT = performance.now(); reportedStart = false; stallAt = 0; clearTimeout(stallTimer2); loadGen++;
      resetSubs(); base = 0; baseKnown = false; restoreSync();
      clearTimeout(stallTimer);
      stallTimer = setTimeout(() => { if (!started && !(video.paused && video.readyState >= 2)) fail('timeout'); }, 30000);
      Mse.startFetch(target, { splice, keepPaused });
      return;
    }
    loadDirect(opts.why);
  }
  function buildAttempts() {
    const list = [{ id: o.id, label: o.label, ext: o.ext, cat: o.cat, catId: o.catId, subMarket: !!o.subMarket }, { id: o.id, tx: true, label: o.label, ext: o.ext, subMarket: !!o.subMarket }];
    for (const v of o.alternates || []) if (v.id !== o.id) list.push({ id: v.id, label: v.label || v.channel, ext: v.ext, cat: v.cat, catId: v.catId, subMarket: !!v.subMarket });
    list.push({ id: o.id, alt: true, label: o.label, ext: o.ext, subMarket: !!o.subMarket });
    return list;
  }
  function report(ev, extra = {}, beacon = false) {
    const id = cur().id; if (!id || o?.catchup) return;
    if (ev === 'fail') { if (failed.has(id)) return; failed.add(id); } // compatibility / backup-server retries are the same source
    const body = { id, ev, ...extra };
    if (beacon) navigator.sendBeacon?.('/api/health', new Blob([JSON.stringify(body)], { type: 'application/json' })); else post('/api/health', body).catch(() => {});
  }
  // watched time + stutter that never became a long stall: short buffering pauses and dropped frames
  function flushWatched(beacon = false) {
    const q = video?.getVideoPlaybackQuality?.(); const d = q && lastQ ? { dropped: q.droppedVideoFrames - lastQ.droppedVideoFrames, frames: q.totalVideoFrames - lastQ.totalVideoFrames } : {};
    if (q) lastQ = { droppedVideoFrames: q.droppedVideoFrames, totalVideoFrames: q.totalVideoFrames };
    if (watchedSecs > 5) report('watched', { secs: Math.round(watchedSecs), hitches, ...d }, beacon);
    watchedSecs = 0; hitches = 0;
  }
  // resume the same version where it stopped (network drop, provider hiccup, cut by another window)
  function resumeHere(why) {
    if (!o) return false;
    if (performance.now() - retryAt > 120000) retries = 0;
    if (retries >= 3) return false;
    retries++; retryAt = performance.now();
    if (o.live) { liveBack = liveBehind; } else { offset = pos(); }
    if (!o.live) toast(why || 'Reconnecting…');
    load({ target: offset });
    return true;
  }
  // Mid-playback: a long freeze or repeated stutters move to the next version/feed of the same title.
  function switchSource(why) {
    const here = cur().id; const nx = attempts.findIndex((a, i) => i > ai && !a.tx && !a.alt && a.id !== here);
    if (nx < 0) { if (why) toast('This stream is struggling. It may be your connection or the provider'); return false; }
    toast(`${o?.live ? 'Switching backup feed' : 'Playback was stuttering, switched'} to ${cur(nx).label || 'another source'}`); useAttempt(nx); return true;
  }
  // move to attempt i: a different stream id gets its own probe, and the same audio/subtitle LANGUAGE is kept
  async function useAttempt(i) {
    flushWatched();
    const at = pos();
    if (!o.live) offset = at;
    stalls = []; retries = 0;
    const prevA = probe?.audio?.[audio]?.lang; const prevS = subSel >= 0 ? probe?.subs?.find((x) => x.i === subSel)?.lang : null;
    const was = cur().id; ai = i; const a = cur(); const me = o;
    if (a.id !== was && !o.live && !o.catchup) {
      spin(true);
      const pq = new URLSearchParams(); if (a.ext) pq.set('ext', a.ext);
      const p = await api(`/api/probe/${o.kind}/${a.id}?${pq}`).catch(() => null);
      if (o !== me) return;
      if (p) {
        probe = p;
        applyTracks(prevA, prevS ?? 'off');
        const ql = qLabel(probe?.pic, probe?.audio);
        const oldQ = $('.qbadge', el); if (oldQ) oldQ.remove();
        if (ql) $('#pSub')?.insertAdjacentHTML('beforeend', ` <span class="qbadge">${esc(ql)}</span>`);
        if (proRule(probe?.pic) && (await proInfo()).available) { renderSources(); if (await toPro()) return; }
        matchDisplay(p.pic?.fps, !!p.pic?.hdr);
      }
    }
    if (o.live) { liveBack = 0; liveBehind = 0; liveBufSecs = 0; }
    load({ target: at, splice: mseActive && started && !o.live && !o.catchup });
    renderSources();
  }
  function renderSources() {
    const sel = $('#pSrc'); if (!sel || !o) return;
    const list = [{ id: o.id, label: o.label || (o.live ? 'Main feed' : 'Original'), cat: o.cat, catId: o.catId }, ...(o.alternates || [])];
    if (list.length < 2) { sel.hidden = true; return; }
    sel.hidden = false;
    sel.title = o.live ? 'Backup feeds' : 'Version';
    sel.innerHTML = list.map((v) => `<option value="${v.id}">${esc(v.label || v.channel || (o.live ? 'Feed' : 'Version'))}${v.cat ? ` · ${esc(v.cat.slice(0, 28))}` : ''}</option>`).join('');
    sel.value = cur().id;
    sel.onchange = () => {
      sel.blur(); focusPlayer();
      const id = +sel.value;
      const i = attempts.findIndex((a) => a.id === id && !a.tx && !a.alt);
      if (i >= 0) useAttempt(i);
      if (!o.live && !o.catchup) {
        const v = list.find((x) => x.id === id);
        if (o.kind === 'series' && o.sid) {
          if (v?.catId || v?.cat) post('/api/pick', { key: 's:' + o.sid, catId: v?.catId || '', cat: v?.cat || '' }).catch(() => {});
        } else if (id) {
          post('/api/pick', { key: o.progressKey, id }).catch(() => {});
        }
      }
    };
  }
  function fail() {
    if (!o) return;
    report('fail');
    if (ai < attempts.length - 1) {
      const a = cur(ai + 1);
      toast(a.tx ? 'Switching to compatibility mode…' : a.alt ? 'Trying the backup server…' : `Trying another source${a.label ? `: ${a.label}` : ''}…`);
      return useAttempt(ai + 1);
    }
    spin(false);
    showMsg(`<h4>This stream won't play right now</h4><p>${o.live ? 'The channel may be offline or the event hasn’t started.' : 'None of the sources responded.'} If another device is using your account, it blocks this one.</p>
      <div class="btns" style="justify-content:center"><button class="btn btn-primary btn-sm" id="pRetry">Try again</button><button class="btn btn-ghost btn-sm" id="pClose">Close</button></div>`);
    $('#pRetry').onclick = () => { ai = 0; load(); };
    $('#pClose').onclick = () => close();
  }
  function spin(on, label) {
    const w = $('.spin-wrap', el);
    if (w) {
      w.hidden = !on;
      const lb = $('.spin-lbl', w);
      if (lb) lb.textContent = on ? (label || '') : '';
    } else {
      $('.spinner', el)?.toggleAttribute('hidden', !on);
    }
  }
  function showMsg(html) { const m = $('.p-msg', el); if (!m) return; if (!html) { m.hidden = true; return; } m.innerHTML = html; m.hidden = false; }

  // ---- subtitles: ffmpeg writes WebVTT as it goes; we poll and (re)build cues.
  // Self-correcting sync: the server compares subtitle timing with detected speech (WebRTC VAD +
  // alass alignment) and returns per-line corrections; we re-check as more audio is heard and only
  // apply a correction when the server's self-check says it improves alignment.
  let base = 0; let baseKnown = false;
  let rawCues = []; let sync = { ratio: 1, deltas: null, tail: 0, analyzed: 0, baseSec: 0, deltaStart: 0 }; let userShift = 0; let syncTimer; let syncChecks = 0; let syncedOnce = false;
  const sk = () => o?.progressKey || o?.id;
  function resetSubs() {
    subSeen = 0; rawCues = [];
    if (subTrack) for (const c of [...(subTrack.cues || [])]) subTrack.removeCue(c);
  }
  function parseVtt(text) {
    const out = []; const blocks = text.replace(/\r/g, '').split('\n\n');
    const ts = (s) => { const p = s.split(':').map(parseFloat); return p.length === 3 ? p[0] * 3600 + p[1] * 60 + p[2] : p[0] * 60 + p[1]; };
    for (const b of blocks) {
      const lines = b.split('\n'); const i = lines.findIndex((l) => l.includes('-->'));
      if (i < 0) continue;
      const [a, z] = lines[i].split('-->').map((s) => s.trim().split(' ')[0]);
      const txt = lines.slice(i + 1).join('\n').replace(/<[^>]+>/g, '').replace(/\{[^}]*\}/g, '').trim();
      if (txt) out.push([ts(a), ts(z), txt]);
    }
    return out;
  }
  // corrected time for cue i: frame-rate ratio anchored to session sync base (honor MSE & direct),
  // per-line alass delta (or the latest offset for lines not analysed yet), plus manual nudge.
  function resolveSyncBaseSec(r) {
    const curStart = mseActive ? mseSessionStart : base;
    if (Number.isFinite(r?.baseMs)) return r.baseMs / 1000;
    if (Number.isFinite(r?.base)) {
      if (r.base === 0) return 0;
      // Server returns base in ms (e.g. 3600000); handle seconds too if ever provided
      if (curStart > 1 && Math.abs(r.base - curStart) < Math.abs(r.base / 1000 - curStart)) return r.base;
      return r.base / 1000;
    }
    return curStart;
  }
  function shiftFor(i) {
    const idx = i - (sync.deltaStart || 0);
    const d = sync.deltas
      ? (idx < 0 ? (sync.deltas[0] ?? sync.tail) : idx < sync.deltas.length ? sync.deltas[idx] : sync.tail)
      : sync.tail;
    return d / 1000 + userShift / 1000;
  }
  function addCue(i) {
    const [s, e, t] = rawCues[i]; const sh = shiftFor(i); const r = sync.ratio;
    const b = Number.isFinite(sync.baseSec) ? sync.baseSec : base;
    const t0 = (s - b) * r + b - base + sh;
    const t1 = (e - b) * r + b - base + sh;
    try { const c = new VTTCue(Math.max(0, t0), Math.max(0.05, t1), t); c._i = i; subTrack.addCue(c); } catch {}
  }
  function rebuildCues() { if (!subTrack) return; for (const c of [...(subTrack.cues || [])]) subTrack.removeCue(c); for (let i = 0; i < rawCues.length; i++) addCue(i); }
  let replaySubTimer = null; let replayPrevSub = null;
  async function pollSubs() {
    if (!o || (subSel < 0 && subSel !== -3) || !subTrack || !started) return;
    const g = loadGen;
    const url = subSel === -3 ? o.extSub : `/api/subs/${subSel}`;
    if (!url) return;
    const text = await fetch(url).then((r) => r.text()).catch(() => '');
    if (g !== loadGen || !started) return; // a seek/version change happened meanwhile: this file belongs to the old session
    const cues = parseVtt(text); const have = new Set(rawCues.map((c) => c[0].toFixed(3) + c[2]));
    for (const c of cues) { const k = c[0].toFixed(3) + c[2]; if (have.has(k)) continue; have.add(k); rawCues.push(c); addCue(rawCues.length - 1); }
  }
  async function checkSync() {
    if (!o || o.live || (subSel < 0 && subSel !== -3) || !started) return;
    const url = subSel === -3 ? `/api/extsubs/sync/${encodeURIComponent(o.progressKey || '')}` : `/api/subsync/${subSel}`;
    const r = await api(url).catch(() => null);
    if (!r?.ready) return;
    syncChecks++;
    const baseSec = resolveSyncBaseSec(r);
    if (!r.improves) {
      if (sync.ratio !== 1 || sync.deltas || Math.abs(sync.tail) > 150) {
        sync = { ratio: 1, deltas: null, tail: 0, analyzed: r.analyzed, baseSec, deltaStart: 0 }; rebuildCues();
        localSet('subauto2.' + sk(), JSON.stringify({ ratio: 1, tail: 0, baseSec }));
        toast('Subtitles re-synced');
      }
      if (!syncedOnce) { syncedOnce = true; console.log('[subsync] verified in sync', r); }
      return;
    }
    const prevTail = sync.tail;
    const deltaStart = baseSec > 0.5 && rawCues.length ? Math.max(0, rawCues.findIndex((c) => c[1] > baseSec)) : 0;
    sync = { ratio: r.ratio || 1, deltas: r.deltas, tail: r.tailOffset ?? r.offset, analyzed: r.analyzed, baseSec, deltaStart };
    rebuildCues();
    localSet('subauto2.' + sk(), JSON.stringify({ ratio: sync.ratio, tail: sync.tail, baseSec: sync.baseSec }));
    const s = (sync.tail / 1000).toFixed(1);
    if (!syncedOnce) toast(`Subtitles auto-synced (${sync.tail > 0 ? '+' : ''}${s}s${sync.ratio !== 1 ? ', frame rate corrected' : ''})`);
    else if (Math.abs(sync.tail - prevTail) > 300) toast(`Subtitles re-synced (${sync.tail > 0 ? '+' : ''}${s}s)`);
    syncedOnce = true;
  }
  function scheduleSync() {
    clearInterval(syncTimer);
    if (o?.live || (subSel < 0 && subSel !== -3)) return;
    const marks = [40, 90, 180]; let lastTry = -999; let lastDone = 0;
    syncTimer = setInterval(async () => {
      if (!video || video.paused) return;
      const heard = mseActive ? Math.max(0, (video.currentTime || 0) - mseSessionStart) : (video.currentTime || 0);
      const due = syncChecks < marks.length ? heard >= marks[syncChecks] : heard - lastDone >= 300;
      if (!due || heard - lastTry < 15) return;
      lastTry = heard; const before = syncChecks; await checkSync(); if (syncChecks > before) lastDone = heard;
    }, 5000);
  }
  function setSubs(i, { temp = false } = {}) {
    if (i === -2) { openExtSubsPicker(); return; }
    if (i === -4) { generateAiSubs(); return; }
    subSel = i;
    if (!temp) localSet('subs', i === -3 ? 'ext' : i >= 0 ? (probe?.subs?.find((s) => s.i === i)?.lang || 'on') : 'off');
    if (!subTrack) { subTrack = video.addTextTrack('subtitles', 'Subtitles', 'en'); }
    resetSubs(); subTrack.mode = (i >= 0 || i === -3) ? 'showing' : 'hidden';
    clearInterval(subTimer);
    if (i >= 0) { pollSubs(); subTimer = setInterval(pollSubs, 4000); }
    else if (i === -3) { pollSubs(); }
    syncChecks = 0; scheduleSync();
  }
  function setAiSubBadge(text) {
    let b = $('.ai-sub-badge', el);
    if (!text) { b?.remove(); return; }
    if (!b) {
      b = document.createElement('div'); b.className = 'ai-sub-badge';
      b.innerHTML = '<span class="ai-sub-spin"></span><span class="ai-sub-txt"></span><button type="button" class="ai-sub-x" title="Hide status" aria-label="Hide">×</button>';
      $('.ai-sub-x', b).onclick = () => b.remove();
      el.appendChild(b);
    }
    const t = $('.ai-sub-txt', b);
    if (t) t.textContent = text;
  }
  function clearAiSubJob() {
    clearInterval(aiSubPoll); aiSubPoll = null; aiSubBusy = false;
    setAiSubBadge(null);
  }
  async function generateAiSubs() {
    if (!o || o.live) return;
    const ss = $('#pSubs', el); if (ss) ss.value = String(subSel);
    if (aiSubBusy) return toast('AI subtitles are already generating on your GPU…');
    const me = o;
    const key = o.progressKey || `${o.kind}:${o.id}`;
    aiSubBusy = true;
    setAiSubBadge('✨ Generating AI Subtitles (Local GPU)…');
    const finish = (vttUrl, cues) => {
      clearAiSubJob();
      if (o !== me || !vttUrl) return;
      o.extSub = vttUrl;
      o.extSubLabel = 'English (AI · Local GPU)';
      applyTracks(null, 'ext');
      setSubs(-3);
      if ($('#pSubs', el)) $('#pSubs', el).value = '-3';
      toast(`✨ AI subtitles ready${cues ? ` (${cues} lines)` : ''}`);
      setTimeout(() => checkSync(), 3500);
    };
    try {
      const a = cur();
      const r = await post('/api/extsubs/whisper', {
        key, kind: o.kind, id: a.id, ext: a.ext || o.ext || '', a: audio,
        title: o.title || '', sub: o.sub || '', t: pos(),
      });
      if (o !== me) return clearAiSubJob();
      if (r.vttUrl && (r.done || r.status === 'done' || r.ready || !r.running)) {
        return finish(r.vttUrl, r.cues);
      }
      const statusUrl = r.statusUrl || `/api/extsubs/whisper?key=${encodeURIComponent(r.jobId || key)}`;
      const startedAt = Date.now();
      aiSubPoll = setInterval(async () => {
        if (o !== me || Date.now() - startedAt > 600e3) return clearAiSubJob();
        const st = await api(statusUrl).catch(() => null);
        if (!st || o !== me) return;
        if (st.error || st.status === 'error' || st.status === 'failed') {
          clearAiSubJob();
          return toast(st.error || 'AI subtitle generation failed');
        }
        if (st.vttUrl && (st.done || st.ready || st.status === 'done')) {
          return finish(st.vttUrl, st.cues);
        }
        const pct = Number.isFinite(st.progress) ? ` ${Math.round(st.progress)}%` : Number.isFinite(st.pct) ? ` ${Math.round(st.pct)}%` : '';
        const stage = st.stage || st.message || 'Transcribing on your GPU';
        setAiSubBadge(`✨ AI Subtitles: ${stage}${pct}…`);
      }, 2000);
    } catch (err) {
      clearAiSubJob();
      toast(err.status === 404 ? 'Local GPU AI subtitles require the updated server' : (err.message || 'Could not start AI subtitle generation'));
    }
  }
  async function openExtSubsPicker() {
    if (!o || o.live) return;
    const ss = $('#pSubs'); if (ss) ss.value = subSel;
    const old = $('.extsub-menu', el); if (old) return old.remove();
    const m = document.createElement('div'); m.className = 'cast-menu extsub-menu';
    m.innerHTML = '<b>Online English subtitles</b><div class="muted">Searching OpenSubtitles…</div>';
    el.appendChild(m);
    const pq = new URLSearchParams({
      mt: o.kind === 'series' ? 'tv' : 'movie',
      tmdb: o.tmdb || '', imdb: o.imdb || '', title: o.title || '',
    });
    if (o.kind === 'series' && o.season != null && o.epn != null) { pq.set('s', o.season); pq.set('e', o.epn); }
    const res = await api(`/api/extsubs/search?${pq}`).catch(() => ({ results: [] }));
    if (!m.isConnected) return;
    const list = res.results || [];
    const aiBtnHtml = '<button type="button" id="esAiGen"><span>✨ Generate AI Subtitles (Local GPU)<small>Whisper on your GPU · works offline</small></span></button>';
    if (!list.length) {
      m.innerHTML = '<b>Online English subtitles</b><div class="muted">No English subtitles found online for this title.</div>' + aiBtnHtml;
    } else {
      m.innerHTML = '<b>Pick English subtitles</b>' + aiBtnHtml + list.slice(0, 12).map((x, idx) =>
        `<button data-es="${idx}"><span>${esc(x.label || 'English')}<small>${esc(x.source || 'OpenSubtitles')}${x.hi ? ' · SDH/HI' : ''}</small></span></button>`
      ).join('');
      $$('[data-es]', m).forEach((btn) => (btn.onclick = async () => {
        const pick = list[+btn.dataset.es]; if (!pick) return;
        btn.disabled = true; btn.querySelector('small').textContent = 'Downloading…';
        try {
          const r = await post('/api/extsubs/fetch', { key: o.progressKey || `${o.kind}_${o.id}`, url: pick.url });
          o.extSub = r.vttUrl;
          o.extSubLabel = 'English (Downloaded)';
          m.remove();
          applyTracks(null, 'ext');
          setSubs(-3);
          if ($('#pSubs')) $('#pSubs').value = '-3';
          toast(`Loaded English subtitles (${r.cues} lines) · auto-syncing…`);
          setTimeout(() => checkSync(), 3500);
        } catch (err) {
          toast(err.message || 'Subtitle download failed');
          btn.disabled = false; btn.querySelector('small').textContent = 'Failed — try another';
        }
      }));
    }
    $('#esAiGen', m)?.addEventListener('click', () => { m.remove(); generateAiSubs(); });
    setTimeout(() => document.addEventListener('click', function off(e) {
      if (!m.contains(e.target) && e.target.closest('#pSubs') === null) { m.remove(); document.removeEventListener('click', off); }
    }), 0);
  }
  function replayWithSubs() {
    if (!o || o.live) return;
    const target = Math.max(0, pos() - 8);
    seekTo(target, { byUser: true });
    const hadOff = subSel < 0 && subSel !== -3;
    if (hadOff) {
      const enSub = probe?.subs?.find((s) => /^en/i.test(s.lang) && !s.forced) || probe?.subs?.find((s) => /^en/i.test(s.lang));
      const pick = enSub ? enSub.i : (o.extSub ? -3 : -1);
      if (pick >= 0 || pick === -3) {
        if (replayPrevSub === null) replayPrevSub = subSel;
        setSubs(pick, { temp: true });
        if ($('#pSubs')) $('#pSubs').value = pick;
        clearTimeout(replaySubTimer);
        replaySubTimer = setTimeout(() => {
          if (o && replayPrevSub !== null) {
            const prev = replayPrevSub; replayPrevSub = null;
            setSubs(prev, { temp: true });
            if ($('#pSubs')) $('#pSubs').value = prev;
          }
        }, 10500);
        return toast('Replaying 8s with English subtitles');
      }
    }
    toast('Replaying 8s');
  }
  // Start of a stream session (play, seek, audio change): keep the last known correction as the
  // starting point, then re-verify against the new session's audio.
  function restoreSync() {
    userShift = +localGet('subshift.' + sk(), 0) || 0;
    let saved = null; try { saved = JSON.parse(localGet('subauto2.' + sk(), 'null')); } catch {}
    const r = saved?.ratio || sync.ratio || 1; const tail = saved ? saved.tail : sync.tail;
    const prevBase = Number.isFinite(saved?.baseSec) ? saved.baseSec : 0;
    const pivot = mseActive ? (mseTarget || mseSessionStart || 0) : offset;
    // with a frame-rate correction, re-anchor cleanly to the new session start (MSE or direct)
    sync = { ratio: r, deltas: null, tail: tail + (r !== 1 ? (pivot - prevBase) * (r - 1) * 1000 : 0), analyzed: 0, baseSec: pivot, deltaStart: 0 };
    syncChecks = 0; scheduleSync();
  }
  // Exact session start (film time) from the server: fixes the clock, resume point and subtitle base.
  async function learnStart() {
    baseKnown = true; const me = o; const g = loadGen;
    const r = await api('/api/streamstart').catch(() => null);
    if (!r?.ok || !Number.isFinite(r.start) || o !== me || g !== loadGen) return;
    offset = r.start; base = r.start; if (!sync.deltas) sync.baseSec = r.start; rebuildCues(); tick();
  }
  function nudgeSubs(ms) {
    userShift += ms; localSet('subshift.' + sk(), userShift); rebuildCues();
    toast(`Subtitle delay ${userShift > 0 ? '+' : ''}${(userShift / 1000).toFixed(2)}s (G earlier · H later)`);
  }
  function nudgeAudio(ms) {
    if (o.live) return;
    const k = 'ad.' + sk(); const v = (+localGet(k, 0) || 0) + ms; localSet(k, v);
    toast(`Audio delay ${v > 0 ? '+' : ''}${(v / 1000).toFixed(1)}s ([ earlier · ] later)`);
    clearTimeout(nudgeAudio.t); nudgeAudio.t = setTimeout(() => { if (o) { offset = pos(); load(); } }, 700);
  }

  // ---- display: refresh rate matched to the frame rate (no judder), OLED True Black HDR/SDR switch
  async function matchDisplay(fps, hdr) {
    const r = await post('/api/display/match', { fps: fps || 0, hdr: !!hdr }).catch(() => null);
    if (r?.changed?.length) { toast(`Screen: ${r.changed.join(' · ')} for this ${o?.live ? 'channel' : 'video'}`); await new Promise((ok) => setTimeout(ok, 1400)); }
  }
  // live TV has no probe: measure the frame rate from presented frames, then match (remembered per channel)
  // exact: presented-frame timestamps (Edge/Chrome); fallback: decoded frames over 12 s of playback
  function measureFps() {
    if (!video || !o) return;
    const me = o; const STD = [23.976, 24, 25, 29.97, 30, 50, 59.94, 60];
    const done = (f0, tol) => {
      if (o !== me || !f0) return;
      const f = STD.reduce((b, x) => (Math.abs(x - f0) < Math.abs(b - f0) ? x : b), 60);
      if (Math.abs(f - f0) / f > tol) return; // irregular stream: don't guess
      me.fps = f; if (localGet('fps.' + me.id, '') !== String(f)) { localSet('fps.' + me.id, f); matchDisplay(f, false); }
    };
    const byCount = () => {
      if (!video.getVideoPlaybackQuality) return;
      const q0 = video.getVideoPlaybackQuality().totalVideoFrames; const t0 = video.currentTime;
      setTimeout(() => { if (o !== me || !video || video.paused) return; const dt = video.currentTime - t0; if (dt > 8) done((video.getVideoPlaybackQuality().totalVideoFrames - q0) / dt, 0.02); }, 12000);
    };
    if (!video.requestVideoFrameCallback) return byCount();
    const ts = []; let fired = false;
    const cb = (_n, md) => { fired = true; if (o !== me || !video) return; ts.push(md.mediaTime); if (ts.length < 90) return video.requestVideoFrameCallback(cb);
      const d = []; for (let i = 1; i < ts.length; i++) { const x = ts[i] - ts[i - 1]; if (x > 0.004 && x < 0.2) d.push(x); }
      if (d.length >= 30) { d.sort((a, b) => a - b); done(1 / d[d.length >> 1], 0.03); } };
    video.requestVideoFrameCallback(cb);
    setTimeout(() => { if (!fired) byCount(); }, 2000);
  }
  const qLabel = (pic, aud) => {
    if (!pic?.h) return '';
    const res = pic.h >= 2000 || pic.w >= 3600 ? '4K' : pic.h >= 1000 || pic.w >= 1900 ? '1080p' : pic.h >= 700 ? '720p' : 'SD';
    const hdr = pic.hdr === 'DV' ? 'Dolby Vision' : pic.hdr || ''; const ch = Math.max(0, ...(aud || []).map((a) => a.ch || 0));
    return [res, hdr, ch >= 8 ? '7.1' : ch >= 6 ? '5.1' : ''].filter(Boolean).join(' · ');
  };

  // ---- stats overlay (I): what is really playing, and how well
  function toggleStats(force) {
    const old = $('.stats', el);
    if (old && force !== true) { old.remove(); clearInterval(statsTimer); localSet('stats', '0'); return; }
    if (old) return;
    const box = document.createElement('div'); box.className = 'stats'; el.appendChild(box); localSet('stats', '1');
    let disp = null; let dispAt = 0;
    let rate = 0; const hist = [];
    const draw = async () => {
      if (!o || !box.isConnected) return clearInterval(statsTimer);
      if (Date.now() - dispAt > 4000) { dispAt = Date.now(); api('/api/display').then((d) => (disp = d.displays.find((x) => x.target))).catch(() => {}); }
      const st = await api('/api/streamstats').catch(() => null);
      if (st && lastBytes !== null && st.bytes >= lastBytes) { hist.push(((st.bytes - lastBytes) * 8) / 1000); if (hist.length > 5) hist.shift(); rate = hist.reduce((a, b) => a + b, 0) / hist.length; }
      lastBytes = st?.bytes ?? null;
      const pic = probe?.pic || {}; const q = video?.getVideoPlaybackQuality?.() || {};
      const dpr = window.devicePixelRatio || 1; const sw = Math.round(screen.width * dpr), sh = Math.round(screen.height * dpr);
      const vw = video?.videoWidth || 0, vh = video?.videoHeight || 0;
      let ahead = 0; try { for (let i = 0; i < video.buffered.length; i++) if (video.buffered.start(i) <= video.currentTime + 0.5 && video.buffered.end(i) > video.currentTime) ahead = video.buffered.end(i) - video.currentTime; } catch {}
      const a = probe?.audio?.[audio]; const ch = a?.ch || 0;
      const rows = [
        ['Picture', vw ? `${vw}×${vh}${pic.codec || probe?.video ? ` · ${(pic.codec || probe.video).toUpperCase()}` : ''}${pic.bits > 8 ? ` ${pic.bits}-bit` : ''}` : '…'],
        ['Dynamic range', pic.hdr ? (pic.hdr === 'DV' ? `Dolby Vision${pic.dv ? ` profile ${pic.dv}` : ''}` : pic.hdr + (pic.dv ? ` (+DV ${pic.dv})` : '')) : o.live ? 'SDR (broadcast)' : 'SDR'],
        ['Frame rate', `${pic.fps || o.fps || '…'} fps${pic.interlaced ? ' interlaced' : ''}`],
        ['Screen', `${sw}×${sh}${disp ? ` · ${disp.hz} Hz` : ''} · ${matchMedia('(dynamic-range: high)').matches ? 'HDR on' : 'SDR'}`],
        ['Scaling', vw && vw < sw * 0.95 ? `${(sw / vw).toFixed(1)}× up to the screen (RTX Super Resolution sharpens this when it's on)` : 'native'],
        ['Bitrate', `${rate ? (rate / 1000).toFixed(1) + ' Mb/s now' : '…'}${pic.kbps ? ` · ${(pic.kbps / 1000).toFixed(1)} Mb/s average` : ''}`],
        ['Buffer', `${ahead.toFixed(1)} s in the player`],
        ['Read-ahead', st?.spool ? `${st.spool.aheadSecs >= 0 ? st.spool.aheadSecs + ' s' : st.spool.aheadMB + ' MB'} cached on this PC · ${st.spool.connected ? 'downloading' : 'provider idle (cache full)'}${st.spool.reconnects ? ` · ${st.spool.reconnects} reconnects absorbed` : ''}` : st?.livebuf ? `${st.livebuf.bufferedSecs} s live ring (${st.livebuf.behindSecs > 3 ? `${fmtTime(st.livebuf.behindSecs)} behind live` : 'at live edge'}) · ${st.livebuf.memMB} MB${st.livebuf.reconnects ? ` · ${st.livebuf.reconnects} reconnects absorbed` : ''}` : o.live ? 'live (direct)' : 'direct'],
        ['Dropped frames', q.totalVideoFrames ? `${q.droppedVideoFrames} of ${q.totalVideoFrames}` : '…'],
        ['Sound', a ? (ch >= 6 && SURROUND_OUT ? `5.1 surround (from ${(a.codec || '').toUpperCase()} ${ch >= 8 ? '7.1' : '5.1'})` : ch >= 6 ? `Stereo mix of ${(a.codec || '').toUpperCase()} ${ch >= 8 ? '7.1' : '5.1'} (your output is stereo)` : `Stereo (from ${(a.codec || '').toUpperCase()}${ch === 1 ? ' mono' : ''})`) : o.live ? 'Stereo' : '…'],
        ['Delivery', `${cur().tx ? 'Converted (compatibility mode)' : 'Original stream, untouched'}${mseActive ? ' · MSE' : ''}${cur().label || o.label ? ` · ${esc(cur().label || o.label)}` : ''}`],
        ...(o.kind === 'series' ? [['Skip data', probe?.skipSource ? `${esc(probe.skipSource)}${SKIPS.some((x) => x.uncertain) ? ' (manual — cuts conflict)' : ''}${SKIPS.some((x) => x.kind === 'ending') ? ' · C-part preserved' : ''}` : 'None']] : []),
      ];
      box.innerHTML = `<b>Playback info</b><button class="stats-x" aria-label="Close">×</button>` + rows.map(([k, v]) => `<div><span>${k}</span><span>${v}</span></div>`).join('');
      $('.stats-x', box).onclick = () => toggleStats();
    };
    draw(); clearInterval(statsTimer); statsTimer = setInterval(draw, 1000);
  }

  // ---- OLED care: tiny periodic picture shift, and a black screen when paused for a while
  const oled = () => localGet('oled', '1') === '1';
  function startOledCare() {
    clearInterval(shiftTimer); if (!oled()) return;
    const steps = [[0, 0], [1, 0], [1, 1], [0, 1], [-1, 1], [-1, 0], [-1, -1], [0, -1], [1, -1]]; let i = 0;
    shiftTimer = setInterval(() => { i = (i + 1) % steps.length; const [x, y] = steps[i]; el.style.setProperty('--shift', `${x}px, ${y}px`); }, 120000);
  }
  function pausedSaver() {
    clearTimeout(saverTimer); if (!oled() || !video) return;
    saverTimer = setTimeout(() => { if (video?.paused && o && !casting && !pro) Saver.show(); }, 180000);
  }

  // ---- pro player (mpv): exact HDR / Dolby Vision, display-synced motion
  async function proInfo() { if (!proInfo.c || Date.now() - proInfo.t > 60000) { proInfo.c = await api('/api/mpv').catch(() => ({})); proInfo.t = Date.now(); } return proInfo.c; }
  function proRule(pic) {
    const r = localGet('pro', 'hdr4k');
    if (r === 'never' || !pic) return false;
    if (r === 'all') return true;
    if (r === 'hdr4k' && pic.hdr && (pic.h >= 2000 || pic.w >= 3600)) return true;
    return pic.hdr === 'DV' || pic.dv === 5;
  }
  async function toPro() {
    const p = await proInfo(); if (!p.available) { toast('The pro player isn’t installed. Settings › Picture & display'); return false; }
    const a = cur(); const ext = a.id === o.id ? o.ext : a.ext; const pic = probe?.pic || {};
    const curAud = probe?.audio?.[audio];
    const body = {
      kind: o.catchup ? 'catchup' : o.kind, id: String(a.id), ext, t: o.live ? 0 : pos(),
      a: audio, sub: subSel, useExtSub: subSel === -3,
      title: o.title || '', subTitle: o.sub || '', dur: duration,
      fps: pic.fps || o.fps || 0, hdr: !!pic.hdr, dv: pic.dv || 0, bits: pic.bits || (pic.hdr ? 10 : 8),
      h: pic.h || 0, w: pic.w || 0, ch: curAud?.ch || 2, sc: SURROUND_OUT,
      night: localGet('night', '0') === '1', deint: !!(o.live && localGet('di.' + o.id, '0') === '1'),
      isAnime: !!o.isAnime, skips: SKIPS, credits, creditsEnd, creditsMixed, hasPostCredits,
      hasNext: !!o.next, nextLabel: o.next?.label || '', autoplay: localGet('autoplay', '1') === '1',
      autoskip: localGet('autoskip', '1') === '1',
      autoskipRecap: localGet('autoSkipRecap', '1') === '1',
      autoskipCredits: localGet('autoSkipCredits', '1') === '1',
      audioDelay: +localGet('ad.' + sk(), 0) || 0, subDelay: +localGet('subshift.' + sk(), 0) || 0,
      progress: o.live ? {} : { key: o.progressKey, item: o.item, tmdb: o.tmdb, historyKind: o.historyKind, historyTitle: o.historyTitle },
    };
    if (o.catchup) { body.cs = o.catchup.cs + Math.floor(offset / 60) * 60000; body.durMin = o.catchup.dur; }
    offset = body.t; pro = { since: Date.now(), gen: 0 };
    clearTimeout(stallTimer); clearTimeout(stallTimer2); stallAt = 0;
    Mse.destroy();
    if (video) { video.pause(); video.removeAttribute('src'); video.load(); }
    spin(false);
    try {
      const res = await post('/api/mpv/play', body);
      if (pro) pro.gen = res?.gen || 0;
    } catch (e) { pro = null; toast(e.message); if (started) load(); return false; }
    showMsg(`<h4>${I.pro} Playing in the pro player</h4>
      <p>Full screen on your OLED with true black levels & display-synced motion. Space pauses, arrows seek, Q or Esc closes it.</p>
      <div class="btns" style="justify-content:center">
        <button class="btn btn-ghost btn-sm" id="proPause">Pause / Play</button>
        ${!o.live ? '<button class="btn btn-ghost btn-sm" id="proSkip">Skip intro</button>' : ''}
        ${o.next ? '<button class="btn btn-accent btn-sm" id="proNext">Next episode</button>' : ''}
        <button class="btn btn-ghost btn-sm" id="proBack">Play here instead</button>
      </div>`);
    $('#proPause').onclick = () => post('/api/mpv/control', { cmd: 'toggle' }).catch(() => {});
    if ($('#proSkip')) $('#proSkip').onclick = () => post('/api/mpv/control', { cmd: 'skip' }).catch(() => {});
    if ($('#proNext')) $('#proNext').onclick = async () => { await post('/api/mpv/stop', {}).catch(() => {}); clearInterval(proPoll); pro = null; o.next.go(); };
    $('#proBack').onclick = async () => { const at = offset; await post('/api/mpv/stop', {}).catch(() => {}); clearInterval(proPoll); pro = null; showMsg(null); offset = at; load(); };
    let proChecking = false;
    clearInterval(proPoll); proPoll = setInterval(async () => {
      if (proChecking) return;
      proChecking = true;
      try {
        const st = await api('/api/mpv').catch(() => null); if (!pro || !o) return clearInterval(proPoll);
        if (st?.session && !o.live) { offset = st.session.pos; if (st.session.dur) duration = st.session.dur; tick(); }
        if (st && !st.playing && Date.now() - pro.since > 4000) {
          let done = st.lastDone && (!pro.gen || st.lastDone.gen === pro.gen) ? st.lastDone : null;
          // Graceful retry once after 600ms if mpv reported not-playing with lastDone=null
          if (!done) {
            await new Promise((r) => setTimeout(r, 600));
            if (!pro || !o) return;
            const st2 = await api('/api/mpv').catch(() => null);
            if (!pro || !o) return;
            if (st2?.playing) return;
            if (st2?.lastDone && (!pro.gen || st2.lastDone.gen === pro.gen)) done = st2.lastDone;
          }
          clearInterval(proPoll); pro = null;
          if (done?.nextNow && o.next) {
            AUTOPLAY.chain = 0;
            toast(`Playing next: ${o.next.label}`);
            return o.next.go();
          }
          if (done?.finished && o.next && localGet('autoplay', '1') === '1' && AUTOPLAY.chain < 10) {
            AUTOPLAY.chain++;
            toast(`Up next: ${o.next.label}`);
            return o.next.go();
          }
          if (done?.error) toast(done.error);
          close();
        }
      } finally {
        proChecking = false;
      }
    }, 2000);
    return true;
  }

  // audio + subtitle menus for the current probe; keeps the given languages when switching versions
  const isEnAudioTrack = (a) => {
    const l = (a?.lang || '').toLowerCase(); const t = (a?.title || '').toLowerCase();
    if (/\b(commentary|audio description|descriptive)\b/.test(t)) return false;
    return /^en($|[-_]|g$)/.test(l) || (/\b(english|eng|dub|dubbed)\b/.test(t) && !/\bsub/.test(t));
  };
  function applyTracks(keepAudio, keepSub) {
    audio = 0; const as = $('#pAudio');
    if (probe?.audio?.length > 1) {
      as.hidden = false;
      as.innerHTML = probe.audio.map((a, i) => {
        const ln = langName(a.lang) || `Track ${i + 1}`;
        const showT = a.title && a.title.trim().toLowerCase() !== ln.toLowerCase() && !/^(english|eng|stereo|5\.1|2\.0)$/i.test(a.title.trim());
        return `<option value="${i}">${esc(ln)}${showT ? ` · ${esc(a.title.slice(0, 20))}` : ''}${a.ch > 2 ? ' · 5.1' : ''}</option>`;
      }).join('');
      const keep = keepAudio ? probe.audio.findIndex((a) => a.lang === keepAudio) : -1;
      const en = probe.audio.findIndex(isEnAudioTrack);
      audio = keep >= 0 ? keep : en >= 0 ? en : 0; as.value = audio;
      as.onchange = () => { as.blur(); focusPlayer(); audio = +as.value; const at = pos(); offset = at; load({ target: at, splice: mseActive && started, why: 'Switching audio…' }); };
    } else if (as) as.hidden = true;
    const ss = $('#pSubs');
    if (ss && !o?.live) {
      const emb = probe?.subs || [];
      ss.hidden = false;
      ss.innerHTML = '<option value="-1">Subtitles off</option>'
        + (o?.extSub ? `<option value="-3">${esc(o.extSubLabel || 'English (Downloaded)')}</option>` : '')
        + emb.map((s) => `<option value="${s.i}">${esc(langName(s.lang) || `Track ${s.i + 1}`)}${s.forced ? ' (forced)' : ''}${s.title && !/^(english|eng)$/i.test(s.title) ? ` · ${esc(s.title.slice(0, 24))}` : ''}</option>`).join('')
        + '<option value="-2">🔍 Find English subtitles online…</option>'
        + '<option value="-4">✨ Generate AI Subtitles (Local GPU)</option>';
      ss.onchange = () => { ss.blur(); focusPlayer(); setSubs(+ss.value); };
      const pref = keepSub ?? localGet('subs', 'off'); const curAud = probe?.audio?.[audio]; const audioIsEn = isEnAudioTrack(curAud);
      const enSub = emb.find((s) => /^en/i.test(s.lang) && !s.forced) || emb.find((s) => /^en/i.test(s.lang));
      const signsSub = emb.find((s) => /^en/i.test(s.lang) && (s.forced || /signs|songs|forced/i.test(s.title || '')));
      let pick = -1;
      if (pref === 'ext' && o?.extSub) pick = -3;
      else if (pref !== 'off') pick = (emb.find((s) => s.lang === pref && !s.forced) || enSub)?.i ?? (o?.extSub ? -3 : -1);
      else if (curAud && !audioIsEn && curAud.lang && curAud.lang !== 'und' && (enSub || o?.extSub)) pick = enSub ? enSub.i : -3;
      else if (o?.isAnime && audioIsEn && signsSub) pick = signsSub.i;
      ss.value = pick; if (pick >= 0 || pick === -3 || subSel >= 0 || subSel === -3) setSubs(pick);
    } else { if (ss) ss.hidden = true; if (subSel >= 0 || subSel === -3) setSubs(-1); }
  }

  function clearDvrTimer() { clearInterval(dvrTimer); dvrTimer = null; }
  function syncDvrUi() {
    const b = $('#pRec', el); if (!b) return;
    if (dvrRec) {
      const secs = Math.max(0, Math.floor((Date.now() - dvrRec.startedAt) / 1000));
      b.classList.add('rec');
      b.innerHTML = `<span class="rec-dot"></span>REC ${fmtTime(secs)}`;
      b.title = 'Stop DVR recording';
    } else {
      b.classList.remove('rec');
      b.textContent = '● REC';
      b.title = 'Record live channel (DVR)';
    }
  }
  async function startDvr(retroSecs = 0) {
    if (!o?.live) return;
    const chId = cur().id;
    try {
      const r = await post('/api/dvr/start', { id: chId, channelId: chId, title: o.title || 'Live TV', epg: o.epg || '', retroSecs: Math.max(0, Math.round(retroSecs || 0)) });
      dvrRec = { id: r.id || chId, channelId: chId, startedAt: Date.now() - (r.retroSecs || retroSecs || 0) * 1000, file: r.file || '' };
      clearDvrTimer();
      dvrTimer = setInterval(syncDvrUi, 1000);
      syncDvrUi();
      showOsd(retroSecs > 0 ? `Recording (+${fmtTime(retroSecs)} buffer)` : 'Recording started');
    } catch (err) {
      toast(err.status === 404 ? 'Live DVR recording requires the updated server' : (err.message || 'Could not start recording'));
    }
  }
  async function stopDvr() {
    if (!dvrRec) return;
    const rec = dvrRec;
    dvrRec = null; clearDvrTimer(); syncDvrUi();
    try {
      const r = await post('/api/dvr/stop', { id: rec.id, channelId: rec.channelId });
      toast(`Recording saved${r?.file ? `: ${r.file}` : ''}`);
    } catch (err) {
      toast(err.message || 'Stopped recording');
    }
  }
  function toggleDvr() {
    if (!o?.live) return;
    const old = $('.dvr-menu', el); if (old) { old.remove(); return; }
    if (dvrRec) return stopDvr();
    if (liveBufSecs > 15) {
      closePopovers();
      const buf = Math.min(Math.floor(liveBufSecs), 1800);
      const m = document.createElement('div'); m.className = 'cast-menu dvr-menu';
      m.innerHTML = `<b>Record ${esc(o.title)}</b>
        <button type="button" id="dvrNow"><span>● Record from now<small>Start live DVR capture immediately</small></span></button>
        <button type="button" id="dvrRetro"><span>⏪ Include last ${fmtTime(buf)}<small>Save retroactive live buffer + keep recording</small></span></button>`;
      el.appendChild(m);
      $('#dvrNow', m).onclick = () => { m.remove(); startDvr(0); focusPlayer(); };
      $('#dvrRetro', m).onclick = () => { m.remove(); startDvr(buf); focusPlayer(); };
      wakeControls();
      setTimeout(() => document.addEventListener('click', function off(e) {
        if (!m.isConnected) return document.removeEventListener('click', off);
        if (!m.contains(e.target) && !e.target.closest('#pRec')) { m.remove(); document.removeEventListener('click', off); wakeControls(); }
      }), 0);
      return;
    }
    startDvr(0);
  }

  async function open(opts) {
    const my = ++openGen;
    try { document.activeElement?.blur?.(); } catch {}
    stopHero(); clearTimeout(detailTrailerT);
    $$('.hero-trailer, .trailer-frame').forEach((f) => f.remove()); $('#heroSound')?.remove();
    document.body.classList.add('playing');
    if (o) await close(true);
    if (my !== openGen) return;
    o = opts; credits = null; creditsEnd = null; creditsMixed = false; hasPostCredits = false; skipped = new Set(); userDismissed = new Set(); creditsDismissed = false; clearUndoSkip(); stalls = []; watchedSecs = 0; watchedAt = 0; offset = opts.resume > 5 ? opts.resume : 0; duration = opts.duration || 0; audio = 0; probe = null; historySent = false; nextShown = false; prespoolSent = false;
    subSel = -1; subTrack = null; clearInterval(subTimer); clearAiSubJob(); clearDvrTimer(); dvrRec = null;
    mseFailed = false; mseActive = false; mseTarget = offset; mseSessionStart = offset;
    playRate = 1; zoomMode = 'fit'; draggingSeek = false; hoveringCtrl = false;
    clearTimeout(pendingSeekTimer); pendingSeekTimer = null; pendingSeekTarget = null; pendingLiveBack = null;
    clearTimeout(hudSeekTimer); hudSeekDir = 0; hudSeekAccum = 0; clearTimeout(clickTimer); clickTimer = null; lastPosStateAt = 0;
    liveBack = 0; liveBufSecs = 0; liveBehind = 0; livePausedAt = 0; clearInterval(livePollTimer); clearTimeout(liveEpgTimer); clearTimeout(liveBannerTimer);
    attempts = buildAttempts(); ai = 0; retries = 0; failed = new Set(); hitches = 0; lastQ = null; lastFreeze = 0;
    const canLast = o.live && LAST_LIVE && LAST_LIVE.id !== o.id;
    const hasPip = !!(document.pictureInPictureEnabled && !document.pictureInPictureElement);
    const savedVol = Math.max(0, Math.min(1, +localGet('vol', 1)));
    const savedMuted = localGet('muted', '0') === '1';
    el.tabIndex = -1;
    el.innerHTML = `<video playsinline></video>
      <div class="spin-wrap"><div class="spinner"></div><div class="spin-lbl">Loading…</div></div>
      <div class="p-osd-center" aria-hidden="true"></div>
      <div class="p-osd-seek left" aria-hidden="true"></div>
      <div class="p-osd-seek right" aria-hidden="true"></div>
      <div class="p-osd-vol" aria-hidden="true"></div>
      <div class="p-msg" hidden></div>
      ${o.live ? '<div class="live-banner" id="pLiveBanner"></div><aside class="live-drawer" id="pDrawer" hidden></aside>' : ''}
      <div class="pc">
        <div class="pc-top"><button class="pbtn" id="pBack" title="Back (Esc)">${I.back}</button>
          <div style="min-width:0;flex:1"><div class="pt">${esc(o.title)}</div><div class="ps" id="pSub">${o.live ? '<span class="live-badge"><span class="live-dot"></span>LIVE</span> ' : ''}${esc(o.sub || '')}</div></div>
          <button class="pbtn" id="pHelp" title="Keyboard shortcuts (?)">${I.help}</button></div>
        <div class="pc-bottom">
          <div class="seek" id="pSeek" tabindex="0" role="slider" aria-label="Seek"><div class="track"><div class="buf"></div><div class="segs"></div><div class="fill"></div><div class="knob"></div></div><div class="hover-t"></div></div>
          <div class="pc-row">
            <button class="pbtn" id="pPlay" title="Play/Pause (Space / K)">${I.pause}</button>
            <button class="pbtn" id="pRew" title="Back 10s (← / J · Shift: 30s)">${I.rew}</button><button class="pbtn" id="pFwd" title="Forward 10s (→ / L · Shift: 30s)">${I.fwd}</button>
            ${!o.live ? `<button class="pbtn pbtn-txt" id="pReplay" title="What did they say? Replay 8s with subtitles (R)">⟲ 8s</button>` : ''}
            ${o.live ? `<button class="live-edge" id="pLiveEdge" title="Jump to live (End)"><span class="live-dot"></span>LIVE</button>` : ''}
            ${o.live && o.channels?.length > 1 ? `<button class="pbtn" id="pPrev" title="Previous channel (PgUp / Shift+↑)">${I.left}</button><button class="pbtn" id="pNext" title="Next channel (PgDn / Shift+↓)">${I.right}</button>` : ''}
            ${canLast ? `<button class="pbtn pbtn-txt" id="pLast" title="Last channel: ${esc(LAST_LIVE.title)} (L)">⇄ Last</button>` : ''}
            ${o.live ? `<button class="pbtn pbtn-txt" id="pGuide" title="Mini-guide & live scores (G)">Guide</button>` : ''}
            ${o.live ? `<button class="pbtn pbtn-txt" id="pRec" title="Record live channel (DVR)">● REC</button>` : ''}
            <div class="vol-wrap"><button class="pbtn" id="pMute" title="Mute (M · Scroll wheel for volume)">${volIcon(savedVol, savedMuted)}</button><input class="vol" id="pVol" type="range" min="0" max="1" step="0.01" value="${savedMuted ? 0 : savedVol}" title="Volume (Scroll wheel or ↑ / ↓)" aria-label="Volume"></div>
            <button type="button" class="time-btn" id="pTime" title="Click to toggle remaining time"></button><span class="spacer"></span>
            <select id="pSubs" hidden title="Subtitles (C)"></select>
            <select id="pAudio" hidden title="Audio track (A)"></select>
            <select id="pSrc" hidden title="${o.live ? 'Backup feeds' : 'Version'}"></select>
            ${o.live ? `<button class="pbtn pbtn-txt ${localGet('di.' + o.id, '0') === '1' ? 'on' : ''}" id="pDeint" title="Deinterlace 1080i broadcast to 50fps">50fps</button>` : ''}
            <button class="pbtn pbtn-txt ${localGet('night', '0') === '1' ? 'on' : ''}" id="pNight" title="Clear Voice / Night Mode (V)">Voice</button>
            <button class="pbtn ${o.live ? '' : 'pbtn-txt'}" id="pTune" title="${o.live ? 'Picture fit & shortcuts (Z)' : 'Playback speed, picture fit & subtitle size (< / > / Z)'}">${o.live ? I.tune : '<span class="tune-lbl">1×</span>'}</button>
            ${o.next ? `<button class="pbtn" id="pNextEp" title="Next episode (N)">${I.next}</button>` : ''}
            <button class="pbtn" id="pInfo" title="Playback info (I)">${I.info}</button>
            <button class="pbtn" id="pPro" title="Open in the pro player (P)" hidden>${I.pro}</button>
            ${hasPip ? `<button class="pbtn" id="pPip" title="Picture-in-Picture (Shift+P)">${I.pip}</button>` : ''}
            <button class="pbtn" id="pCast" title="Play on a TV">${I.cast}</button>
            <button class="pbtn" id="pFull" title="Fullscreen (F)">${document.fullscreenElement ? I.unfull : I.full}</button>
          </div></div></div>`;
    el.hidden = false;
    el.classList.toggle('spoiler-free', !!o.spoilerFree);
    el.classList.remove('zoom-fill', 'zoom-110');
    el.classList.toggle('sub-sm', subSize === 'sm');
    el.classList.toggle('sub-lg', subSize === 'lg');
    document.body.style.overflow = 'hidden'; SKIPS = [];
    if (!PLAYER_STATE.open) {
      if (history.state?.modal) history.replaceState({ player: true }, '', location.href); else history.pushState({ player: true }, '', location.href);
      PLAYER_STATE.open = true;
    }
    video = $('video', el);
    video.volume = savedVol;
    video.muted = savedMuted;
    syncVolUi();
    wire(); startOledCare(); focusPlayer();
    proInfo().then((p) => { if (p.available && $('#pPro')) $('#pPro').hidden = false; });
    if (o.live) {
      api('/api/dvr/status').then((ds) => {
        if (!o?.live || o !== opts || !ds) return;
        const active = ds.active || ds.recording || (Array.isArray(ds.recordings) ? ds.recordings.find((x) => +x.channelId === +o.id || +x.id === +o.id) : null);
        if (active && (+active.channelId === +o.id || +active.id === +o.id || ds.recording === true)) {
          dvrRec = { id: active.id || o.id, channelId: o.id, startedAt: active.startedAt || (Date.now() - (active.secs || 0) * 1000) };
          clearDvrTimer(); dvrTimer = setInterval(syncDvrUi, 1000); syncDvrUi();
        }
      }).catch(() => {});
    }
    if (typeof pushRemoteState === 'function') pushRemoteState(true);
    spin(true, 'Loading…');
    if (!o || o !== opts) return;
    if (!o.live && !o.catchup) {
      // Series: resolve ranked/picked version BEFORE probing so we open directly on the best copy
      if (o.kind === 'series' && o.sid && o.season != null && o.epn != null) {
        const list = await api(`/api/ep-alts/${o.sid}/${o.season}/${o.rawN ?? o.epn}`).catch(() => null);
        if (!o || o !== opts) return;
        if (Array.isArray(list) && list.length) {
          const best = list[0];
          o.id = best.id; o.ext = best.ext || o.ext; o.label = best.label; o.cat = best.cat; o.catId = best.catId || ''; o.subMarket = !!best.subMarket;
          o.alternates = list.slice(1);
          attempts = buildAttempts(); ai = 0;
        }
      }
      const pq = new URLSearchParams();
      if (o.ext) pq.set('ext', o.ext);
      if (o.kind === 'series' && o.sid) {
        pq.set('sid', o.sid); pq.set('s', o.season); pq.set('n', o.epn);
        if (o.abs) pq.set('abs', o.abs);
        if (o.tmdb) pq.set('tmdb', o.tmdb);
        if (o.imdb) pq.set('imdb', o.imdb);
        if (o.isAnime) pq.set('anime', '1');
      }
      const p0 = await api(`/api/probe/${o.kind}/${o.id}?${pq}`).catch(() => null);
      if (!o || o !== opts) return;
      probe = p0;
      // English-audio safety gate: if a cross-market 4K copy ever lacks English audio, fall back automatically to native EN
      if (!o.isAnime && cur().subMarket && probe?.audio?.length > 0 && probe?.hasEnAudio === false) {
        const nx = attempts.findIndex((a, idx) => idx > ai && !a.tx && !a.alt && !a.subMarket);
        if (nx >= 0) {
          toast('No English audio on international 4K copy — switching to English version');
          renderSources(); setMediaSession();
          return useAttempt(nx);
        }
      }
      if (o.isAnime && probe?.hasEnAudio === false && !o.allowSubOnly) {
        spin(false);
        const langs = [...new Set((probe.audio || []).map((a) => langName(a.lang) || a.lang || 'Unknown'))].join(', ');
        showMsg(`<h4>No English dub on this ${o.kind === 'series' ? 'episode' : 'film'} yet</h4>
          <p>Audio tracks found: ${esc(langs || 'Japanese only')}. It has been marked sub-only and hidden from your Anime lists.</p>
          <div class="btns" style="justify-content:center">
            ${o.next ? '<button class="btn btn-primary btn-sm" id="anNext">Next dubbed episode</button>' : ''}
            <button class="btn btn-accent btn-sm" id="anWatchDub">🔔 Notify when dubbed</button>
            <button class="btn btn-ghost btn-sm" id="anAnyway">Play original audio anyway</button>
            <button class="btn btn-ghost btn-sm" id="anClose">Close</button>
          </div>`);
        if ($('#anNext')) $('#anNext').onclick = () => o.next.go();
        $('#anWatchDub').onclick = async () => {
          await post('/api/anime/dub-watch', {
            key: o.progressKey, kind: o.kind, id: o.id, sid: o.sid, season: o.season, epNum: o.epn,
            ext: o.ext, title: o.title, sub: o.sub, on: true,
          }).catch(() => {});
          $('#anWatchDub').textContent = '🔔 Watching for dub';
          $('#anWatchDub').disabled = true;
          toast(`We'll alert you as soon as the English dub drops for ${o.title}`);
        };
        $('#anClose').onclick = () => close();
        const proceed = await new Promise((resolve) => {
          $('#anAnyway').onclick = () => { o.allowSubOnly = true; showMsg(null); spin(true); resolve(true); };
        });
        if (!proceed || !o || o !== opts) return;
      }
      if (probe?.duration) duration = probe.duration;
      applyTracks(null, null);
      if (offset > 5) toast(`Resuming from ${fmtTime(offset)}`);
      SKIPS = (probe?.skips || []).map((x) => ({ ...x }));
      if (!SKIPS.length && Array.isArray(probe?.intro)) {
        SKIPS.push({ s: Math.max(0, probe.intro[0]), e: probe.intro[1], title: 'Intro', kind: 'intro', auto: true });
      }
      if (!SKIPS.length && Array.isArray(probe?.recap)) {
        SKIPS.unshift({ s: Math.max(0, probe.recap[0]), e: probe.recap[1], title: 'Recap', kind: 'recap', auto: !o.isAnime });
      }
      credits = typeof probe?.credits === 'number' ? probe.credits : null;
      creditsEnd = typeof probe?.creditsEnd === 'number' ? probe.creditsEnd : null;
      creditsMixed = !!probe?.creditsMixed;
      hasPostCredits = !!probe?.hasPostCredits;
      renderSeekMarkers();
      const ql = qLabel(probe?.pic, probe?.audio); if (ql) $('#pSub').insertAdjacentHTML('beforeend', ` <span class="qbadge">${esc(ql)}</span>`);
      if (proRule(probe?.pic) && (await proInfo()).available) { renderSources(); setMediaSession(); if (await toPro()) return; }
      if (probe?.pic?.hdr === 'DV' && !(await proInfo()).available) toast('Dolby Vision: colours may look off in this player. The pro player shows it properly (Settings › Picture & display)');
      if (!o || o !== opts) return;
      await matchDisplay(probe?.pic?.fps, !!probe?.pic?.hdr);
      if (!o || o !== opts) return;
    } else if (o.live) {
      // Discover backup feeds & channel metadata in background without delaying start
      api(`/api/live/channel/${o.id}`).then((ch) => {
        if (!o || o !== opts || !ch) return;
        if (!o.epg && ch.epg) o.epg = ch.epg;
        if (!o.logo && ch.logo) o.logo = ch.logo;
        if (!o.alternates?.length && ch.feeds?.length > 1) {
          o.label = ch.feeds.find((f) => f.id === o.id)?.label || 'Main';
          o.alternates = ch.feeds.filter((f) => f.id !== o.id).map((f) => ({ id: f.id, label: f.label || f.channel, channel: f.channel }));
          const tail = attempts.findIndex((a) => a.alt);
          const add = o.alternates.filter((v) => !attempts.some((a) => a.id === v.id)).map((v) => ({ id: v.id, label: v.label, channel: v.channel }));
          attempts.splice(tail < 0 ? attempts.length : tail, 0, ...add);
          renderSources();
        }
        updateLiveBanner(true);
      }).catch(() => {});
      updateLiveBanner(true);
      livePollTimer = setInterval(pollLiveBuf, 4000);
      const f = +localGet('fps.' + o.id, 0); o.fps = f || 0; await matchDisplay(f, false); if (!o || o !== opts) return;
    } else {
      const f = +localGet('fps.' + o.id, 0); o.fps = f || 0; await matchDisplay(f, false); if (!o || o !== opts) return;
    }
    renderSources(); setMediaSession();
    load({ target: offset });
    saveTimer = setInterval(save, 10000);
  }

  function videoBufAhead() {
    if (!video) return 0;
    try {
      for (let i = 0; i < video.buffered.length; i++) {
        if (video.buffered.start(i) <= video.currentTime + 0.5 && video.buffered.end(i) > video.currentTime) {
          return video.buffered.end(i) - video.currentTime;
        }
      }
    } catch {}
    return 0;
  }
  async function pollLiveBuf() {
    if (!o?.live) return;
    const st = await api('/api/streamstats').catch(() => null);
    if (!o?.live || !st?.livebuf) return;
    liveBufSecs = st.livebuf.bufferedSecs || 0;
    if (!video?.paused && started) {
      // At 1x playback speed, you stay `liveBack` seconds behind a 1x broadcast until you seek or Go Live.
      const serverBehind = st.livebuf.behindSecs || 0;
      liveBehind = liveBack > 0.5 ? Math.max(serverBehind, Math.min(liveBufSecs || liveBack, liveBack)) : serverBehind;
    } else if (livePausedAt) {
      liveBehind = Math.min(liveBufSecs, liveBack + (Date.now() - livePausedAt) / 1000);
    }
    updateLiveEdgeUi();
  }
  function updateLiveEdgeUi() {
    const b = $('#pLiveEdge'); if (!b || !o?.live) return;
    const behind = Math.round(liveBehind);
    if (behind > 4) {
      b.classList.add('behind');
      b.innerHTML = `−${fmtTime(behind)} · Go Live`;
    } else {
      b.classList.remove('behind');
      b.innerHTML = '<span class="live-dot"></span>LIVE';
    }
    const f = $('#pSeek .fill'); const k = $('#pSeek .knob');
    if (f && k) {
      const pct = liveBufSecs > 5 ? Math.max(0, Math.min(100, ((liveBufSecs - liveBehind) / liveBufSecs) * 100)) : 100;
      f.style.width = pct + '%'; k.style.left = pct + '%';
    }
    const tm = $('#pTime');
    if (tm) tm.textContent = liveBufSecs > 5 ? (behind > 4 ? `−${fmtTime(behind)} / ${fmtTime(liveBufSecs)} buf` : `Live · ${fmtTime(liveBufSecs)} buf`) : '';
  }
  function seekLiveToBack(backSecs) {
    if (!o?.live) return;
    const maxB = Math.max(0, liveBufSecs - 2);
    const clamped = Math.max(0, Math.min(maxB, backSecs));
    liveBack = clamped; liveBehind = clamped; livePausedAt = 0;
    updateLiveEdgeUi();
    toast(clamped <= 2 ? 'Jumped to live' : `Timeshift: −${fmtTime(clamped)}`);
    loadDirect();
  }

  async function updateLiveBanner(flash = false) {
    if (!o?.live) return;
    clearTimeout(liveEpgTimer);
    const list = await api(`/api/epg/${o.id}`).catch(() => []);
    if (!o?.live) return;
    const now = Date.now();
    const ci = list.findIndex((p) => p.s <= now && p.e > now);
    const curP = ci >= 0 ? list[ci] : null;
    const nextP = ci >= 0 ? list[ci + 1] : list.find((p) => p.s > now);
    if (curP) {
      $('#pSub').innerHTML = `<span class="live-badge"><span class="live-dot"></span>LIVE</span> ${esc(curP.title)} · until ${fmtClock(curP.e)}${nextP ? ` · Next: ${esc(nextP.title)}` : ''}`;
      const banner = $('#pLiveBanner');
      if (banner) {
        const pct = Math.min(100, Math.max(0, ((now - curP.s) / (curP.e - curP.s)) * 100));
        banner.innerHTML = `<div class="lb-head">${o.logo ? `<img src="${esc(o.logo)}" alt="" onerror="this.remove()">` : ''}<b>${esc(o.title)}</b><span class="lb-time">${fmtClock(curP.s)} – ${fmtClock(curP.e)}</span></div>
          <div class="lb-now">${esc(curP.title)}</div>
          <div class="lb-prog"><i style="width:${pct.toFixed(1)}%"></i></div>
          ${nextP ? `<div class="lb-next">Up next at ${fmtClock(nextP.s)}: <b>${esc(nextP.title)}</b></div>` : ''}`;
        banner.classList.add('ready');
        if (flash) {
          banner.classList.add('on');
          clearTimeout(liveBannerTimer);
          liveBannerTimer = setTimeout(() => banner.classList.remove('on'), 5000);
        }
      }
      // Schedule automatic rollover when current programme finishes
      const waitMs = Math.max(15000, Math.min(3600e3, curP.e - now + 2000));
      liveEpgTimer = setTimeout(() => updateLiveBanner(true), waitMs);
    }
  }

  function save(beacon = false) {
    if (!o || o.live || (!started && !casting && !pro)) return;
    let p = pos(); if (!Number.isFinite(p) || p < 5 || !duration) return;
    p = Math.min(duration, Math.max(0, p));
    const body = { key: o.progressKey, pos: p, dur: duration, item: o.item, tmdb: o.tmdb };
    if (!historySent && p > 60 && o.historyKind) { body.historyKind = o.historyKind; body.historyTitle = o.historyTitle; historySent = true; }
    if (beacon) navigator.sendBeacon?.('/api/progress', new Blob([JSON.stringify(body)], { type: 'application/json' }));
    else post('/api/progress', body).catch(() => {});
  }

  function paintSeekPreview(targetSec) {
    if (!o || o.live || !duration) return;
    const pct = Math.max(0, Math.min(100, (targetSec / duration) * 100));
    const f = $('#pSeek .fill', el); const k = $('#pSeek .knob', el);
    if (f) f.style.width = pct + '%';
    if (k) k.style.left = pct + '%';
    renderTimeText(targetSec);
  }
  function clearUndoSkip() {
    if (lastAutoSkip?.timer) clearTimeout(lastAutoSkip.timer);
    lastAutoSkip = null;
    $('#pUndoSkip', el)?.remove();
  }
  function showUndoSkip(sk, fromPos, label) {
    clearUndoSkip();
    const msg = sk.kind === 'ending' ? `Skipped ${label} → Post-Credits` : `Skipped ${label}`;
    const btn = document.createElement('div');
    btn.id = 'pUndoSkip';
    btn.className = 'skip-undo';
    btn.innerHTML = `<span>${esc(msg)}</span><button type="button" class="btn btn-ghost btn-sm" id="pUndoBtn">Undo <kbd>U</kbd></button>`;
    el.appendChild(btn);
    $('#pUndoBtn', btn).onclick = (e) => { e.stopPropagation(); undoLastSkip(); };
    const timer = setTimeout(clearUndoSkip, 7500);
    lastAutoSkip = { sk, from: fromPos, label, timer };
  }
  function undoLastSkip() {
    if (!lastAutoSkip || !o) return false;
    const { sk, from, label } = lastAutoSkip;
    userDismissed.add(sk);
    clearUndoSkip();
    showOsd(`Undo ${label} · ${fmtTime(from)}`);
    seekTo(from, { byUser: true });
    focusPlayer();
    return true;
  }
  function seekTo(t, { byUser = false } = {}) {
    clearTimeout(pendingSeekTimer); pendingSeekTimer = null; pendingSeekTarget = null; pendingLiveBack = null;
    if (o.live) {
      if (liveBufSecs <= 5) return showOsd('Live buffer is still filling…');
      return seekLiveToBack(Math.max(0, liveBufSecs - t));
    }
    if (!duration) return;
    const prev = pos();
    const clamped = Math.max(0, Math.min(duration - 3, t));
    if (byUser) {
      clearUndoSkip();
      // Manual scrub-back / seek-into immunity: if the viewer explicitly seeks into or rewinds back
      // before a skip segment, never auto-skip them forward over it again (manual button stays available).
      for (const c of SKIPS) {
        if ((clamped >= c.s - 2 && clamped < c.e) || (clamped < prev - 1 && prev >= c.s && clamped < c.e)) {
          userDismissed.add(c);
        }
      }
      if (!creditsDismissed && credits && clamped < credits - 15) nextShown = false;
    }
    if (casting) { offset = clamped; tick(); return castCtl('seek', { t: offset }); }
    // MSE instant in-buffer seek (0ms, zero ffmpeg restart, zero subtitle reset!)
    if (mseActive && Mse.isBuffered(clamped)) {
      video.currentTime = clamped;
      tick();
      return;
    }
    const keepPaused = started && video.paused;
    offset = clamped;
    load({ target: clamped, keepPaused, why: `Seeking to ${fmtTime(clamped)}…` });
    tick();
    if (keepPaused && !mseActive) video.addEventListener('playing', () => video.pause(), { once: true });
  }
  function seekBy(d) {
    if (!o || !d) return;
    const dir = d > 0 ? 1 : -1;
    if (hudSeekDir !== dir) { hudSeekDir = dir; hudSeekAccum = 0; }
    hudSeekAccum += d;
    clearTimeout(hudSeekTimer);
    hudSeekTimer = setTimeout(() => { hudSeekDir = 0; hudSeekAccum = 0; }, 750);

    if (o.live) {
      if (liveBufSecs <= 5) return showOsd('Live buffer is still filling…');
      const baseB = pendingLiveBack !== null ? pendingLiveBack : liveBehind;
      const nextB = Math.max(0, Math.min(Math.max(0, liveBufSecs - 2), baseB - d));
      pendingLiveBack = nextB;
      showSeekOsd(dir, hudSeekAccum, null, nextB <= 2 ? 'Live' : `−${fmtTime(nextB)}`);
      clearTimeout(pendingSeekTimer);
      pendingSeekTimer = setTimeout(() => {
        const b = pendingLiveBack; pendingLiveBack = null; pendingSeekTimer = null;
        if (o?.live && b !== null) seekLiveToBack(b);
      }, 240);
      return;
    }

    if (!duration) return;
    const curPos = pendingSeekTarget !== null ? pendingSeekTarget : pos();
    const target = Math.max(0, Math.min(duration - 3, curPos + d));
    showSeekOsd(dir, hudSeekAccum, target);
    paintSeekPreview(target);

    if (casting) {
      pendingSeekTarget = target;
      clearTimeout(pendingSeekTimer);
      pendingSeekTimer = setTimeout(() => {
        const t = pendingSeekTarget; pendingSeekTarget = null; pendingSeekTimer = null;
        if (o && t !== null) seekTo(t, { byUser: true });
      }, 200);
      return;
    }

    // 0ms instant in-buffer MSE seek!
    if (mseActive && pendingSeekTimer === null && Mse.isBuffered(target)) {
      seekTo(target, { byUser: true });
      return;
    }

    // Out-of-buffer seek: batch rapid taps over 220ms into one clean server seek
    pendingSeekTarget = target;
    clearTimeout(pendingSeekTimer);
    pendingSeekTimer = setTimeout(() => {
      const t = pendingSeekTarget; pendingSeekTarget = null; pendingSeekTimer = null;
      if (o && t !== null) seekTo(t, { byUser: true });
    }, 220);
  }

  function skipLabelFor(s) {
    if (!s) return o?.isAnime ? 'Opening' : 'Intro';
    if (s.kind === 'ending') return s.mixed ? 'Ending (Mixed)' : 'Ending';
    const t = s.title || '';
    if (/recap/i.test(t) && /intro|opening/i.test(t)) return o?.isAnime ? 'Recap + Opening' : 'Recap + Intro';
    if (s.kind === 'recap' || /recap|previously/i.test(t)) return 'Recap';
    const base = o?.isAnime ? 'Opening' : 'Intro';
    return s.mixed ? `${base} (Mixed)` : base;
  }
  function shouldAutoSkipSeg(sk) {
    if (!sk || sk.auto === false || skipped.has(sk) || userDismissed.has(sk)) return false;
    if (sk.kind === 'ending') return localGet('autoSkipCredits', '1') === '1';
    if (sk.kind === 'recap') return localGet('autoSkipRecap', '1') === '1';
    if (/intro|opening|recap|previously/i.test(sk.title || '')) return localGet('autoskip', '1') === '1';
    return false;
  }

  function tick() {
    if (!o) return;
    if (o.live) { updateLiveEdgeUi(); return; }
    const p = pos();
    updateBufferedBar();
    const sk = SKIPS.find((c) => p >= c.s && p < c.e - 2);
    let btn = $('#pSkip', el);
    if (sk && !btn) { btn = document.createElement('button'); btn.id = 'pSkip'; btn.className = 'btn btn-primary skip-btn'; el.appendChild(btn); }
    if (btn) {
      if (!sk) btn.remove();
      else {
        const lbl = skipLabelFor(sk);
        btn.textContent = sk.kind === 'ending' ? `Skip ${lbl} → Post-Credits` : `Skip ${lbl}`;
        btn.onclick = () => { skipped.add(sk); clearUndoSkip(); seekTo(sk.e); showOsd(`Skipped ${lbl}`); focusPlayer(); };
      }
    }
    if (sk && started && p >= sk.s + 0.12 && shouldAutoSkipSeg(sk)) {
      const fromPos = p;
      const lbl = skipLabelFor(sk);
      skipped.add(sk);
      seekTo(sk.e);
      showUndoSkip(sk, fromPos, lbl);
      return;
    }
    if (!draggingSeek && pendingSeekTarget === null) {
      if (duration) {
        const pct = Math.min(100, (p / duration) * 100);
        const f = $('#pSeek .fill', el); if (f) { f.style.width = pct + '%'; $('#pSeek .knob', el).style.left = pct + '%'; }
      }
      renderTimeText(p);
    }
    if (duration) {
      const edSeg = SKIPS.find((c) => c.kind === 'ending');
      let nextAt;
      if (credits && credits > duration * 0.55) {
        nextAt = credits;
      } else if (edSeg) {
        // Episode has an in-episode ED followed by a C-part epilogue: NEVER cut off the C-part!
        nextAt = Math.max(duration - 15, edSeg.e + 10);
      } else {
        nextAt = o.isAnime ? duration - 22 : duration - 40;
      }
      if (o.next && !nextShown && !creditsDismissed && p >= nextAt && p > 60) showNextUp();
      if (!o.catchup && !prespoolSent && o.next?.id && duration > 60 && (p >= duration * 0.85 || (credits && credits > duration * 0.55 && p >= credits - 15))) {
        prespoolSent = true;
        post('/api/prespool', { kind: 'series', id: o.next.id, ext: o.next.ext || '', sid: o.next.sid || o.sid, season: o.next.season, epn: o.next.epn, rawN: o.next.rawN }).catch(() => {});
      }
      const now = Date.now();
      if (now - lastPosStateAt > 4000 && navigator.mediaSession?.setPositionState) {
        lastPosStateAt = now;
        try { navigator.mediaSession.setPositionState({ duration, playbackRate: playRate || 1, position: Math.max(0, Math.min(duration, p)) }); } catch {}
      }
    }
  }

  // Up next at the end credits: counts down and plays the next episode (fast 3s auto-skip when
  // true end credits are detected and autoSkipCredits is on; gentle 10s on mixed-ed or unknown credits).
  function showNextUp() {
    nextShown = true;
    const auto = localGet('autoplay', '1') === '1'; const ask = AUTOPLAY.chain >= 10;
    const hasKnownCredits = !!(credits && duration && credits > duration * 0.55);
    const fastCredits = auto && !ask && hasKnownCredits && !creditsMixed && localGet('autoSkipCredits', '1') === '1';
    const canSkipEd = !!(hasPostCredits && creditsEnd && duration && creditsEnd < duration - 12 && pos() < creditsEnd - 4);
    const credNoun = o.isAnime ? (SKIPS.some((c) => c.kind === 'ending') ? 'credits' : 'ending') : 'credits';
    const n = document.createElement('div'); n.className = 'next-up';
    n.innerHTML = `${o.next.still ? `<img src="${esc(o.next.still)}" alt="">` : ''}<div><small>${ask ? 'Still watching?' : fastCredits ? `Skipping ${credNoun} · Up next` : 'Up next'}</small><b>${esc(o.next.label)}</b></div>
      <div class="nu-btns"><button class="btn btn-primary btn-sm" id="nuPlay">${I.play}<span>${ask ? 'Keep watching' : 'Play now'}</span></button>${canSkipEd ? '<button class="btn btn-ghost btn-sm" id="nuPost">Post-credits →</button>' : ''}<button class="btn btn-ghost btn-sm" id="nuStay">${credits ? `Watch ${credNoun}` : 'Dismiss'}</button></div>`;
    el.appendChild(n);
    let left = fastCredits ? 3 : 10; let t = null;
    const go = (byUser) => { clearInterval(t); if (byUser) AUTOPLAY.chain = 0; else AUTOPLAY.chain++; o.next.go(); };
    $('#nuPlay', n).onclick = () => go(true);
    if ($('#nuPost', n)) $('#nuPost', n).onclick = () => { clearInterval(t); creditsDismissed = true; n.remove(); showOsd('Skipped Ending → Post-Credits'); seekTo(creditsEnd); focusPlayer(); };
    $('#nuStay', n).onclick = () => { clearInterval(t); creditsDismissed = true; n.remove(); };
    if (auto && !ask) {
      const lab = $('#nuPlay span', n);
      const labelFor = (s) => (fastCredits ? `Skipping ${credNoun} in ${s}s` : `Next episode in ${s}`);
      lab.textContent = labelFor(left);
      t = setInterval(() => {
        if (!o || !n.isConnected) return clearInterval(t);
        if (video.paused) return;
        left--;
        lab.textContent = labelFor(left);
        if (left <= 0) go(false);
      }, 1000);
    }
  }

  function wire() {
    const pc = $('.pc', el);
    video.addEventListener('playing', () => {
      started = true; spin(false); clearTimeout(stallTimer); clearTimeout(stallTimer2);
      if (!o.live && playRate !== 1 && video.playbackRate !== playRate) { try { video.playbackRate = playRate; } catch {} }
      if (!o.live && !o.catchup && !baseKnown && !mseActive) learnStart();
      if (!reportedStart) { reportedStart = true; report('start', { ms: Math.round(performance.now() - loadT) }); if ((o.live || o.catchup) && !o.fps) measureFps(); }
      if (stallAt) { const d = performance.now() - stallAt; stallAt = 0; clearTimeout(stallTimer2); if (d > 2500) { report('stall'); const now = performance.now(); stalls = stalls.filter((x) => now - x < 180000); stalls.push(now); if (stalls.length >= 3) { stalls = []; switchSource('stutter'); } } }
    });
    video.addEventListener('loadedmetadata', () => {
      if (!o?.live && playRate !== 1) { try { video.playbackRate = playRate; } catch {} }
      updateBufferedBar();
    });
    video.addEventListener('progress', updateBufferedBar);
    video.addEventListener('waiting', () => {
      spin(true, started ? 'Buffering…' : 'Loading…');
      // only a real stall of THIS session's stream counts (not a teardown, a reload or a seek)
      if (started && !video.seeking && video.currentSrc && (mseActive || video.currentSrc === video.src) && o) {
        hitches++;
        const me = o; const at = performance.now(); const ct = video.currentTime; stallAt = at; clearTimeout(stallTimer2);
        // frozen 12 s: first reconnect the same version; a second freeze within 3 min moves to another copy/feed
        stallTimer2 = setTimeout(() => {
          if (o !== me || stallAt !== at || !started || video.paused || video.currentTime - ct >= 2) return;
          report('stall'); stallAt = 0; const now = performance.now(); const again = now - lastFreeze < 180000; lastFreeze = now;
          if (!again || !switchSource('freeze')) resumeHere(o.live ? null : 'Playback froze, reconnecting…');
        }, 12000);
      }
    });
    video.addEventListener('timeupdate', () => { const t = video.currentTime; if (t > watchedAt && t - watchedAt < 2) watchedSecs += t - watchedAt; watchedAt = t; });
    video.addEventListener('timeupdate', tick);
    // mid-play error: a network drop or a cut stream resumes the same version; only decode/format errors
    // (code 3/4) or failures before the first frame move down the fallback chain
    video.addEventListener('error', () => {
      if (!video.error || !video.src || !o) return;
      const code = video.error.code;
      if (mseActive && !started) { mseFailed = true; Mse.destroy(); offset = mseTarget; return loadDirect(); }
      if (started && code !== 3 && code !== 4 && resumeHere()) return;
      fail(started ? 'error' : 'start');
    });
    video.addEventListener('ended', () => {
      if (!o) return;
      if (o.live) return load();
      // the stream stopped well before the end (connection dropped): that's not the end of the film
      const realEnd = credits && credits > duration * 0.55 ? credits - 30 : duration - 90;
      if (duration && pos() < realEnd && resumeHere()) return;
      save(); if (o.next && localGet('autoplay', '1') === '1' && AUTOPLAY.chain < 10) { AUTOPLAY.chain++; o.next.go(); } else close();
    });
    video.addEventListener('pause', () => { $('#pPlay').innerHTML = I.play; pausedSaver(); wake(); });
    video.addEventListener('play', () => { $('#pPlay').innerHTML = I.pause; clearTimeout(saverTimer); wake(); });
    video.addEventListener('volumechange', syncVolUi);
    $('#pBack').onclick = () => close();
    $('#pPlay').onclick = () => togglePlay(false);
    video.onclick = () => {
      if (closePopovers()) return;
      clearTimeout(clickTimer);
      clickTimer = setTimeout(() => { clickTimer = null; togglePlay(true); }, 210);
    };
    video.ondblclick = (e) => {
      e.preventDefault();
      clearTimeout(clickTimer); clickTimer = null;
      fullscreen();
    };
    $('#pFull').onclick = fullscreen;
    $('#pMute').onclick = () => toggleMute(true);
    $('#pVol').oninput = (e) => setVolume(+e.target.value, { hud: false, unmute: true });
    $('#pVol').onchange = (e) => { e.target.blur(); focusPlayer(); };
    $('#pVol').addEventListener('pointerup', (e) => { e.target.blur(); focusPlayer(); });
    if ($('#pTime')) $('#pTime').onclick = () => {
      if (o.live) return;
      showRem = !showRem;
      localSet('timeRem', showRem ? '1' : '0');
      renderTimeText(pendingSeekTarget ?? pos());
    };
    if ($('#pRew')) { $('#pRew').onclick = () => seekBy(-10); $('#pFwd').onclick = () => seekBy(10); }
    if ($('#pReplay')) $('#pReplay').onclick = () => replayWithSubs();
    if ($('#pLiveEdge')) $('#pLiveEdge').onclick = () => { if (liveBehind > 2) seekLiveToBack(0); else showOsd('Already at live edge'); };
    if ($('#pLast')) $('#pLast').onclick = () => toggleLastChannel();
    if ($('#pGuide')) $('#pGuide').onclick = () => toggleMiniGuide();
    if ($('#pRec')) $('#pRec').onclick = (e) => { e.stopPropagation(); toggleDvr(); };
    if ($('#pHelp')) $('#pHelp').onclick = (e) => { e.stopPropagation(); toggleShortcutsHelp(); };
    if ($('#pTune')) $('#pTune').onclick = (e) => { e.stopPropagation(); toggleSettingsMenu(); };
    if ($('#pPip')) $('#pPip').onclick = () => togglePip();
    if ($('#pDeint')) $('#pDeint').onclick = () => {
      const next = localGet('di.' + o.id, '0') !== '1';
      localSet('di.' + o.id, next ? '1' : '0');
      $('#pDeint').classList.toggle('on', next);
      showOsd(next ? 'Deinterlace (50fps): On' : 'Deinterlace: Off');
      loadDirect('Switching mode…');
    };
    if ($('#pNight')) $('#pNight').onclick = () => {
      const next = localGet('night', '0') !== '1';
      localSet('night', next ? '1' : '0');
      $('#pNight').classList.toggle('on', next);
      showOsd(next ? 'Clear Voice: On' : 'Clear Voice: Off');
      const at = pos(); offset = at; load({ target: at, splice: mseActive && started && !o.live, why: 'Updating audio…' });
    };
    if ($('#pNextEp')) $('#pNextEp').onclick = () => o.next.go();
    $('#pCast').onclick = (e) => { e.stopPropagation(); castMenu(); };
    $('#pInfo').onclick = () => toggleStats();
    $('#pPro').onclick = () => toPro();
    if ($('#pPrev')) { $('#pPrev').onclick = () => zap(-1); $('#pNext').onclick = () => zap(1); }
    // Keep keyboard focus off mouse-clicked controls so Arrow/Space keys always control playback
    pc.addEventListener('click', (e) => {
      const btn = e.target.closest('button');
      if (btn && e.detail > 0) { btn.blur(); focusPlayer(); }
    });
    const sk = $('#pSeek');
    if (sk) {
      const at = (e) => { const r = $('.track', sk).getBoundingClientRect(); return r.width ? Math.max(0, Math.min(1, (e.clientX - r.left) / r.width)) : 0; };
      const paintHover = (f) => {
        const ht = $('.hover-t', sk); if (!ht) return;
        ht.style.left = Math.max(4, Math.min(96, f * 100)) + '%';
        if (o.live) {
          const b = Math.round((1 - f) * liveBufSecs);
          ht.textContent = liveBufSecs > 5 ? (b > 2 ? `−${fmtTime(b)}` : 'Live') : 'Live';
        } else if (duration) {
          const sec = f * duration;
          const seg = segmentLabelAt(sec);
          ht.textContent = seg ? `${fmtTime(sec)} · ${seg}` : fmtTime(sec);
        } else ht.textContent = '';
      };
      const paintDrag = (e) => {
        const f = at(e);
        paintHover(f);
        if (o.live) {
          const fl = $('.fill', sk); const kb = $('.knob', sk);
          if (fl) fl.style.width = f * 100 + '%';
          if (kb) kb.style.left = f * 100 + '%';
        } else if (duration) {
          paintSeekPreview(f * duration);
        }
      };
      let dragMove = null; let dragUp = null; let dragCancel = null;
      const cleanupDrag = () => {
        if (dragMove) { window.removeEventListener('pointermove', dragMove); dragMove = null; }
        if (dragUp) { window.removeEventListener('pointerup', dragUp); dragUp = null; }
        if (dragCancel) { window.removeEventListener('pointercancel', dragCancel); dragCancel = null; }
      };
      const endDrag = (e, commit) => {
        if (!draggingSeek) return;
        draggingSeek = false;
        cleanupDrag();
        sk.classList.remove('dragging');
        try { sk.releasePointerCapture(e.pointerId); } catch {}
        if (commit) {
          const f = at(e);
          if (o.live) seekTo(f * liveBufSecs, { byUser: true });
          else if (duration) seekTo(f * duration, { byUser: true });
        }
        sk.blur(); focusPlayer(); wake();
      };
      sk.addEventListener('pointerdown', (e) => {
        if (e.button !== 0) return;
        e.preventDefault();
        draggingSeek = true;
        sk.classList.add('dragging');
        try { sk.setPointerCapture(e.pointerId); } catch {}
        cleanupDrag();
        dragMove = (ev) => paintDrag(ev);
        dragUp = (ev) => endDrag(ev, true);
        dragCancel = (ev) => endDrag(ev, false);
        window.addEventListener('pointermove', dragMove);
        window.addEventListener('pointerup', dragUp);
        window.addEventListener('pointercancel', dragCancel);
        paintDrag(e);
        wake();
      });
      sk.addEventListener('pointermove', (e) => {
        if (!draggingSeek) paintHover(at(e));
      });
    }
    const wake = () => {
      el.classList.remove('idle');
      clearTimeout(idleTimer);
      idleTimer = setTimeout(() => {
        if (video && !video.paused && !hoveringCtrl && !draggingSeek && !hasOpenPopover()) el.classList.add('idle');
      }, oled() ? 2500 : 3000);
      if (video?.paused) pausedSaver();
    };
    wakeControls = wake;
    el.onmousemove = wake; wake();
    $$('.pc-top, .pc-bottom', pc).forEach((bar) => {
      bar.addEventListener('mouseenter', () => { hoveringCtrl = true; wake(); });
      bar.addEventListener('mouseleave', () => { hoveringCtrl = false; wake(); });
    });
  }
  // ---- casting: the TV plays the stream; this player becomes its remote control
  let casting = null; let castPoll;
  async function castMenu() {
    const old = $('.cast-menu', el); if (old) return old.remove();
    const m = document.createElement('div'); m.className = 'cast-menu'; m.innerHTML = '<b>Play on</b><div class="muted">Looking for TVs…</div>'; el.appendChild(m);
    const chrome = video?.remote && typeof video.remote.prompt === 'function';
    const r = await api('/api/cast/devices').catch(() => ({ devices: [] }));
    if (!m.isConnected) return;
    m.innerHTML = '<b>Play on</b>' + r.devices.map((d) => `<button data-cast="${esc(d.id)}">${I.cast}<span>${esc(d.name)}<small>${esc(d.model || 'Smart TV')}</small></span></button>`).join('')
      + (chrome ? `<button data-chromecast="1">${I.cast}<span>Chromecast / Google TV…<small>uses the browser's cast</small></span></button>` : '')
      + (!r.devices.length ? `<div class="muted">No TVs found. Turn the TV on and make sure it's on the same Wi-Fi${chrome ? '' : ''}.</div>` : '')
      + '<div class="muted">The first time, Windows may ask to allow tv-server on private networks: choose Allow.</div>';
    $$('[data-cast]', m).forEach((b) => (b.onclick = () => { m.remove(); startCast(b.dataset.cast, b.querySelector('span').firstChild.textContent); }));
    $('[data-chromecast]', m)?.addEventListener('click', () => { m.remove(); video.remote.prompt().catch((e) => toast(e?.message || 'Casting was cancelled')); });
    setTimeout(() => document.addEventListener('click', function off(e) { if (!m.contains(e.target) && e.target.closest('#pCast') === null) { m.remove(); document.removeEventListener('click', off); } }), 0);
  }
  async function startCast(device, name) {
    const a = cur(); const params = { a: audio };
    const ext = a.id === o.id ? o.ext : a.ext; if (ext) params.ext = ext;
    if (o.kind === 'series' && o.sid) params.sid = o.sid;
    if (o.catchup) { params.cs = o.catchup.cs; params.dur = o.catchup.dur; }
    const t = o.live ? 0 : pos();
    toast(`Starting on ${name}…`);
    try { await post('/api/cast/start', { device, kind: o.kind, id: String(a.id), t, title: [o.title, o.sub].filter(Boolean).join(' · '), params }); }
    catch (e) { return toast(e.message); }
    casting = { device, name, state: 'PLAYING', vol: Math.max(0, Math.min(1, +localGet('vol', 0.5))), muted: false }; offset = t;
    syncVolUi();
    if (video) { video.pause(); video.removeAttribute('src'); video.load(); }
    clearTimeout(stallTimer); clearTimeout(stallTimer2); spin(false);
    showMsg(`<h4>${I.cast} Playing on ${esc(name)}</h4><p>Use the controls here or the phone remote.</p><div class="btns" style="justify-content:center"><button class="btn btn-primary btn-sm" id="castBack">Play here instead</button><button class="btn btn-ghost btn-sm" id="castStop">Stop casting</button></div>`);
    $('#castBack').onclick = () => stopCast(true); $('#castStop').onclick = () => stopCast(false);
    clearInterval(castPoll); castPoll = setInterval(async () => {
      const st = await api('/api/cast/status').catch(() => null); if (!casting) return;
      if (!st?.casting) { casting = null; clearInterval(castPoll); showMsg(null); return; }
      casting.state = st.state; if (!o.live) offset = st.pos; $('#pPlay').innerHTML = st.state === 'PLAYING' ? I.pause : I.play; tick();
    }, 2000);
  }
  async function stopCast(resumeHere) {
    if (!casting) return; const at = offset; casting = null; clearInterval(castPoll);
    await post('/api/cast/control', { action: 'stop' }).catch(() => {});
    showMsg(null);
    if (resumeHere) { offset = at; load(); } else close();
  }
  const castCtl = (action, extra = {}) => post('/api/cast/control', { action, ...extra }).catch((e) => toast(e.message));
  function togglePlay(showHud = false) {
    if (pro) { post('/api/mpv/control', { cmd: 'toggle' }).catch(() => {}); return; }
    if (casting) { const playNow = casting.state !== 'PLAYING'; casting.state = playNow ? 'PLAYING' : 'PAUSED_PLAYBACK'; $('#pPlay').innerHTML = playNow ? I.pause : I.play; if (showHud) showOsd('', playNow ? I.play : I.pause); return castCtl(playNow ? 'play' : 'pause'); }
    if (!video) return;
    if (video.paused) {
      if (showHud) showOsd('', I.play);
      if (o.live) {
        const pausedSecs = livePausedAt ? (Date.now() - livePausedAt) / 1000 : 0;
        livePausedAt = 0;
        if (pausedSecs > 2.5) {
          const targetBack = Math.max(0, liveBehind + pausedSecs);
          liveBack = liveBufSecs > 5 ? Math.min(liveBufSecs - 1, targetBack) : targetBack;
          liveBehind = liveBack;
          updateLiveEdgeUi();
          loadDirect();
        } else {
          video.play().catch(() => {});
        }
      } else {
        video.play().catch(() => {});
      }
    } else {
      if (showHud) showOsd('', I.pause);
      if (o.live) { livePausedAt = Date.now(); showOsd('Paused · live buffering'); }
      video.pause();
    }
  }
  function fullscreen() { if (document.fullscreenElement) document.exitFullscreen(); else el.requestFullscreen?.(); }
  function zap(dir) {
    const list = o.channels; if (!list?.length) return;
    const i = list.findIndex((c) => c.id === o.id);
    const nx = list[(i + dir + list.length) % list.length];
    rememberLiveChannel(nx);
    open({ ...o, id: nx.id, title: nx.title, epg: nx.epg, logo: nx.logo || '', sub: '', alternates: [], label: 'Main' });
  }
  function toggleLastChannel() {
    if (!o?.live || !LAST_LIVE || LAST_LIVE.id === o.id) return toast('No previous channel yet');
    const target = { ...LAST_LIVE };
    playLive(target);
  }
  let drawerTab = 'scores';
  async function toggleMiniGuide(force) {
    if (!o?.live) return;
    const dr = $('#pDrawer', el); if (!dr) return;
    const show = force !== undefined ? !!force : dr.hidden;
    if (!show) { dr.hidden = true; $('#pGuide', el)?.classList.remove('on'); return; }
    dr.hidden = false; $('#pGuide', el)?.classList.add('on');
    dr.innerHTML = `<div class="ld-head">
      <div class="ld-tabs"><button class="ld-tab ${drawerTab === 'scores' ? 'on' : ''}" data-ldt="scores">Live Scores</button><button class="ld-tab ${drawerTab === 'channels' ? 'on' : ''}" data-ldt="channels">Channels</button></div>
      <button class="pbtn" id="ldClose" title="Close (G / Esc)">${I.close}</button></div>
      <div class="ld-body"><div class="muted" style="padding:14px">Loading…</div></div>`;
    $('#ldClose', dr).onclick = () => toggleMiniGuide(false);
    const renderBody = async () => {
      const body = $('.ld-body', dr); if (!body || dr.hidden) return;
      $$('[data-ldt]', dr).forEach((b) => b.classList.toggle('on', b.dataset.ldt === drawerTab));
      if (drawerTab === 'scores') {
        const sp = await api('/api/sports').catch(() => null);
        if (!body.isConnected || dr.hidden) return;
        const hideScores = localGet('spoilers', '0') === '1';
        const liveG = (sp?.fixtures || [])
          .filter((f) => f.state === 'in' || f.state === 'pre')
          .sort((a, b) => (a.state === 'in' ? 0 : 1) - (b.state === 'in' ? 0 : 1) || (b.fav ? 1 : 0) - (a.fav ? 1 : 0) || (b.feeds?.length ? 1 : 0) - (a.feeds?.length ? 1 : 0) || (a.start || 0) - (b.start || 0))
          .slice(0, 24);
        const liveEv = (sp?.events || []).filter((e) => e.status === 'live').slice(0, 18);
        const gameHtml = liveG.map((f) => {
          const ch = f.feeds?.[0];
          const chName = ch?.channel || ch?.label || '';
          const title = f.solo ? (f.title || '') : `${f.away?.short || f.away?.name || ''} vs ${f.home?.short || f.home?.name || ''}`;
          const k = ch ? remember({ k: 'l', id: ch.id, title: f.title || title || chName, sub: `${f.leagueName || f.sport || ''} · ${chName}`.replace(/^ · /, ''), logo: ch.logo || '', epg: ch.epg || '', feeds: f.feeds }) : '';
          const sc = hideScores
            ? '<span class="muted">Scores hidden</span>'
            : (f.state === 'in' ? (f.solo ? '<b>LIVE</b>' : `<b>${esc(f.away?.score ?? '')} – ${esc(f.home?.score ?? '')}</b>`) : esc(fmtClock(f.start)));
          return `<div class="ld-item ${ch ? '' : 'dim'}" ${ch ? `data-ld-play="${k}" tabindex="0" role="button"` : ''}>
            <div class="ld-top"><span>${f.fav ? '★ ' : ''}${esc(f.leagueName || f.sport || '')}</span><span class="${f.state === 'in' ? 'ok' : 'muted'}">${esc(hideScores && f.state === 'in' ? 'LIVE' : (f.detail || (f.state === 'in' ? 'LIVE' : 'Soon')))}</span></div>
            <div class="ld-title">${esc(title)}</div>
            <div class="ld-sub">${sc}${chName ? ` · ${esc(chName)}` : ' · No feed yet'}</div>
          </div>`;
        }).join('');
        const evHtml = liveEv.map((e) => {
          const feeds = e.alts?.length ? [{ id: e.id, channel: e.channel, label: e.channel }, ...e.alts.map((a) => ({ id: a.id, channel: a.channel, label: a.channel }))] : null;
          const k = remember({ k: 'l', id: e.id, title: e.title, sub: e.channel, logo: e.logo || '', feeds });
          return `<div class="ld-item" data-ld-play="${k}" tabindex="0" role="button">
            <div class="ld-top"><span>${esc(e.league || 'Live Sport')}</span><span class="ok">LIVE</span></div>
            <div class="ld-title">${esc(e.title)}</div>
            <div class="ld-sub">${esc(e.channel || '')}</div>
          </div>`;
        }).join('');
        body.innerHTML = (gameHtml || evHtml)
          ? (gameHtml ? `<div class="ld-sec">Live & Upcoming Games</div>${gameHtml}` : '') + (evHtml ? `<div class="ld-sec">Live Sport Channels</div>${evHtml}` : '')
          : '<div class="muted" style="padding:14px">No live sport fixtures right now.</div>';
      } else {
        const [rec, favs] = await Promise.all([api('/api/live/recent').catch(() => []), api('/api/live/channels?cat=favs').catch(() => [])]);
        if (!body.isConnected || dr.hidden) return;
        const chRow = (ch) => {
          const k = remember({ k: 'l', id: ch.id, title: ch.title, logo: ch.logo || '', epg: ch.epg || '' });
          const curCh = ch.id === o?.id;
          return `<div class="ld-item ${curCh ? 'cur' : ''}" data-ld-play="${k}" tabindex="0" role="button">
            <div class="ld-top"><span>${esc(ch.title)}${ch.qtag ? ` <em class="qtag ${esc(ch.qtag.toLowerCase())}">${esc(ch.qtag)}</em>` : ''}</span>${curCh ? '<span class="ok">Watching</span>' : ''}</div>
            ${ch.now ? `<div class="ld-title">${esc(ch.now.title)}</div><div class="ld-sub">until ${fmtClock(ch.now.e)}${ch.next ? ` · Next: ${esc(ch.next.title)}` : ''}</div>` : ''}
          </div>`;
        };
        const grp = (o.channels || []).slice(0, 30);
        body.innerHTML =
          (rec.length ? `<div class="ld-sec">Recent Channels</div>${rec.map(chRow).join('')}` : '') +
          (favs.length ? `<div class="ld-sec">Favourites</div>${favs.map(chRow).join('')}` : '') +
          (grp.length ? `<div class="ld-sec">Current Group</div>${grp.map(chRow).join('')}` : '') ||
          '<div class="muted" style="padding:14px">No channels loaded yet.</div>';
      }
      $$('[data-ld-play]', body).forEach((elItem) => {
        elItem.onclick = () => {
          const target = ITEMS.get(elItem.dataset.ldPlay);
          if (!target) return;
          toggleMiniGuide(false);
          playLive(target);
        };
      });
    };
    $$('[data-ldt]', dr).forEach((b) => (b.onclick = () => { drawerTab = b.dataset.ldt; renderBody(); }));
    renderBody();
  }

  async function close(switching = false) {
    if (!o) return;
    Mse.destroy();
    clearAiSubJob(); clearDvrTimer(); dvrRec = null; clearTimeout(castVolTimer);
    clearInterval(livePollTimer); clearTimeout(liveEpgTimer); clearTimeout(liveBannerTimer);
    if (casting) { casting = null; clearInterval(castPoll); post('/api/cast/control', { action: 'stop' }).catch(() => {}); }
    if (pro) { pro = null; clearInterval(proPoll); post('/api/mpv/stop', {}).catch(() => {}); }
    clearInterval(statsTimer); clearInterval(shiftTimer); clearTimeout(saverTimer); clearTimeout(replaySubTimer); replayPrevSub = null; lastBytes = null; el.style.removeProperty('--shift');
    clearTimeout(pendingSeekTimer); pendingSeekTimer = null; pendingSeekTarget = null; pendingLiveBack = null;
    clearTimeout(hudSeekTimer); hudSeekDir = 0; hudSeekAccum = 0;
    clearTimeout(osdTimer); clearTimeout(volOsdTimer); clearTimeout(clickTimer); clearTimeout(wheelResetTimer); clearUndoSkip();
    draggingSeek = false; hoveringCtrl = false;
    if (document.pictureInPictureElement && document.pictureInPictureElement === video) { document.exitPictureInPicture().catch(() => {}); }
    save(); flushWatched(); clearInterval(saveTimer); clearInterval(subTimer); clearInterval(syncTimer); clearTimeout(stallTimer); clearTimeout(stallTimer2); clearTimeout(idleTimer);
    stallAt = 0; started = false; clearTimeout(stallTimer2);
    if (video) { video.removeAttribute('src'); video.load(); }
    o = null; clearTimeout(stallTimer2); stallAt = 0;
    await post('/api/stop', { pg: PAGE_ID }).catch(() => {});
    if (!switching) {
      invalidateCache('/api/home', '/api/anime', '/api/docs', '/api/mylist');
      post('/api/display/restore', {}).catch(() => {});
      if (PLAYER_STATE.open) { PLAYER_STATE.open = false; if (history.state?.player) { PLAYER_STATE.ignorePop = true; history.back(); } }
      if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
      el.hidden = true; el.innerHTML = ''; el.classList.remove('zoom-fill', 'zoom-110', 'sub-sm', 'sub-lg'); document.body.style.overflow = modal.hidden ? '' : 'hidden'; document.body.classList.remove('playing');
      if (typeof pushRemoteState === 'function') pushRemoteState(true);
      if (/^#\/(home|mylist)?$/.test(location.hash) || !location.hash) route();
    }
  }

  document.addEventListener('fullscreenchange', () => {
    if (el.hidden || !o) return;
    const fb = $('#pFull', el);
    if (fb) {
      const fs = !!document.fullscreenElement;
      fb.innerHTML = fs ? I.unfull : I.full;
      fb.title = fs ? 'Exit fullscreen (F)' : 'Fullscreen (F)';
    }
    focusPlayer();
  });

  // Global wheel volume control: works anywhere on the player except inside scrollable menus/drawers
  window.addEventListener('wheel', (e) => {
    if (el.hidden || !o || document.querySelector('.saver')) return;
    if (e.ctrlKey || e.metaKey) return;
    if (e.target && typeof e.target.closest === 'function' && e.target.closest('.live-drawer, .extsub-menu, .cast-menu, .p-menu, .p-help, .stats, .p-msg')) return;
    if (!video || !e.deltaY) return;
    e.preventDefault();
    wheelAcc += e.deltaY;
    clearTimeout(wheelResetTimer);
    wheelResetTimer = setTimeout(() => { wheelAcc = 0; }, 160);
    const threshold = e.deltaMode === 1 ? 1 : 40;
    if (Math.abs(wheelAcc) >= threshold) {
      const dir = wheelAcc < 0 ? 1 : -1;
      wheelAcc = 0;
      stepVolume(dir * 0.05);
    }
  }, { passive: false, capture: true });

  // Global capture-phase keyboard handler: works no matter what element had focus
  window.addEventListener('keydown', (e) => {
    if (el.hidden || !o || document.querySelector('.saver')) return;
    if (e.ctrlKey || e.metaKey) return;
    if (/^F\d+$/.test(e.key)) return;
    const k = e.key.toLowerCase();
    if (e.altKey && (k === 'arrowleft' || k === 'arrowright' || k === 'f4' || k === 'tab')) return;

    const tgt = e.target;
    const inPlayer = !!(tgt && typeof tgt.closest === 'function' && tgt.closest('#player'));
    // Never hijack typing in an actual text input INSIDE the player; blur stray inputs outside the player
    if (tgt && ((tgt.tagName === 'INPUT' && tgt.type !== 'range') || tgt.tagName === 'TEXTAREA' || tgt.isContentEditable)) {
      if (inPlayer) return;
      try { tgt.blur(); } catch {}
    }

    const dir = { arrowright: 'right', arrowleft: 'left', arrowdown: 'down', arrowup: 'up' }[k];
    const inCtrl = inPlayer && tgt !== el && tgt !== video && !!tgt.closest('#player .pc, #player .p-msg, #player .next-up, #player .skip-btn, #player .live-drawer');
    const isTvNav = !e.isTrusted || document.body.classList.contains('tv');

    // Dropdown <select>: Esc blurs; Up/Down/Enter navigate the dropdown; playback keys blur & execute
    if (tgt?.tagName === 'SELECT' && inPlayer) {
      if (k === 'escape' || k === 'backspace') { e.preventDefault(); e.stopPropagation(); tgt.blur(); focusPlayer(); return; }
      if (k === 'arrowup' || k === 'arrowdown' || k === 'enter') return;
      tgt.blur(); focusPlayer();
    }

    // TV / Gamepad / Phone-Remote spatial navigation
    if (isTvNav && dir && inCtrl) {
      el.dispatchEvent(new Event('mousemove'));
      if (tgt.id === 'pSeek' && (dir === 'left' || dir === 'right')) {
        e.preventDefault(); e.stopPropagation(); seekBy(dir === 'left' ? -10 : 10); return;
      }
      if (tgt.type === 'range' && (dir === 'left' || dir === 'right')) return;
      e.preventDefault(); e.stopPropagation();
      navMove(dir);
      return;
    }
    if (k === 'enter' && inCtrl) {
      if (!e.isTrusted) { e.preventDefault(); e.stopPropagation(); tgt.click(); }
      return;
    }
    // Live mini-guide drawer keyboard navigation
    if (inPlayer && tgt.closest('.live-drawer') && (k === 'arrowup' || k === 'arrowdown')) {
      e.preventDefault(); e.stopPropagation(); navMove(k === 'arrowup' ? 'up' : 'down'); return;
    }

    const stop = () => { e.preventDefault(); e.stopPropagation(); if (e.isTrusted) AUTOPLAY.chain = 0; Saver.touch?.(); };

    // Escape / Backspace layered close
    if (k === 'escape') {
      if (closePopovers()) { stop(); return; }
      if (!document.fullscreenElement) { stop(); close(); }
      return;
    }
    if (k === 'backspace') {
      stop();
      if (closePopovers()) return;
      if (!document.fullscreenElement) close();
      return;
    }

    // Help overlay toggle
    if (e.key === '?' || e.key === '/') { stop(); toggleShortcutsHelp(); return; }

    // Playback & Seeking
    if (k === ' ' || k === 'k') { stop(); togglePlay(true); return; }
    if (k === 'arrowleft') { stop(); seekBy(e.shiftKey ? -30 : -10); return; }
    if (k === 'arrowright') { stop(); seekBy(e.shiftKey ? 30 : 10); return; }
    if (k === 'j') { stop(); seekBy(e.shiftKey ? -30 : -10); return; }
    if (k === 'l') {
      stop();
      if (o.live) toggleLastChannel();
      else seekBy(e.shiftKey ? 30 : 10);
      return;
    }

    // Volume & Live channel zap
    if (k === 'arrowup') {
      stop();
      if (o.live && (e.shiftKey || isTvNav) && o.channels?.length > 1) zap(-1);
      else stepVolume(0.05);
      return;
    }
    if (k === 'arrowdown') {
      stop();
      if (o.live && (e.shiftKey || isTvNav) && o.channels?.length > 1) zap(1);
      else stepVolume(-0.05);
      return;
    }
    if (k === 'pageup' && o.live && o.channels?.length > 1) { stop(); zap(-1); return; }
    if (k === 'pagedown' && o.live && o.channels?.length > 1) { stop(); zap(1); return; }
    if (e.key === '=' || e.key === '+') { stop(); stepVolume(0.05); return; }
    if (e.key === '-' || e.key === '_') { stop(); stepVolume(-0.05); return; }
    if (k === 'm') { stop(); toggleMute(true); return; }

    // Direct jump 0-9 (0%..90%), Home, End
    if (!e.shiftKey && !e.altKey && /^[0-9]$/.test(e.key) && !o.live && duration > 0) {
      stop();
      const target = (+e.key / 10) * duration;
      seekTo(target, { byUser: true });
      showOsd(o.spoilerFree ? `${+e.key * 10}%` : `${fmtTime(target)} (${+e.key * 10}%)`);
      return;
    }
    if (k === 'home' && !o.live && duration > 0) { stop(); seekTo(0, { byUser: true }); showOsd('0:00'); return; }
    if (k === 'end' && o.live) { stop(); seekLiveToBack(0); return; }

    // Speed (< / >) & Frame Step (, / .)
    if (e.key === '<' || (e.shiftKey && e.key === ',')) { stop(); stepSpeed(-1); return; }
    if (e.key === '>' || (e.shiftKey && e.key === '.')) { stop(); stepSpeed(1); return; }
    if (!e.shiftKey && (e.key === ',' || e.key === '.') && !o.live) { stop(); stepFrame(e.key === '.' ? 1 : -1); return; }

    // View, Audio, Subtitles & Navigation
    if (k === 'f') { stop(); fullscreen(); return; }
    if (k === 'z') { stop(); cycleZoom(); return; }
    if (k === 'c') { stop(); cycleSubs(); return; }
    if (k === 'a') { stop(); switchAudio(); return; }
    if (k === 'v') { stop(); $('#pNight', el)?.click(); return; }
    if (k === 'r' && !o.live) { stop(); replayWithSubs(); return; }
    if (k === 's') {
      if (pro) { stop(); post('/api/mpv/control', { cmd: 'skip' }).catch(() => {}); return; }
      const b = $('#pSkip', el) || $('#nuPlay', el);
      if (b) { stop(); b.click(); }
      return;
    }
    if (k === 'u' && lastAutoSkip) { stop(); undoLastSkip(); return; }
    if (k === 'n' && o.next) { stop(); o.next.go(); return; }
    if (k === 'b' && o.prev) { stop(); o.prev.go(); return; }
    if (k === 'g') {
      if (o.live) { stop(); toggleMiniGuide(); }
      else if (subSel >= 0 || subSel === -3) { stop(); nudgeSubs(-250); }
      return;
    }
    if (k === 'h' && !o.live && (subSel >= 0 || subSel === -3)) { stop(); nudgeSubs(250); return; }
    if (e.key === '[') { stop(); nudgeAudio(-100); return; }
    if (e.key === ']') { stop(); nudgeAudio(100); return; }
    if (k === 'i') { stop(); toggleStats(); return; }
    if (k === 'p') {
      if (e.shiftKey) { stop(); togglePip(); }
      else if ($('#pPro', el) && !$('#pPro', el).hidden) { stop(); toPro(); }
      return;
    }
  }, true);
  // closing the window: save where we are (beacons survive unload; fetch doesn't), stop only our own stream
  window.addEventListener('beforeunload', () => {
    if (!o) return;
    save(true); flushWatched(true);
    const b = (x) => new Blob([JSON.stringify(x)], { type: 'application/json' });
    navigator.sendBeacon?.('/api/stop', b({ pg: PAGE_ID }));
  });

  function setMediaSession() {
    const ms = navigator.mediaSession; if (!ms || !o) return;
    try {
      ms.metadata = new MediaMetadata({ title: o.title, artist: o.sub || '', artwork: o.item?.poster ? [{ src: o.item.poster }] : [] });
      ms.setActionHandler('play', () => video?.paused && togglePlay(false));
      ms.setActionHandler('pause', () => !video?.paused && togglePlay(false));
      ms.setActionHandler('seekbackward', () => seekBy(-10));
      ms.setActionHandler('seekforward', () => seekBy(10));
      ms.setActionHandler('seekto', (d) => { if (!o?.live && Number.isFinite(d?.seekTime)) seekTo(d.seekTime); });
      ms.setActionHandler('previoustrack', o.prev ? () => o.prev.go() : null);
      ms.setActionHandler('nexttrack', o.next ? () => o.next.go() : null);
      ms.setActionHandler('stop', () => close());
    } catch {}
  }
  function cycleSubs() {
    if (!o || o.live) return;
    const opts = [-1];
    if (o.extSub) opts.push(-3);
    for (const x of probe?.subs || []) opts.push(x.i);
    if (opts.length < 2) return showOsd('No subtitles · use Subtitles menu');
    const nx = opts[(opts.indexOf(subSel) + 1) % opts.length];
    if ($('#pSubs', el)) $('#pSubs', el).value = String(nx);
    setSubs(nx);
    const label = nx === -1 ? 'Off' : nx === -3 ? 'English (Downloaded)' : (langName(probe?.subs?.find((x) => x.i === nx)?.lang) || 'On');
    showOsd(`Subtitles: ${label}`);
  }
  function switchAudio(v) {
    const tracks = probe?.audio || [];
    if (tracks.length < 2) return showOsd('Single audio track');
    const idx = (v !== undefined && v !== null && !Number.isNaN(+v)) ? Math.max(0, Math.min(tracks.length - 1, +v)) : (audio + 1) % tracks.length;
    audio = idx;
    const as = $('#pAudio', el); if (as) as.value = String(idx);
    const a = tracks[idx];
    showOsd(`Audio: ${langName(a.lang) || `Track ${idx + 1}`}${a.ch > 2 ? ' · 5.1' : ''}`);
    const at = pos(); offset = at; load({ target: at, splice: mseActive && started, why: 'Switching audio…' });
  }
  // commands from the phone remote
  function cmd(c, v) {
    if (!o || (!video && !casting && !pro)) return;
    el.dispatchEvent(new Event('mousemove'));
    if (c === 'toggle') togglePlay(true);
    else if (c === 'seek') seekBy(+v || 0);
    else if (c === 'seekto' && !o.live) seekTo(+v || 0, { byUser: true });
    else if (c === 'next' && o.next) o.next.go();
    else if (c === 'last' && o.live) toggleLastChannel();
    else if (c === 'zap' && o.live) zap(+v || 1);
    else if (c === 'subs') cycleSubs();
    else if (c === 'audio') switchAudio(v);
    else if (c === 'vol') setVolume(+v, { hud: true, unmute: true, immediate: true });
    else if (c === 'mute') toggleMute(true);
    else if (c === 'night') $('#pNight', el)?.click();
    else if (c === 'pro') toPro();
    else if (c === 'undo') undoLastSkip();
    else if (c === 'full') fullscreen();
    else if (c === 'close') close();
    else if (c === 'skip') { const b = $('#pSkip', el) || $('#nuPlay', el); b?.click(); }
    else if (c === 'info') toggleStats();
    else if (c === 'source') { const sel = $('#pSrc', el); if (sel && !sel.hidden) { sel.value = v; sel.onchange(); } }
  }
  function snapshot() {
    if (!o) return null;
    const audios = (probe?.audio || []).map((a, i) => ({ i, label: `${langName(a.lang) || `Track ${i + 1}`}${a.title ? ` · ${a.title}` : ''}${a.ch > 2 ? ' · 5.1' : ''}` }));
    return {
      title: o.title, sub: o.sub || '', live: !!o.live, pos: pos(), dur: duration,
      paused: casting ? casting.state !== 'PLAYING' : !!video?.paused,
      vol: casting ? (casting.vol ?? 1) : (video?.volume ?? 1),
      muted: casting ? !!casting.muted : !!video?.muted,
      casting: casting?.name || null, pro: !!pro, night: localGet('night', '0') === '1',
      next: o.next?.label || null,
      skip: $('#pSkip', el)?.textContent || ($('#nuPlay', el) ? 'Next episode' : null),
      undoSkip: !!lastAutoSkip,
      subs: subSel >= 0 || subSel === -3,
      hasSubs: !!(probe?.subs?.length || o.extSub),
      audios, audio,
      sources: [{ id: o.id, label: o.label || (o.live ? 'Main feed' : 'Original') }, ...(o.alternates || []).map((v) => ({ id: v.id, label: v.label || v.channel }))],
      src: cur().id, img: o.item?.backdrop || o.item?.poster || '',
    };
  }

  return { open, close, cmd, snapshot, cycleSubs, _dbg: () => ({ ai, mse: mseActive, liveBufSecs, liveBehind, attempts: attempts.map((a) => a.id + (a.tx ? 'tx' : '') + (a.alt ? 'alt' : '')), src: video?.src }) };
})();

// OLED care: a black screen with a dim drifting clock after a while with nothing moving.
const Saver = (() => {
  let elx = null; let t = null; let drift = null; let idleT = null;
  function show() {
    if (elx || localGet('oled', '1') !== '1') return;
    elx = document.createElement('div'); elx.className = 'saver'; elx.innerHTML = '<div class="saver-clock"></div>'; document.body.appendChild(elx);
    const c = $('.saver-clock', elx);
    const upd = () => { c.textContent = new Date().toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' }); };
    const move = () => { c.style.left = 10 + Math.random() * 70 + '%'; c.style.top = 10 + Math.random() * 70 + '%'; };
    upd(); move(); t = setInterval(upd, 10000); drift = setInterval(move, 60000);
  }
  function hide() { if (!elx) return false; elx.remove(); elx = null; clearInterval(t); clearInterval(drift); return true; }
  // menus idle for 5 minutes (not while a video plays)
  const arm = () => { clearTimeout(idleT); idleT = setTimeout(() => { const v = $('#player video'); if (!v || v.paused) show(); }, 300000); };
  ['mousemove', 'keydown', 'pointerdown', 'wheel', 'touchstart'].forEach((ev) => document.addEventListener(ev, (e) => {
    if (hide() && ev === 'keydown') { e.preventDefault(); e.stopImmediatePropagation(); }
    arm();
  }, true));
  arm();
  return { show, hide, touch: arm };
})();
window.__saver = Saver;
function localGet(k, d) { try { return localStorage.getItem('mq.' + k) ?? d; } catch { return d; } }
function localSet(k, v) { try { localStorage.setItem('mq.' + k, v); } catch {} }
const LANGS = { en: 'English', eng: 'English', fr: 'French', fre: 'French', fra: 'French', es: 'Spanish', spa: 'Spanish', de: 'German', ger: 'German', deu: 'German', it: 'Italian', ita: 'Italian', pt: 'Portuguese', por: 'Portuguese', ja: 'Japanese', jpn: 'Japanese', ko: 'Korean', kor: 'Korean', hi: 'Hindi', hin: 'Hindi', ar: 'Arabic', ara: 'Arabic', ru: 'Russian', rus: 'Russian', zh: 'Chinese', chi: 'Chinese', zho: 'Chinese', tr: 'Turkish', tur: 'Turkish', nl: 'Dutch', dut: 'Dutch', nld: 'Dutch', pl: 'Polish', pol: 'Polish', ta: 'Tamil', tam: 'Tamil' };
const langName = (l) => LANGS[(l || '').toLowerCase()] || (l && l !== 'und' ? l.toUpperCase() : '');

// ---------------------------------------------------------------- settings
function renderAccountHtml(acct) {
  if (!acct) return '<p class="muted">Checking subscription with provider…</p>';
  if (acct.error) return '<p>Couldn’t reach the provider for account details.</p>';
  return `<div class="stat"><span>Status</span><b class="${acct.status === 'Active' ? 'ok' : ''}">${esc(acct.status)}</b>
    <span>Ends</span><b>${new Date(acct.exp).toLocaleDateString([], { day: 'numeric', month: 'long', year: 'numeric' })} · ${acct.daysLeft} day${acct.daysLeft === 1 ? '' : 's'} left</b>
    <span>Streams in use</span><b>${acct.active} of ${acct.max}${acct.ours ? ' (this app)' : acct.active ? ' (another device)' : ''}</b></div>`;
}
async function openSettings() {
  const [s, st, prefs, skipStats] = await Promise.all([api('/api/settings'), api('/api/status'), api('/api/prefs'), api('/api/skips/stats').catch(() => null)]);
  const ago = st.lastSync ? Math.round((Date.now() - st.lastSync) / 3600e3) : null;
  const autoplayOn = localGet('autoplay', prefs.autoplay === false ? '0' : '1') === '1';
  const autoskipOn = localGet('autoskip', prefs.autoskip === false ? '0' : '1') === '1';
  const autoskipRecapOn = localGet('autoSkipRecap', prefs.autoSkipRecap === false ? '0' : '1') === '1';
  const autoskipCredOn = localGet('autoSkipCredits', prefs.autoSkipCredits === false ? '0' : '1') === '1';
  const skipCov = skipStats && (skipStats.matched || skipStats.audioMatched)
    ? ` Cached so far: <b>${(skipStats.matched || 0).toLocaleString()}</b> episodes across <b>${(skipStats.shows || 0).toLocaleString()}</b> shows${skipStats.audioMatched ? ` + <b>${skipStats.audioMatched.toLocaleString()}</b> audio-matched` : ''}.`
    : '';
  openModal(`<div class="sheet settings"><button class="close" aria-label="Close">${I.close}</button>
    <h2>Settings</h2><p>Everything runs on this PC. Your viewing history stays local.</p>
    <h4>Trending data (TMDB) ${s.tmdbKeySet ? '<span class="ok">· connected</span>' : ''}</h4>
    <p>Real "trending this week" rows come from The Movie Database. It's free:</p>
    <ol><li>Create an account at <a href="https://www.themoviedb.org/signup" target="_blank" rel="noopener">themoviedb.org/signup</a></li>
      <li>Open <a href="https://www.themoviedb.org/settings/api" target="_blank" rel="noopener">Settings → API</a> and request a Developer key (personal use)</li>
      <li>Paste the <b>API Key</b> or <b>API Read Access Token</b> below</li></ol>
    <div class="field"><input id="tmdbKey" type="password" placeholder="${s.tmdbKeySet ? 'Key saved. Paste a new one to replace it' : 'Paste your TMDB key'}" autocomplete="off">
      <button class="btn btn-accent btn-sm" id="saveKey" style="height:42px">Save</button></div>
    <h4>Subscription</h4>
    <div id="subAcctBox">${renderAccountHtml(ACCOUNT)}</div>
    <h4>Hide from all rows</h4><p>Never show these in recommendations, browse or curated rows (search still finds them).</p>
    <div class="hide-grid">
      <label class="tog"><input type="checkbox" id="optHideCams" ${prefs.hideCams !== false ? 'checked' : ''}> Hide CAM &amp; Telesync rips (still viewable in 4K › CAM Watch)</label>
      <label class="tog"><input type="checkbox" id="optUfcSpoiler" ${prefs.ufcSpoilerFree ? 'checked' : ''}> Spoiler-free UFC replays (hide runtime &amp; progress bars)</label>
      ${[['kids', 'Kids & family'], ['anime', 'Anime'], ['reality', 'Reality TV'], ['talk', 'Talk & news'], ['docs', 'Documentaries'], ['standup', 'Stand-up'], ['horror', 'Horror']].map(([k, l]) => `<label class="tog"><input type="checkbox" data-hide="${k}" ${prefs.hide.includes(k) ? 'checked' : ''}> ${l}</label>`).join('')}</div>
    <h4>IMDb & Rotten Tomatoes scores ${s.omdbKeySet ? '<span class="ok">· connected</span>' : ''}</h4>
    <p>Free key from <a href="https://www.omdbapi.com/apikey.aspx" target="_blank" rel="noopener">omdbapi.com/apikey.aspx</a> (choose FREE, then click the activation link they email you).</p>
    <div class="field"><input id="omdbKey" type="password" placeholder="${s.omdbKeySet ? 'Key saved. Paste a new one to replace it' : 'Paste your OMDb key'}" autocomplete="off"><button class="btn btn-accent btn-sm" id="saveOmdb" style="height:42px">Save</button></div>
    <h4>Library</h4>
    <div class="stat"><span>Movies</span><b>${st.counts?.movies?.toLocaleString() ?? '…'}</b><span>Series</span><b>${st.counts?.series?.toLocaleString() ?? '…'}</b>
      <span>Channels</span><b>${st.counts?.live?.toLocaleString() ?? '…'}</b><span>TV guide</span><b>${st.epgChannels} channels</b>
      <span>Last refresh</span><b>${ago === null ? '—' : ago < 1 ? 'under an hour ago' : `${ago}h ago`}${st.syncing ? ' · refreshing now' : ''}</b><span>Server</span><b>${esc(s.active)}</b></div>
    <div class="btns" style="margin-top:14px"><button class="btn btn-ghost btn-sm" id="syncNow">Refresh catalogue now</button></div>
    <h4>Playback</h4>
    <div class="hide-grid"><label class="tog"><input type="checkbox" id="optAutoplay" ${autoplayOn ? 'checked' : ''}> Play the next episode automatically</label>
      <label class="tog"><input type="checkbox" id="optAutoskip" ${autoskipOn ? 'checked' : ''}> Skip intros &amp; openings automatically</label>
      <label class="tog"><input type="checkbox" id="optAutoskipRecap" ${autoskipRecapOn ? 'checked' : ''}> Skip verified recaps automatically</label>
      <label class="tog"><input type="checkbox" id="optAutoskipCredits" ${autoskipCredOn ? 'checked' : ''}> Skip endings &amp; credits automatically (preserves anime post-credits scenes)</label>
      <label class="tog"><input type="checkbox" id="optSkipFillerSet" ${prefs.skipFiller ? 'checked' : ''}> Skip anime filler episodes automatically</label>
      <label class="tog"><input type="checkbox" id="optTv" ${document.body.classList.contains('tv') ? 'checked' : ''}> TV mode (big screen, arrows / gamepad)</label></div>
    <p class="muted">Intros, recaps and end credits combine MKV chapters, crowd-sourced TV &amp; anime skip databases (IntroDB, TheIntroDB, AniSkip — zero provider connections) and local audio fingerprinting.${skipCov} When a version stutters, playback moves to another copy of the same title, and the smoothest copy is picked first next time.</p>
    <h4>Picture & display</h4><div id="dispBox"><p>…</p></div>
    <h4>System & network health</h4><div id="sysHealthBox"><p>…</p></div>
    <h4>Phone remote</h4><div id="remoteBox"><p>…</p></div>
    <h4>Import & sync</h4>
    <p><b>Letterboxd</b>: on <a href="https://letterboxd.com/settings/data/" target="_blank" rel="noopener">letterboxd.com/settings/data</a> choose <i>Export your data</i>, then pick the .zip here. Watched films, ratings (3½★+ = like, 2★ or less = dislike) and your watchlist come across.</p>
    <div class="btns"><label class="btn btn-ghost btn-sm" style="cursor:pointer">Choose Letterboxd export<input type="file" id="lbFile" accept=".zip,.csv" multiple hidden></label><span class="muted" id="lbStatus"></span></div>
    <div id="traktBox"><p>…</p></div>
    <h4>Keyboard &amp; mouse</h4><p>In the player (works regardless of focus): Space / K play or pause · ← → or J / L skip 10s (Shift: 30s) · Scroll wheel or ↑ ↓ volume (±5%) · M mute · F or double-click fullscreen · Shift+P Picture-in-Picture · 0–9 jump 0%–90% · &lt; &gt; speed · , . frame step · Z picture fit/crop · C subtitles · A audio track · V Clear Voice · R replay 8s with subtitles · S skip intro · N / B next / prev episode · G / H subtitle delay · [ ] audio delay · I stats · P pro player (mpv) · ? shortcuts sheet · Live TV: G mini-guide, L last channel, PgUp/PgDn or Shift+↑/↓ zap, End live edge · Browse: Arrows navigate, / search, Esc back</p>
  </div>`);
  $('#saveKey').onclick = async () => {
    const v = $('#tmdbKey').value.trim(); if (!v) return;
    $('#saveKey').textContent = 'Checking…';
    try { await post('/api/settings', { tmdbKey: v }); toast('TMDB connected. Trending rows are live.'); closeModal(); route(); }
    catch (e) { toast(e.message); $('#saveKey').textContent = 'Save'; }
  };
  $('#optHideCams').onchange = (e) => post('/api/prefs', { hideCams: e.target.checked }).then(() => toast(e.target.checked ? 'CAM rips hidden from discovery rows' : 'CAM rips shown')).catch(() => {});
  $('#optUfcSpoiler').onchange = (e) => { localSet('ufcSpoilerFree', e.target.checked ? '1' : '0'); post('/api/prefs', { ufcSpoilerFree: e.target.checked }).catch(() => {}); toast(e.target.checked ? 'UFC Spoiler-Free mode enabled' : 'UFC Spoiler-Free mode off'); };
  $$('[data-hide]', modal).forEach((c) => (c.onchange = async () => {
    const hide = $$('[data-hide]', modal).filter((x) => x.checked).map((x) => x.dataset.hide);
    await post('/api/prefs', { hide }); toast('Saved. Rows will update');
  }));
  $('#saveOmdb').onclick = async () => {
    const v = $('#omdbKey').value.trim(); if (!v) return; $('#saveOmdb').textContent = 'Checking…';
    try { await post('/api/settings', { omdbKey: v }); toast('OMDb connected. Scores now show on detail pages'); closeModal(); }
    catch (e) { toast(e.message); $('#saveOmdb').textContent = 'Save'; }
  };
  $('#optAutoplay').onchange = (e) => { localSet('autoplay', e.target.checked ? '1' : '0'); post('/api/prefs', { autoplay: e.target.checked }).catch(() => {}); };
  $('#optAutoskip').onchange = (e) => { localSet('autoskip', e.target.checked ? '1' : '0'); post('/api/prefs', { autoskip: e.target.checked }).catch(() => {}); };
  $('#optAutoskipRecap').onchange = (e) => { localSet('autoSkipRecap', e.target.checked ? '1' : '0'); post('/api/prefs', { autoSkipRecap: e.target.checked }).catch(() => {}); };
  $('#optAutoskipCredits').onchange = (e) => { localSet('autoSkipCredits', e.target.checked ? '1' : '0'); post('/api/prefs', { autoSkipCredits: e.target.checked }).catch(() => {}); };
  $('#optSkipFillerSet').onchange = (e) => post('/api/prefs', { skipFiller: e.target.checked }).then(() => toast(e.target.checked ? 'Anime filler skipping on' : 'Anime filler skipping off')).catch(() => {});
  $('#optTv').onchange = (e) => setTvMode(e.target.checked);
  renderRemote(); renderTrakt(); renderDisplay(prefs); renderSysHealth();
  api('/api/account?fresh=1').then((acct) => {
    ACCOUNT = { ...acct, _t: Date.now() };
    updateSubPill();
    const box = $('#subAcctBox');
    if (box && box.isConnected) box.innerHTML = renderAccountHtml(acct);
  }).catch(() => {
    const box = $('#subAcctBox');
    if (box && box.isConnected && !ACCOUNT) box.innerHTML = '<p>Couldn’t reach the provider for account details.</p>';
  });
  $('#lbFile').onchange = async (e) => {
    const files = [...e.target.files]; if (!files.length) return; const st = $('#lbStatus');
    let body; let headers = {};
    if (files.length === 1 && /\.zip$/i.test(files[0].name)) { body = await files[0].arrayBuffer(); headers = { 'Content-Type': 'application/octet-stream' }; }
    else { const m = {}; for (const f of files) m[f.name.toLowerCase()] = await f.text(); body = JSON.stringify({ files: m }); headers = { 'Content-Type': 'application/json' }; }
    st.textContent = 'Uploading…';
    const r = await fetch('/api/import/letterboxd', { method: 'POST', headers, body }); const j = await r.json().catch(() => ({}));
    if (!r.ok) { st.textContent = j.error || 'Import failed'; return; }
    const poll = setInterval(async () => {
      const s = await api('/api/import/status').catch(() => null); if (!s) return;
      if (s.running) { st.textContent = `Matching films… ${s.done} of ${s.total}`; return; }
      clearInterval(poll); const x = s.result || {};
      st.textContent = `Done: ${x.watched} watched, ${x.liked} liked, ${x.disliked} disliked, ${x.watchlist} added to Wanted${x.unmatched?.length ? ` · ${x.unmatched.length} not matched` : ''}`;
      toast('Letterboxd imported. Recommendations will use it');
    }, 1500);
  };
  $('#syncNow').onclick = async () => { await post('/api/sync', {}); toast('Refreshing in the background. This takes about a minute.'); };
}

// ---------------------------------------------------------------- TV guide (time grid + catch-up)
const guideState = { group: null, from: null };
const GUIDE_PX_PER_MIN = 6; // 30 min = 180 px
async function pageGuide() {
  const my = routeGen;
  const { groups, favs } = await apiCached('/api/live/groups', 120000);
  if (my !== routeGen) return;
  // default: favourites, else the group with the most guide listings in your region (AU first)
  const bestIn = (cc) => groups.filter((g) => g.country === cc && g.epg > 0).sort((a, b) => b.epg - a.epg)[0]?.id;
  if (!guideState.group) guideState.group = favs.length ? 'favs' : bestIn('AU') || bestIn('UK') || bestIn('US') || groups[0]?.id;
  const half = 30 * 60e3; const now = Date.now();
  if (!guideState.from) guideState.from = Math.floor((now - 60 * 60e3) / half) * half;
  const hours = 6; const from = guideState.from; const to = from + hours * 3600e3;
  const regionOrder = ['AU', 'US', 'UK', 'CA'];
  const opts = [`<option value="favs">★ Favourites (${favs.length})</option>`, '<option value="catchup">⟲ Catch-up channels (watch past shows)</option>']
    .concat(groups.filter((g) => g.epg > 0).sort((a, b) => (regionOrder.indexOf(a.country) + 9) % 9 - (regionOrder.indexOf(b.country) + 9) % 9 || b.epg - a.epg).map((g) => `<option value="${esc(g.id)}">${g.sport ? '⚽ ' : ''}${esc(g.name)} · ${g.epg} with listings</option>`))
    .concat(`<optgroup label="No guide listings">${groups.filter((g) => !g.epg).map((g) => `<option value="${esc(g.id)}">${esc(g.name)}</option>`).join('')}</optgroup>`);
  view.innerHTML = `<div class="page-pad"><div class="page-head"><h1 class="page-title">TV Guide</h1>
    <div class="chips"><a class="chip" href="#/live">Channels</a><a class="chip on" href="#/guide">Guide</a></div></div>
    <div class="guide-bar"><select id="gGroup" aria-label="Channel group">${opts.join('')}</select>
      <button class="chip" id="gPrev" aria-label="Earlier">‹ Earlier</button><button class="chip" id="gNow">Now</button><button class="chip" id="gNext" aria-label="Later">Later ›</button>
      <span class="muted" id="gRange"></span><button class="chip" id="gAll" hidden></button><span class="muted g-legend" id="gLegend"></span></div>
    <div class="guide" id="guide"><div class="loading" style="min-height:240px"><div class="spinner"></div></div></div></div>`;
  $('#gGroup').value = guideState.group;
  $('#gGroup').onchange = (e) => { guideState.group = e.target.value; pageGuide(); };
  $('#gPrev').onclick = () => { guideState.from -= 3 * 3600e3; pageGuide(); };
  $('#gNext').onclick = () => { guideState.from += 3 * 3600e3; pageGuide(); };
  $('#gNow').onclick = () => { guideState.from = null; pageGuide(); };
  $('#gRange').textContent = `${dayLabel(from)} ${fmtClock(from)} – ${fmtClock(to)}`;
  const d = await api(`/api/guide?cat=${encodeURIComponent(guideState.group)}&from=${from}&hours=${hours}`);
  const W = hours * 60 * GUIDE_PX_PER_MIN; const x = (t) => ((Math.max(from, Math.min(to, t)) - from) / 60e3) * GUIDE_PX_PER_MIN;
  let ruler = ''; for (let t = from; t < to; t += half) ruler += `<span style="left:${x(t)}px">${fmtClock(t)}</span>`;
  // channels with listings first; the rest behind a toggle
  const withData = d.rows.filter((r) => r.progs.length); const without = d.rows.length - withData.length;
  const shown = guideState.all || !withData.length ? d.rows : withData;
  d.rows = shown;
  const anyCatch = shown.some((r) => r.archive);
  $('#gLegend').textContent = anyCatch ? '⟲ = watch from the start / catch-up' : 'Catch-up isn’t offered on these channels (your provider has it on UK & Irish channels)';
  if (without && withData.length) { const b = $('#gAll'); b.hidden = false; b.textContent = guideState.all ? 'Hide channels without listings' : `+ ${without} without listings`; b.onclick = () => { guideState.all = !guideState.all; pageGuide(); }; }
  const rows = d.rows.map((r, ri) => {
    const blocks = r.progs.map((p, pi) => {
      const past = p.e <= d.now; const live = p.s <= d.now && p.e > d.now; const canCatch = r.archive && p.s >= d.now - r.archive * 86400e3 && p.s < d.now;
      const w = Math.max(4, x(p.e) - x(p.s) - 2);
      const bar = p.progress > 0.02 && p.progress < 0.94 ? `<div class="gp-bar"><i style="width:${Math.round(p.progress * 100)}%"></i></div>` : '';
      return `<button class="gp ${past ? 'past' : ''} ${live ? 'now' : ''} ${canCatch ? 'cu' : ''} ${p.reminded ? 'reminded' : ''}" style="left:${x(p.s)}px;width:${w}px" data-gr="${ri}" data-gp="${pi}" title="${esc(p.title)} · ${fmtClock(p.s)}–${fmtClock(p.e)}">
        <b>${p.reminded ? '🔔 ' : ''}${canCatch && !live ? '⟲ ' : ''}${esc(p.title)}</b><small>${fmtClock(p.s)}</small>${bar}</button>`;
    }).join('');
    return `<div class="g-row"><button class="g-ch" data-gch="${ri}" title="${esc(r.title)}">${r.logo ? `<img src="${esc(r.logo)}" alt="" loading="lazy">` : ''}<span>${esc(r.title)}</span>${r.feeds.length > 1 ? `<small>${r.feeds.length} feeds</small>` : ''}</button>
      <div class="g-line" style="width:${W}px">${blocks || '<span class="g-none">No guide data for this channel</span>'}</div></div>`;
  }).join('');
  const nowX = d.now > from && d.now < to ? x(d.now) : -1;
  $('#guide').innerHTML = d.rows.length ? `<div class="g-scroll"><div class="g-ruler" style="width:${W}px">${ruler}</div>${nowX >= 0 ? `<div class="g-nowline" style="left:calc(var(--gch) + ${nowX}px)"></div>` : ''}${rows}</div>`
    : `<div class="empty">${guideState.group === 'favs' ? 'Star channels in Live TV (or pick a group above) to build your guide.' : 'No channels in this group.'}</div>`;
  const sc = $('.g-scroll'); if (sc && nowX > 200) sc.scrollLeft = nowX - 200;
  const feedsOf = (r) => r.feeds.map((f) => ({ id: f.id, channel: `${r.title} · ${f.label}`, label: f.label }));
  const live = (r, feedId) => playLive({ id: feedId || r.id, title: r.title, logo: r.logo, epg: r.epg, feeds: r.feeds.length > 1 ? feedsOf(r) : null });
  $$('[data-gch]').forEach((b) => (b.onclick = () => live(d.rows[+b.dataset.gch])));
  $$('[data-gp]').forEach((b) => (b.onclick = () => {
    const r = d.rows[+b.dataset.gr]; const p = r.progs[+b.dataset.gp];
    openProgSheet(r, p, (fid) => live(r, fid), () => {
      b.classList.toggle('reminded', !!p.reminded);
      const bEl = $('b', b);
      if (bEl) bEl.textContent = `${p.reminded ? '🔔 ' : ''}${r.archive && p.s < Date.now() && p.e <= Date.now() ? '⟲ ' : ''}${p.title}`;
    });
  }));
}
function openProgSheet(r, p, goLive, onRemindChange) {
  const now = Date.now();
  const isLive = p.s <= now && p.e > now;
  const isPast = p.e <= now;
  const isFuture = p.s > now;
  const canCatch = !!(r.archive && p.s >= now - r.archive * 86400e3 && p.s < now);
  const canResume = canCatch && p.resume > 30 && p.progress < 0.94;
  const durMin = Math.max(1, Math.round((p.e - p.s) / 60e3));
  const remId = `rem:${r.id}:${p.s}`;
  const badge = isLive ? '<span class="live-badge"><span class="live-dot"></span>ON NOW</span>' : isPast ? (canCatch ? '<span class="pill">Catch-up</span>' : '<span class="pill">Ended</span>') : '<span class="pill">Upcoming</span>';
  let btns = '';
  if (isLive) {
    btns += `<button class="btn btn-primary" id="psLive">${I.play}Watch live</button>`;
    if (canCatch) btns += `<button class="btn btn-ghost" id="psStart">⟲ From the start</button>`;
  } else if (isPast) {
    if (canResume) btns += `<button class="btn btn-primary" id="psResume">${I.play}Resume from ${fmtTime(p.resume)}</button><button class="btn btn-ghost" id="psStart">⟲ Start from beginning</button>`;
    else if (canCatch) btns += `<button class="btn btn-primary" id="psStart">${I.play}Watch catch-up</button>`;
    else btns += '<span class="muted">Catch-up isn’t available on this channel.</span>';
  } else if (isFuture) {
    btns += `<button class="btn ${p.reminded ? 'btn-ghost' : 'btn-primary'}" id="psRemind">${p.reminded ? '✓ Reminder set' : '🔔 Remind me'}</button>`;
    btns += `<button class="btn btn-ghost" id="psLive">Watch channel now</button>`;
  }
  const feedPicker = r.feeds?.length > 1
    ? `<div style="margin-bottom:14px"><label class="muted" style="display:block;margin-bottom:6px">Feed</label><select class="sel" id="psFeed">${r.feeds.map((f) => `<option value="${f.id}">${esc(f.label || f.channel)}</option>`).join('')}</select></div>`
    : '';
  openModal(`<div class="sheet settings prog-sheet"><button class="close" aria-label="Close">${I.close}</button>
    <h2>${esc(p.title)}</h2>
    <div class="ps-meta">${r.logo ? `<img src="${esc(r.logo)}" alt="" onerror="this.remove()">` : ''}<b>${esc(r.title)}</b><span>· ${esc(dayLabel(p.s))} ${fmtClock(p.s)}–${fmtClock(p.e)} (${runtime(durMin)})</span>${badge}</div>
    <p class="ps-desc">${esc(p.desc || 'No programme description available.')}</p>
    ${feedPicker}
    <div class="btns">${btns}</div></div>`);
  const pickedFeed = () => ($('#psFeed') ? +$('#psFeed').value : r.id);
  if ($('#psLive')) $('#psLive').onclick = () => { closeModal(); goLive(pickedFeed()); };
  if ($('#psResume')) $('#psResume').onclick = () => { closeModal(); playCatchup(r, p, p.resume); };
  if ($('#psStart')) $('#psStart').onclick = () => { closeModal(); playCatchup(r, p, 0); };
  if ($('#psRemind')) $('#psRemind').onclick = async () => {
    const next = !p.reminded;
    await post('/api/reminders', { id: remId, chId: pickedFeed(), chTitle: r.title, epg: r.epg || '', title: p.title, start: p.s, end: p.e, desc: p.desc || '', on: next }).catch((e) => toast(e.message));
    p.reminded = next;
    const b = $('#psRemind');
    if (b) { b.textContent = next ? '✓ Reminder set' : '🔔 Remind me'; b.className = `btn ${next ? 'btn-ghost' : 'btn-primary'}`; }
    onRemindChange?.();
    toast(next ? `Reminder set for ${p.title} (${fmtClock(p.s)})` : 'Reminder removed');
  };
}
function playCatchup(r, p, resumeSec) {
  const feeds = r.feeds.filter((f) => f.archive);
  const main = feeds[0] || r.feeds[0];
  const mins = Math.max(1, Math.ceil((p.e - p.s) / 60e3));
  const durSec = (p.e - p.s) / 1000;
  const startAt = resumeSec !== undefined ? resumeSec : (p.resume > 30 && p.progress < 0.94 ? p.resume : 0);
  const key = `cu:${r.id}:${p.s}`;
  const exp = p.s + (r.archive || 3) * 86400e3;
  const cuItem = { k: 'cu', id: main.id, rowId: r.id, title: p.title, sub: `${r.title} · ${dayLabel(p.s)} ${fmtClock(p.s)}`, poster: r.logo, cs: p.s, dur: mins, duration: durSec, exp, key, feeds: feeds.map((f) => ({ id: f.id, label: f.label })) };
  Player.open({ kind: 'catchup', id: main.id, title: p.title, sub: cuItem.sub, catchup: { cs: p.s, dur: mins },
    resume: startAt, duration: durSec, progressKey: key, label: main.label, alternates: feeds.slice(1).map((f) => ({ id: f.id, label: f.label })), item: cuItem });
}

// ---------------------------------------------------------------- Tonight
async function pageTonight() {
  const [t, sp] = await Promise.all([api('/api/tonight'), api('/api/sports').catch(() => ({ fixtures: [], events: [], teams: [] }))]);
  const now = t.now; const until = t.until;
  const games = sp.fixtures.filter((f) => !f.completed && f.start < until && f.start > now - 4 * 3600e3).sort((a, b) => (b.fav - a.fav) || a.start - b.start);
  const liveEv = sp.events.filter((e) => e.status === 'live' && e.en);
  const soonEv = sp.events.filter((e) => e.status === 'upcoming' && e.en && e.start < until);
  let html = `<div class="page-pad"><div class="page-head"><h1 class="page-title">Tonight</h1><span class="muted">${new Date().toLocaleDateString([], { weekday: 'long', day: 'numeric', month: 'long' })} · until ${fmtClock(until)}</span></div>`;
  if (t.newEps.length) html += renderRow({ id: 'tn-new', title: 'New episodes for you', items: t.newEps });
  if (games.length) html += `<div class="section-label">Games today & tonight</div>` + fixtureRows(games);
  if (liveEv.length) html += row({ id: 'tn-live', title: 'Live now', sub: `${liveEv.length}` }, liveEv.slice(0, 40).map(eventCard).join(''));
  if (soonEv.length) html += row({ id: 'tn-soon', title: 'Starting later', sub: `${soonEv.length}` }, soonEv.slice(0, 40).map(eventCard).join(''));
  html += `<div class="section-label">${t.mine ? 'On your channels' : `On TV tonight · ${esc(t.city)}`}</div>`;
  html += t.favs.length ? `<div class="tn-chs">${t.favs.map((c, ci) => `<div class="tn-ch"><button class="tn-logo" data-tch="${ci}" title="Watch ${esc(c.title)} live">${c.logo ? `<img src="${esc(c.logo)}" alt="" loading="lazy">` : ''}<b>${esc(c.title)}</b></button>
      <div class="tn-progs">${c.progs.map((p) => `<div class="tn-p ${p.s <= now ? 'now' : ''}"><time>${p.s <= now ? 'Now' : fmtClock(p.s)}</time><span>${esc(p.title)}</span></div>`).join('')}</div></div>`).join('')}</div>`
    : `<div class="empty">No TV listings available right now.</div>`;
  if (!t.mine && t.favs.length) html += `<p class="muted" style="padding:6px var(--gutter,4%)">Star channels in Live TV to choose which ones show here.</p>`;
  if (!t.newEps.length && !games.length && !liveEv.length && !t.favs.length) html += `<div class="empty">Nothing lined up yet. Follow shows, star teams and channels to fill this page.</div>`;
  view.innerHTML = html + '</div>';
  $$('[data-tch]').forEach((b) => (b.onclick = () => { const c = t.favs[+b.dataset.tch]; playLive({ id: c.id, title: c.title, epg: c.epg }); }));
  wireRows();
}

// ---------------------------------------------------------------- couch mode: arrows / gamepad / remote
// Arrow keys move focus to the nearest thing in that direction (cards, buttons, chips); Enter
// opens; Backspace/Esc goes back. A gamepad's d-pad/A/B do the same, and TV mode enlarges the UI.
const NAV_SEL = '.card,.wide,.event,.ch,button:not([disabled]),a[href],select,input,[tabindex="0"]';
function navVisible(e) { if (e.closest('[hidden]')) return false; const r = e.getBoundingClientRect(); return r.width > 4 && r.height > 4; }
function navMove(dir) {
  const playerOpen = !$('#player').hidden;
  if (playerOpen) $('#player').dispatchEvent(new Event('mousemove'));
  const root = playerOpen ? $('#player') : !modal.hidden ? modal : document;
  const els = [...root.querySelectorAll(NAV_SEL)].filter((e) => (playerOpen || !e.closest('#player')) && navVisible(e));
  const cur = els.includes(document.activeElement) ? document.activeElement : null;
  const vw = innerWidth; const vh = innerHeight;
  if (!cur) {
    const first = playerOpen
      ? ($('#pPlay', root) || els[0])
      : (els.find((e) => { const r = e.getBoundingClientRect(); return r.top >= 60 && r.top < vh && r.left >= 0 && r.left < vw && e.matches('.card,.wide,button'); }) || els[0]);
    first?.focus(); return;
  }
  const r = cur.getBoundingClientRect(); const cx = r.left + r.width / 2; const cy = r.top + r.height / 2;
  let best = null; let bs = Infinity;
  for (const e of els) {
    if (e === cur) continue;
    const q = e.getBoundingClientRect(); const ex = q.left + q.width / 2; const ey = q.top + q.height / 2; const dx = ex - cx; const dy = ey - cy;
    let score;
    if (dir === 'right') { if (q.left < r.right - 6) continue; score = dx + Math.abs(dy) * 3; }
    else if (dir === 'left') { if (q.right > r.left + 6) continue; score = -dx + Math.abs(dy) * 3; }
    else if (dir === 'down') { if (q.top < r.bottom - 6) continue; score = dy + Math.abs(dx) * 0.6 + (q.right < 0 || q.left > vw ? 4000 : 0); }
    else { if (q.bottom > r.top + 6) continue; score = -dy + Math.abs(dx) * 0.6 + (q.right < 0 || q.left > vw ? 4000 : 0); }
    if (score < bs) { bs = score; best = e; }
  }
  if (best) { best.focus({ preventScroll: true }); best.scrollIntoView({ block: 'nearest', inline: 'nearest', behavior: 'smooth' }); if (best.getBoundingClientRect().top < 70) scrollBy({ top: -90, behavior: 'smooth' }); }
}
function navBack() {
  if (!$('#player').hidden) return Player.close();
  if (!modal.hidden) return closeModal();
  if (location.hash && !/^#\/(home)?$/.test(location.hash)) history.back();
}
document.addEventListener('keydown', (e) => {
  if (!$('#player').hidden || !$('#cmdk')?.hidden) return; // player or command palette owns keys
  const t = e.target;
  const typing = t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT' || t.isContentEditable;
  const dir = { ArrowRight: 'right', ArrowLeft: 'left', ArrowDown: 'down', ArrowUp: 'up' }[e.key];
  if (dir) {
    // Never hijack native <select> dropdown arrows or desktop scrolling outside TV/couch mode
    if (t.tagName === 'SELECT') return;
    if (!document.body.classList.contains('tv') && e.isTrusted) return;
    if (typing && (dir === 'left' || dir === 'right')) return;
    e.preventDefault();
    if (typing) t.blur();
    navMove(dir);
    return;
  }
  if (e.key === 'Backspace' && !typing) { e.preventDefault(); navBack(); }
  if (e.key === 'Enter' && !e.isTrusted && t.matches('button,a,[data-open],[data-quickplay],[data-resume],[data-live],[data-fixture],.rc,.fcard,.ep,.ld-item,.radar-card,.mar-step,.coll-item')) t.click();
});

// TV mode: bigger everything, clear focus ring, hidden cursor when idle.
let REMOTE_ON = false;
let pushRemoteState = () => {};
function setTvMode(on, quiet) {
  document.body.classList.toggle('tv', on);
  localSet('tvmode', on ? '1' : '0');
  if (!quiet) toast(on ? 'TV mode on. Use arrows / a gamepad / your phone' : 'TV mode off');
  pushRemoteState();
}
if (localGet('tvmode', '0') === '1') setTvMode(true, true);
{ let ct; document.addEventListener('mousemove', () => { document.body.classList.remove('nocursor'); clearTimeout(ct); ct = setTimeout(() => document.body.classList.contains('tv') && document.body.classList.add('nocursor'), 3000); }); }

// Gamepad: d-pad / left stick = arrows, A = select, B = back, Start = play/pause, X = subtitles, Y = search.
(() => {
  let raf = 0; const held = {}; const fire = (key) => {
    const t = document.activeElement || document.body;
    if (key === 'Enter' && t.matches?.(NAV_SEL)) { t.click(); return; }
    if (key === 'Backspace') { navBack(); return; }
    t.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }));
  };
  const map = [[12, 'ArrowUp'], [13, 'ArrowDown'], [14, 'ArrowLeft'], [15, 'ArrowRight'], [0, 'Enter'], [1, 'Backspace'], [9, ' '], [2, 'c'], [3, '/']];
  function poll() {
    const pads = navigator.getGamepads ? [...navigator.getGamepads()].filter(Boolean) : [];
    const now = performance.now();
    for (const gp of pads) {
      const ax = gp.axes || []; const stick = { 12: ax[1] < -0.6, 13: ax[1] > 0.6, 14: ax[0] < -0.6, 15: ax[0] > 0.6 };
      for (const [b, key] of map) {
        const down = !!(gp.buttons[b]?.pressed || stick[b]); const h = held[b];
        if (down && !h) { held[b] = { at: now, next: now + 380 }; if (key === ' ' && $('#player').hidden) continue; fire(key === '/' && !$('#player').hidden ? 'f' : key); }
        else if (down && h && b >= 12 && now >= h.next) { h.next = now + 110; fire(key); }
        else if (!down && h) delete held[b];
      }
    }
    raf = pads.length ? requestAnimationFrame(poll) : 0;
  }
  addEventListener('gamepadconnected', () => { if (!document.body.classList.contains('tv')) setTvMode(true); if (!raf) raf = requestAnimationFrame(poll); });
})();

// ---------------------------------------------------------------- phone remote (TV side)
window.__player = Player;
(() => {
  let es; let retry = 1000;
  let lastSig = ''; let lastPushAt = 0;
  pushRemoteState = (force = false) => {
    if (!REMOTE_ON) return;
    const snap = Player.snapshot();
    const payload = { player: snap, page: location.hash || '#/home', tv: document.body.classList.contains('tv') };
    const sig = JSON.stringify(snap ? {
      p: payload.page, tv: payload.tv, t: snap.title, s: snap.sub, l: snap.live,
      pa: snap.paused, pos: Math.floor((snap.pos || 0) / 2), d: Math.floor(snap.dur || 0),
      v: Math.round((snap.vol ?? 1) * 100), m: snap.muted, su: snap.subs, hs: snap.hasSubs,
      a: snap.audio, src: snap.src, c: snap.casting, pr: snap.pro, n: snap.night,
      sk: snap.skip, us: snap.undoSkip, nx: snap.next,
    } : { p: payload.page, tv: payload.tv, idle: 1 });
    const now = Date.now();
    if (!force && sig === lastSig && (!snap || now - lastPushAt < 2500)) return;
    lastSig = sig;
    lastPushAt = now;
    post('/api/remote/state', payload).catch(() => {});
  };
  function connect() {
    es = new EventSource('/api/remote/events');
    es.onopen = () => { retry = 1000; };
    es.onmessage = async (ev) => {
      let m; try { m = JSON.parse(ev.data); } catch { return; }
      if (m.cmd === 'alert') { showAlert(m); return; }
      AUTOPLAY.chain = 0;
      if (m.cmd === 'hello') { REMOTE_ON = !!m.enabled; if (REMOTE_ON) pushRemoteState(true); return; }
      if (m.cmd === 'paired') { toast('Phone remote paired'); if ($('#remoteBox')) renderRemote(); pushRemoteState(true); return; }
      if (m.cmd === 'reload' && document.hidden) return;
      if (m.cmd === 'play') {
        try {
          if (m.k === 'm') return quickPlay({ k: 'm', id: m.id });
          if (m.k === 'e') return resumeItem({ k: 'e', id: m.id, sid: m.sid });
          if (m.k === 'l') {
            if (m.title) return playLive({ id: +m.id, title: m.title, epg: m.epg || '', logo: m.logo || '' });
            const ch = await api(`/api/live/channel/${m.id}`).catch(() => null);
            return playLive(ch || { id: +m.id, title: 'Live TV' });
          }
          if (m.k === 's') { const d = await api(`/api/series/${m.id}`); const flat = d.seasons.flatMap((x) => x.episodes.map((e) => ({ ...e, season: x.n }))); const nu = d.nextUp && flat.find((x) => x.id === d.nextUp.id); if (nu) return playEpisode(d, flat, nu, nu.progress < 0.94 ? nu.resume : 0); return openDetail({ k: 's', id: m.id }); }
        } catch (e) { toast(e.message); }
        return;
      }
      if (m.cmd === 'reload') { await Player.close().catch(() => {}); return location.reload(); }
      if (m.cmd === 'tvmode') return setTvMode(!document.body.classList.contains('tv'));
      if (m.cmd === 'key') {
        const key = String(m.v || '');
        if (key === 'Back') return navBack();
        if (key === 'Enter') { const t = document.activeElement; if (t && t.matches?.(NAV_SEL)) return t.click(); }
        (document.activeElement || document.body).dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }));
        setTimeout(() => pushRemoteState(), 150);
        return;
      }
      Player.cmd(m.cmd, m.v);
      setTimeout(() => pushRemoteState(), 150);
    };
    es.onerror = () => { es.close(); setTimeout(connect, retry); retry = Math.min(retry * 2, 15000); };
  }
  connect();
  // Only push every 2.5s while player is open (or on actual state change); zero spam when idle!
  setInterval(() => { if (REMOTE_ON && !$('#player').hidden) pushRemoteState(); }, 2500);
})();
async function renderTrakt() {
  const box = $('#traktBox'); if (!box) return;
  const t = await api('/api/trakt').catch(() => null); if (!t || !box.isConnected) return;
  if (t.connected) {
    box.innerHTML = `<p><b>Trakt</b>: connected${t.user ? ` as <b>${esc(t.user)}</b>` : ''}. History, ratings and watchlist sync every 6 hours; what you finish, rate or add here is sent to Trakt.${t.lastSync ? ` Last sync ${new Date(t.lastSync).toLocaleString([], { dateStyle: 'medium', timeStyle: 'short' })}.` : ''}</p>
      ${t.result ? `<p class="muted">Last pull: ${t.result.watched} films, ${t.result.episodes} episodes, ${t.result.ratings} ratings, ${t.result.watchlist} watchlist.</p>` : ''}
      <div class="btns"><button class="btn btn-ghost btn-sm" id="trSync">${t.syncing ? 'Syncing…' : 'Sync now'}</button><button class="btn btn-ghost btn-sm" id="trOff">Disconnect</button></div>`;
    $('#trSync').onclick = async () => { await post('/api/trakt/sync', {}); toast('Syncing with Trakt in the background'); setTimeout(renderTrakt, 4000); };
    $('#trOff').onclick = async () => { await post('/api/trakt/disconnect', {}); renderTrakt(); };
    return;
  }
  if (t.pending && t.pending.until > Date.now()) {
    box.innerHTML = `<p><b>Trakt</b>: open <a href="${esc(t.pending.url)}" target="_blank" rel="noopener">${esc(t.pending.url.replace(/^https?:\/\//, ''))}</a> and enter</p><div class="pair-code">${esc(t.pending.code)}</div><p class="muted">Waiting for you to approve…</p>`;
    setTimeout(() => $('#traktBox') && renderTrakt(), 4000); return;
  }
  box.innerHTML = `<p><b>Trakt</b>${t.error ? ` <span class="muted">(${esc(t.error)})</span>` : ''}: two-way sync of history, ratings and watchlist.
      ${t.hasApp ? '' : `One-time setup: on <a href="https://trakt.tv/oauth/applications/new" target="_blank" rel="noopener">trakt.tv/oauth/applications/new</a> create an app (any name; Redirect URI <span class="mono">urn:ietf:wg:oauth:2.0:oob</span>), then paste its Client ID and Secret.`}</p>
    ${t.hasApp ? '' : `<div class="field"><input id="trId" placeholder="Client ID" autocomplete="off"></div><div class="field"><input id="trSec" type="password" placeholder="Client Secret" autocomplete="off"></div>`}
    <div class="btns"><button class="btn btn-accent btn-sm" id="trGo">Connect Trakt</button></div>`;
  $('#trGo').onclick = async () => {
    try { await post('/api/trakt/connect', { clientId: $('#trId')?.value || undefined, clientSecret: $('#trSec')?.value || undefined }); renderTrakt(); }
    catch (e) { toast(e.message); }
  };
}

async function renderDisplay(prefs) {
  const box = $('#dispBox'); if (!box) return;
  const [d, m] = await Promise.all([api('/api/display').catch(() => null), api('/api/mpv').catch(() => ({}))]);
  if (!box.isConnected) return;
  if (!d) { box.innerHTML = '<p class="muted">Display info unavailable.</p>'; return; }
  const t = d.displays.find((x) => x.target) || d.displays[0]; const max = t ? Math.max(...t.rates) : 0;
  const tog = (id, on, label) => `<label class="tog"><input type="checkbox" id="${id}" ${on ? 'checked' : ''}> ${label}</label>`;
  const peak = prefs.oledPeak || 'auto';
  box.innerHTML = `${d.displays.length > 1 ? `<div class="field"><select id="dispSel">${d.displays.map((x) => `<option value="${esc(x.gdi)}" ${x.target ? 'selected' : ''}>${esc(x.name)} · ${x.width}×${x.height} · ${x.hz} Hz${x.hdrSupported ? ' · HDR' : ''}</option>`).join('')}</select></div>` : ''}
    ${t ? `<div class="stat"><span>Screen</span><b>${esc(t.name)}</b><span>Mode</span><b>${t.width}×${t.height} at ${t.hz} Hz (up to ${max} Hz)</b><span>HDR</span><b>${t.hdrSupported ? (t.hdrOn ? 'On' : 'Off (switched on for HDR titles)') : 'Not supported'}</b></div>` : ''}
    <div class="hide-grid" style="margin-top:12px">${tog('optRate', d.matchRate, 'Match the refresh rate to each video (no judder on films)')}
      ${tog('optHdr', d.autoHdr, 'Turn Windows HDR on automatically for HDR & Dolby Vision titles')}
      ${tog('optSdrOffHdr', d.sdrOffHdr !== false && prefs.sdrOffHdr !== false, 'OLED True Black: turn Windows HDR off for SDR titles & restore afterwards (fixes washed-out grey blacks)')}
      ${tog('opt4k', prefs.prefer4k !== false, 'Pick the best picture first (4K / surround)')}
      ${tog('optPrefHdr', !!prefs.preferHdr, 'Prefer HDR copies in the browser player too (mpv already plays HDR natively)')}
      ${tog('optMse', prefs.mse !== undefined ? !!prefs.mse : localGet('mse', '1') === '1', 'Fast seeking & seamless version switching (MSE engine)')}
      ${tog('optOled', localGet('oled', '1') === '1', 'OLED care (black screensaver, pixel shift, quick-hiding controls)')}</div>
    ${t && max >= t.hz * 1.5 ? `<p class="tip">Your screen runs at ${t.hz} Hz. Setting Windows to <b>${max} Hz</b> (Settings › System › Display › Advanced display › refresh rate) makes menus smoother, and ${max % 24 === 0 ? '24, 30 and 60 fps then play without any switching' : 'more videos play without switching'}.</p>` : ''}
    <p class="tip"><b>OLED True Black tip:</b> Windows 11 lifts near-blacks on SDR video whenever desktop HDR is on. Keep <b>OLED True Black</b> checked above (and leave NVIDIA <i>RTX HDR</i> off if blacks look grey); in the NVIDIA App you can still enable <b>RTX Video Super Resolution</b> (quality 4) for crisp 1080p→4K upscaling.</p>
    <h4 style="margin-top:18px">Pro player (mpv · OLED tuned)</h4>
    <p>Plays straight to your OLED with infinite contrast (0-nit blacks), grain-free 10-bit PQ HDR / Dolby Vision, pure gamma 2.2 SDR, intro auto-skip, auto-next episode, and GPU upscaling. ${m.available ? '<span class="ok">Installed.</span>' : 'Not installed yet.'}</p>
    ${m.available ? `<div class="field"><select id="proSel">${[['hdr4k', 'Use mpv for all 4K HDR & Dolby Vision titles (recommended)'], ['dv', 'Use mpv for Dolby Vision titles only'], ['all', 'Use mpv for all films and episodes'], ['never', 'Only when I press P']].map(([v, l]) => `<option value="${v}" ${(prefs.pro || localGet('pro', 'hdr4k')) === v ? 'selected' : ''}>${l}</option>`).join('')}</select></div>
      <div class="field" style="margin-top:8px"><select id="shaderSel">${[['auto', 'GPU Shaders: Auto (Anime4K for Anime + FSRCNNX for 1080p films)'], ['anime', 'GPU Shaders: Anime4K for Anime only'], ['anime_hq', 'GPU Shaders: Anime4K HQ (VL) for Anime'], ['off', 'GPU Shaders: Off (Ctrl+1/2/3 still toggles live in mpv)']].map(([v, l]) => `<option value="${v}" ${(prefs.mpvShaders || 'auto') === v ? 'selected' : ''}>${l}</option>`).join('')}</select></div>
      <div class="field" style="margin-top:8px"><select id="peakSel">${[['auto', 'OLED Peak Brightness: Auto (from Windows HDR EDID)'], ['400', 'OLED Peak: 400 nits (TrueBlack 400)'], ['600', 'OLED Peak: 600 nits'], ['800', 'OLED Peak: 800 nits'], ['1000', 'OLED Peak: 1000 nits (Peak 1000)']].map(([v, l]) => `<option value="${v}" ${peak === v ? 'selected' : ''}>${l}</option>`).join('')}</select></div>
      <div class="hide-grid" style="margin-top:10px">
        ${tog('optMpvVsr', !!prefs.mpvVsr, 'RTX Video Super Resolution in mpv (upscales 1080p/720p & live TV on your GPU)')}
        ${tog('optMpvAnimeSharp', prefs.mpvAnimeSharp !== false, 'Crisp anime upscaling in mpv (ewa_lanczossharp + light sharpen)')}
      </div>`
      : `<div class="btns"><button class="btn btn-accent btn-sm" id="proInstall">Install mpv (free, via winget)</button><span class="muted" id="proSt"></span></div>`}`;
  const setp = (b) => post('/api/prefs', b).catch((e) => toast(e.message));
  $('#dispSel')?.addEventListener('change', (e) => setp({ display: e.target.value }).then(() => renderDisplay(prefs)));
  $('#optRate').onchange = (e) => setp({ matchRate: e.target.checked });
  $('#optHdr').onchange = (e) => setp({ autoHdr: e.target.checked });
  $('#optSdrOffHdr').onchange = (e) => { setp({ sdrOffHdr: e.target.checked }); prefs.sdrOffHdr = e.target.checked; toast(e.target.checked ? 'OLED True Black enabled for SDR' : 'SDR HDR-off disabled'); };
  $('#optPrefHdr').onchange = (e) => { setp({ preferHdr: e.target.checked }); prefs.preferHdr = e.target.checked; };
  $('#opt4k').onchange = async (e) => { await setp({ prefer4k: e.target.checked }); prefs.prefer4k = e.target.checked; toast(e.target.checked ? 'Best picture first' : 'HD versions first (less data)'); };
  $('#optMse').onchange = (e) => { localSet('mse', e.target.checked ? '1' : '0'); setp({ mse: e.target.checked }); toast(e.target.checked ? 'MSE engine enabled' : 'Direct stream mode enabled'); };
  $('#optOled').onchange = (e) => { localSet('oled', e.target.checked ? '1' : '0'); document.body.classList.toggle('oled', e.target.checked); };
  $('#proSel')?.addEventListener('change', (e) => { localSet('pro', e.target.value); setp({ pro: e.target.value }); });
  $('#shaderSel')?.addEventListener('change', (e) => { prefs.mpvShaders = e.target.value; setp({ mpvShaders: e.target.value }); toast('GPU shader preset saved'); });
  $('#peakSel')?.addEventListener('change', (e) => { prefs.oledPeak = e.target.value; setp({ oledPeak: e.target.value }); toast(`OLED peak set to ${e.target.value}`); });
  $('#optMpvVsr')?.addEventListener('change', (e) => { prefs.mpvVsr = e.target.checked; setp({ mpvVsr: e.target.checked }); toast(e.target.checked ? 'RTX Video Super Resolution enabled in mpv' : 'RTX VSR disabled in mpv'); });
  $('#optMpvAnimeSharp')?.addEventListener('change', (e) => { prefs.mpvAnimeSharp = e.target.checked; setp({ mpvAnimeSharp: e.target.checked }); });
  $('#proInstall')?.addEventListener('click', async () => {
    const st = $('#proSt'); st.textContent = 'Installing… (a minute or two)'; $('#proInstall').disabled = true;
    try { await post('/api/mpv/install', {}); for (let i = 0; i < 90; i++) { await new Promise((ok) => setTimeout(ok, 3000)); const r = await api('/api/mpv').catch(() => ({})); if (r.available) { toast('Pro player installed'); return renderDisplay(prefs); } if (r.installError) { st.textContent = r.installError; break; } } }
    catch (e) { st.textContent = e.message; }
    $('#proInstall').disabled = false;
  });
}

async function renderSysHealth() {
  const box = $('#sysHealthBox'); if (!box) return;
  const h = await api('/api/syshealth').catch(() => null);
  if (!box.isConnected) return;
  if (!h || !h.net) {
    box.innerHTML = '<p class="muted">System health activates after the queued server update finishes.</p>';
    return;
  }
  const link = h.net.linkSpeed || (h.net.mbps ? `${h.net.mbps} Mbps` : 'Unknown');
  const linkClass = h.net.slow ? 'warn-txt' : 'ok';
  const netWarn = h.net.slow ? `<p class="tip warn-tip"><b>Heads up:</b> Your Ethernet link (${esc(h.net.name || 'Adapter')}) dropped to <b>${esc(link)}</b>. Re-seat the cable or disable Green Ethernet / Energy-Efficient Ethernet in Device Manager to get back to 1 Gbps.</p>` : '';
  const cal = h.hdrCal;
  const calWarn = cal?.raisedBlacks
    ? `<p class="tip warn-tip"><b>OLED Black-Level Alert:</b> Your Windows HDR Calibration profile (<code>${esc(cal.file)}</code>) sets the minimum black floor to <b>${cal.minNits} nits</b> (and peak ${cal.peakNits} nits) instead of <b>0.00 nits</b> — this lifts deep OLED blacks to dark grey whenever Windows HDR is on. Fix: open the <b>Windows HDR Calibration</b> app, drag Step 1 (Minimum Luminance) all the way left to <b>0</b>, set Step 2 & 3 to your OLED's true peak (~1000 nits), and save (or remove the profile in Settings › System › Display › Color management).</p>`
    : '';
  const d = h.display;
  box.innerHTML = `<div class="stat">
    <span>Network link</span><b class="${linkClass}">${esc(link)}${h.net.name ? ` · ${esc(h.net.name)}` : ''}</b>
    <span>OLED state</span><b>${d ? `${esc(d.name)} · ${d.hz} Hz · HDR ${d.hdrOn ? 'On' : 'Off'}${h.displayChanged ? ' (temp)' : ''}` : '—'}</b>
    ${cal ? `<span>HDR calibration</span><b class="${cal.raisedBlacks ? 'warn-txt' : 'ok'}">Black ${cal.minNits} nits · Peak ${cal.peakNits} nits</b>` : ''}
    <span>Playback</span><b>${h.mpv?.playing ? 'mpv active' : h.streaming ? 'Browser stream active' : 'Idle'}</b>
    <span>Anime dub cache</span><b>${h.dubs?.verifiedYes ?? 0} English dubs · ${h.dubs?.verifiedNo ?? 0} sub-only hidden</b>
    <span>Probed streams</span><b>${h.probes ?? 0} cached · ${h.health?.penalised ?? 0} penalised</b>
  </div>${calWarn}${netWarn}
  <div class="btns" style="margin-top:12px">
    <button class="btn btn-ghost btn-sm" id="btnRestoreDisp">Restore OLED display (Hz/HDR)</button>
    <button class="btn btn-ghost btn-sm" id="btnResetHealth">Reset stream health penalties</button>
    <button class="btn btn-ghost btn-sm" id="btnRefreshHealth">Refresh</button>
  </div>`;
  $('#btnRestoreDisp', box).onclick = async () => { await post('/api/display/restore', {}).catch(() => {}); toast('Display restore requested'); renderSysHealth(); };
  $('#btnResetHealth', box).onclick = async () => { await post('/api/health/reset', {}).catch(() => {}); toast('Stream health history reset'); renderSysHealth(); };
  $('#btnRefreshHealth', box).onclick = () => renderSysHealth();
}

async function renderRemote() {
  const box = $('#remoteBox'); if (!box) return;
  const r = await api('/api/remote').catch(() => null); if (!r || !box.isConnected) return;
  REMOTE_ON = r.enabled;
  box.innerHTML = r.enabled ? `<div class="remote-pair">${r.qr ? `<div class="qr">${r.qr}</div>` : ''}<div>
      <p>On your phone (same Wi-Fi), open<br><b class="mono">${esc(r.url || 'this PC’s address, port 7880')}</b><br>or scan the code, then enter</p>
      <div class="pair-code">${esc((r.code || '').replace(/(\d{3})(\d{3})/, '$1 $2'))}</div>
      <p class="muted">${r.paired ? `${r.paired} phone${r.paired === 1 ? '' : 's'} paired. ` : ''}If Windows asks whether to allow “tv-server” on private networks, choose Allow.</p>
      <div class="btns"><button class="btn btn-ghost btn-sm" id="remoteOff">Turn off</button>${r.paired ? '<button class="btn btn-ghost btn-sm" id="remoteForget">Unpair phones</button>' : ''}</div></div></div>`
    : `<p>Control playback, browse and search from your phone over your home Wi-Fi. Off until you switch it on, and phones need a one-time pairing code.</p><div class="btns"><button class="btn btn-accent btn-sm" id="remoteOn">Turn on phone remote</button></div>`;
  $('#remoteOn', box)?.addEventListener('click', async () => { await post('/api/remote', { enabled: true }); renderRemote(); });
  $('#remoteOff', box)?.addEventListener('click', async () => { await post('/api/remote', { enabled: false }); renderRemote(); });
  $('#remoteForget', box)?.addEventListener('click', async () => { await post('/api/remote', { forget: true }); toast('Phones unpaired'); renderRemote(); });
}

route();
})();
