// Progressive transport only. Rust owns rendering, authorization and playlist decisions.
(() => {
  'use strict';
  const actions = new Set(['/build', '/refine', '/sync', '/coverage', '/pause', '/publish',
    '/preview/edit', '/preview/abandon', '/history/reuse']);
  let timer, pending = false, polling = false, observedRunning = false, failures = 0, lastStatus = '';
  let refreshVersion = 0;
  let autoSyncAttempted = false;
  let removalTimer, removing = false, removals = [], editFailure = false;
  const editsPending = () => removing || removals.length > 0 || editFailure;
  function updateRemovalUI() {
    const rows = [...document.querySelectorAll('.track-row')];
    document.querySelectorAll('.result-count').forEach(el => el.textContent = `${rows.length} tracks`);
    rows.forEach((row, index) => {
      row.querySelector('.track-number').textContent = String(index + 1).padStart(2, '0');
      row.querySelectorAll('input[name="index"]').forEach(el => el.value = index);
      row.querySelectorAll('form').forEach(form => {
        const command = form.elements.command?.value;
        const button = form.querySelector('button');
        if (button && command && command !== 'remove') button.disabled = editsPending() || (command === 'up' && index === 0) || (command === 'down' && index === rows.length - 1);
      });
    });
    document.querySelectorAll('.save-form button,.build-button,.refinement-form button').forEach(el => el.disabled = editsPending() || isRunning() || (!rows.length && el.closest('.save-form')));
    document.querySelector('.preview')?.setAttribute('aria-busy', String(editsPending()));
  }
  async function flushRemovals() {
    if (removing || !removals.length) return;
    removing = true;
    const batch = removals.splice(0);
    updateRemovalUI();
    const csrf = document.querySelector('meta[name="csrf-token"]').content;
    const preview_id = document.querySelector('#selection').dataset.previewId;
    try {
      const response = await request('/preview/edit', {method:'POST', credentials:'same-origin', cache:'no-store', body:new URLSearchParams({csrf, preview_id, command:'remove_many', removed:JSON.stringify(batch)})});
      if (!response.ok) throw new Error('Edit not confirmed');
      const result = JSON.parse(response.text);
      if (typeof result.preview_id !== 'string') throw new Error('Invalid edit receipt');
      document.querySelector('#selection').dataset.previewId = result.preview_id;
      document.querySelectorAll('input[name="preview_id"]').forEach(el => el.value = result.preview_id);
    } catch {
      removals = [];
      editFailure = true;
      try { await refresh(true, true); editFailure = false; } catch { /* Keep export locked until authoritative state is available. */ }
      message(editFailure ? 'Could not confirm removals. Reload to restore saved state before exporting.' : 'Could not confirm every removal. The saved selection has been restored; review it before exporting.');
    } finally {
      removing = false;
      updateRemovalUI();
      if (removals.length) flushRemovals();
    }
  }
  function autoSync() {
    const form = document.querySelector('#auto-sync');
    if (form && !autoSyncAttempted && !pending && !isRunning()) {
      autoSyncAttempted = true;
      form.requestSubmit();
    }
  }
  const status = () => document.querySelector('#job-status');
  const isRunning = () => status()?.dataset.running === 'true';
  // Mobile browsers can suspend a request when Spotify opens or the phone locks.
  // Bound the entire response read so a stranded request cannot hold the UI busy.
  async function request(url, options = {}) {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 20000);
    try {
      const response = await fetch(url, {...options, signal: controller.signal});
      return {ok: response.ok, status: response.status, text: await response.text()};
    } finally { clearTimeout(timeout); }
  }
  function message(text) {
    const node = status();
    if (!node) return;
    node.hidden = false;
    const target = node.querySelector('.job-label span:last-child');
    if (target) target.textContent = text;
  }
  function schedule(delay = 1000) {
    clearTimeout(timer);
    timer = setTimeout(poll, delay);
  }
  function composerValues() {
    const form = document.querySelector('.composer-form');
    return form ? [...form.elements].filter(el => el.name && el.type !== 'hidden')
      .map(el => [el.name, el.value, el.checked]) : [];
  }
  function renderPage(text, preserve = true) {
    const next = new DOMParser().parseFromString(text, 'text/html');
    const main = next.querySelector('main');
    if (!main) throw new Error('Invalid page response');
    const values = preserve ? composerValues() : [];
    const refinementValue = preserve ? document.querySelector('.refinement-form textarea')?.value : '';
    const nameInput = document.querySelector('.save-form input[name="name"]');
    const nameValue = preserve && nameInput?.dataset.edited === 'true' ? nameInput.value : null;
    const details = [...document.querySelectorAll('details')].map(el => el.open);
    const scrolls = [...document.querySelectorAll('.scroll-region')].map(el => el.scrollTop);
    const focused = document.activeElement;
    const focusForm = focused?.closest('.refinement-form') ? '.refinement-form' : '.composer-form';
    const focusName = focused?.closest(focusForm) ? focused.name : null;
    const selection = focused?.selectionStart;
    const y = scrollY;
    const mainY = preserve ? document.querySelector('main').scrollTop : 0;
    document.querySelector('main').replaceWith(main);
    document.body.className = next.body.className;
    // Keep the live SDK player and its status node across server-rendered patches.
    for (const selector of ['#radio-player', '#playback-status']) {
      const existing = document.querySelector(selector), replacement = next.querySelector(selector);
      if (existing && replacement) {
        if (selector === '#radio-player') existing.dataset.connected = replacement.dataset.connected;
        replacement.replaceWith(existing);
      }
    }
    const csrf = next.querySelector('meta[name="csrf-token"]');
    if (csrf) document.querySelector('meta[name="csrf-token"]')?.setAttribute('content', csrf.content);
    for (const selector of ['body > header', 'body > footer']) {
      const replacement = next.querySelector(selector);
      if (replacement) document.querySelector(selector)?.replaceWith(replacement);
    }
    document.title = next.title;
    for (const [name, value, checked] of values) {
      const el = document.querySelector('.composer-form')?.elements.namedItem(name);
      if (el) { el.value = value; if (el.type === 'checkbox') el.checked = checked; }
    }
    const refinement = document.querySelector('.refinement-form textarea');
    if (refinement && refinementValue) refinement.value = refinementValue;
    if (nameValue !== null) {
      const input = document.querySelector('.save-form input[name="name"]');
      if (input) { input.value = nameValue; input.dataset.edited = 'true'; }
    }
    document.querySelectorAll('details').forEach((el, i) => { if (details[i] !== undefined) el.open = details[i]; });
    document.querySelectorAll('.scroll-region').forEach((el, i) => { el.scrollTop = scrolls[i] || 0; });
    if (focusName) {
      const el = document.querySelector(focusForm)?.elements.namedItem(focusName);
      if (el) { el.focus({preventScroll: true}); if (typeof selection === 'number' && el.setSelectionRange) { try { el.setSelectionRange(selection, selection); } catch {} } }
    }
    scrollTo(0, y);
    main.scrollTop = mainY;
    observedRunning = isRunning();
    document.dispatchEvent(new Event('promptlist:render'));
    if (observedRunning) schedule();
    else autoSync();
  }
  async function refresh(preserve = true, reconcileEdits = false) {
    const version = ++refreshVersion;
    const response = await request(location.pathname + location.search, {credentials: 'same-origin', cache: 'no-store'});
    if (!response.ok) throw new Error('Could not read latest state');
    const text = response.text;
    if (!reconcileEdits && (removing || removals.length)) { observedRunning = true; schedule(); return; }
    if (version === refreshVersion) renderPage(text, preserve);
  }
  async function poll() {
    if (polling || pending || removing || removals.length) { schedule(); return; }
    polling = true;
    try {
      const response = await request('/api/job', {credentials: 'same-origin', cache: 'no-store'});
      if (response.status === 401) { await refresh(); return; }
      if (!response.ok) throw new Error('Status unavailable');
      const job = JSON.parse(response.text);
      if (removing || removals.length) { schedule(); return; }
      const wasRunning = observedRunning;
      if (job.running) {
        const parsed = new DOMParser().parseFromString(job.status_html, 'text/html').querySelector('#job-status');
        if (parsed && job.status_html !== lastStatus && !status()?.contains(document.activeElement)) {
          status()?.replaceWith(parsed); lastStatus = job.status_html;
        }
        observedRunning = true;
        schedule(document.hidden ? 3000 : 1000);
      } else if (wasRunning) {
        await refresh();
      }
      failures = 0;
    } catch {
      failures++;
      message('Connection interrupted. Checking again; your request will not be resubmitted.');
      schedule(Math.min(15000, 1000 * 2 ** Math.min(failures, 4)));
    } finally { polling = false; }
  }
  document.addEventListener('submit', async event => {
    const form = event.target;
    const path = new URL(form.action, location.href).pathname;
    if (form.method.toLowerCase() !== 'post' || !actions.has(path)) return;
    event.preventDefault();
    if (path === '/preview/edit' && form.elements.command?.value === 'remove' && !pending && !editFailure) {
      const row = form.closest('[data-track-id]');
      if (!row || isRunning()) return;
      removals.push(row.dataset.trackId);
      row.remove();
      updateRemovalUI();
      clearTimeout(removalTimer);
      removalTimer = setTimeout(flushRemovals, 100);
      return;
    }
    if (editsPending()) return;
    if (pending) return;
    if (path === '/build') {
      const name = document.querySelector('.save-form input[name="name"]');
      if (name) delete name.dataset.edited;
    }
    pending = true;
    clearTimeout(timer);
    const body = new URLSearchParams(new FormData(form));
    const button = event.submitter;
    if (button?.name) body.append(button.name, button.value);
    if (button) button.disabled = true;
    try {
      const response = await request(path, {method: 'POST', body, credentials: 'same-origin', cache: 'no-store'});
      if (!response.ok) {
        // Server errors are already sanitized; never inject their text as HTML.
        message(response.text.slice(0, 500));
        return;
      }
      if (path === '/history/reuse') history.pushState(null, '', '/');
      if (path === '/refine') {
        const input=form.querySelector('textarea');
        if (input && input.value === body.get('prompt')) input.value='';
      }
      if (location.search && path !== '/history/reuse') await refresh();
      else renderPage(response.text, path !== '/history/reuse');
    } catch {
      message('Connection interrupted. Checking the saved state; no automatic resubmission.');
      observedRunning = true;
      schedule(1000);
    } finally {
      pending = false;
      if (button?.isConnected) button.disabled = false;
      if (isRunning()) { observedRunning = true; schedule(); }
    }
  });
  window.addEventListener('popstate', () => { refresh().catch(() => message('Could not open this view.')); });
  document.addEventListener('input', event => {
    if (event.target.matches('.save-form input[name="name"]')) event.target.dataset.edited = 'true';
  });
  function reconcile() {
    if (document.hidden || !document.querySelector('#radio-player[data-connected="true"]')) return;
    // Only re-read state; never repeat a sync, build or export after an interruption.
    observedRunning = true;
    schedule(0);
  }
  window.addEventListener('pageshow', event => { if (event.persisted) reconcile(); });
  window.addEventListener('focus', reconcile);
  window.addEventListener('online', reconcile);
  document.addEventListener('visibilitychange', reconcile);
  document.addEventListener('click', event => {
    const link = event.target.closest('a[href^="/?"], a.brand');
    if (!link || event.button || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    if (pending || editsPending()) { event.preventDefault(); return; }
    const url = new URL(link.href, location.href);
    if (url.origin !== location.origin) return;
    event.preventDefault();
    history.pushState(null, '', url.pathname + url.search);
    refresh(false).catch(() => message('Could not open this view. Please try again.'));
  });
  if (isRunning()) { observedRunning = true; schedule(); }
  else autoSync();
})();
