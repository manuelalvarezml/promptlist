// Progressive transport only. Rust owns rendering, authorization and playlist decisions.
(() => {
  'use strict';
  const actions = new Set(['/build', '/refine', '/sync', '/coverage', '/pause', '/publish',
    '/preview/edit', '/preview/abandon', '/history/reuse']);
  let timer, pending = false, polling = false, observedRunning = false, failures = 0, lastStatus = '';
  let refreshVersion = 0;
  let autoSyncAttempted = false;
  function autoSync() {
    const form = document.querySelector('#auto-sync');
    if (form && !autoSyncAttempted && !pending && !isRunning()) {
      autoSyncAttempted = true;
      form.requestSubmit();
    }
  }
  const status = () => document.querySelector('#job-status');
  const isRunning = () => status()?.dataset.running === 'true';
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
    const details = [...document.querySelectorAll('details')].map(el => el.open);
    const scrolls = [...document.querySelectorAll('.scroll-region')].map(el => el.scrollTop);
    const focused = document.activeElement;
    const focusForm = focused?.closest('.refinement-form') ? '.refinement-form' : '.composer-form';
    const focusName = focused?.closest(focusForm) ? focused.name : null;
    const selection = focused?.selectionStart;
    const y = scrollY;
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
    document.querySelectorAll('details').forEach((el, i) => { if (details[i] !== undefined) el.open = details[i]; });
    document.querySelectorAll('.scroll-region').forEach((el, i) => { el.scrollTop = scrolls[i] || 0; });
    if (focusName) {
      const el = document.querySelector(focusForm)?.elements.namedItem(focusName);
      if (el) { el.focus({preventScroll: true}); if (typeof selection === 'number' && el.setSelectionRange) { try { el.setSelectionRange(selection, selection); } catch {} } }
    }
    scrollTo(0, y);
    observedRunning = isRunning();
    document.dispatchEvent(new Event('promptlist:render'));
    if (observedRunning) schedule();
    else autoSync();
  }
  async function refresh(preserve = true) {
    const version = ++refreshVersion;
    const response = await fetch(location.pathname + location.search, {credentials: 'same-origin', cache: 'no-store'});
    if (!response.ok) throw new Error('Could not read latest state');
    const text = await response.text();
    if (version === refreshVersion) renderPage(text, preserve);
  }
  async function poll() {
    if (polling || pending) { schedule(); return; }
    polling = true;
    try {
      const response = await fetch('/api/job', {credentials: 'same-origin', cache: 'no-store'});
      if (response.status === 401) { await refresh(); return; }
      if (!response.ok) throw new Error('Status unavailable');
      const job = await response.json();
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
    if (pending) return;
    pending = true;
    clearTimeout(timer);
    const body = new URLSearchParams(new FormData(form));
    const button = event.submitter;
    if (button?.name) body.append(button.name, button.value);
    if (button) button.disabled = true;
    try {
      const response = await fetch(path, {method: 'POST', body, credentials: 'same-origin', cache: 'no-store'});
      if (!response.ok) {
        // Server errors are already sanitized; never inject their text as HTML.
        message((await response.text()).slice(0, 500));
        return;
      }
      if (path === '/history/reuse') history.pushState(null, '', '/');
      if (path === '/refine') {
        const input=form.querySelector('textarea');
        if (input && input.value === body.get('prompt')) input.value='';
      }
      if (location.search && path !== '/history/reuse') await refresh();
      else renderPage(await response.text(), path !== '/history/reuse');
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
  document.addEventListener('click', event => {
    const link = event.target.closest('a[href^="/?"], a.brand');
    if (!link || event.button || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || pending) return;
    const url = new URL(link.href, location.href);
    if (url.origin !== location.origin) return;
    event.preventDefault();
    history.pushState(null, '', url.pathname + url.search);
    refresh(false).catch(() => message('Could not open this view. Please try again.'));
  });
  if (isRunning()) { observedRunning = true; schedule(); }
  else autoSync();
})();
