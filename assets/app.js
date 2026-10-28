// Progressive transport only. Rust owns rendering, authorization and playlist decisions.
(() => {
  'use strict';
  const actions = new Set(['/build', '/sync', '/coverage', '/pause', '/publish',
    '/preview/edit', '/preview/abandon', '/history/reuse']);
  let timer, pending = false, polling = false, observedRunning = false, failures = 0, lastStatus = '';
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
    const details = [...document.querySelectorAll('details')].map(el => el.open);
    const scrolls = [...document.querySelectorAll('.scroll-region')].map(el => el.scrollTop);
    const focused = document.activeElement;
    const focusName = focused?.closest('.composer-form') ? focused.name : null;
    const selection = focused?.selectionStart;
    const y = scrollY;
    document.querySelector('main').replaceWith(main);
    for (const selector of ['body > header', 'body > footer']) {
      const replacement = next.querySelector(selector);
      if (replacement) document.querySelector(selector)?.replaceWith(replacement);
    }
    document.title = next.title;
    for (const [name, value, checked] of values) {
      const el = document.querySelector('.composer-form')?.elements.namedItem(name);
      if (el) { el.value = value; if (el.type === 'checkbox') el.checked = checked; }
    }
    document.querySelectorAll('details').forEach((el, i) => { if (details[i] !== undefined) el.open = details[i]; });
    document.querySelectorAll('.scroll-region').forEach((el, i) => { el.scrollTop = scrolls[i] || 0; });
    if (focusName) {
      const el = document.querySelector('.composer-form')?.elements.namedItem(focusName);
      if (el) { el.focus({preventScroll: true}); if (typeof selection === 'number' && el.setSelectionRange) { try { el.setSelectionRange(selection, selection); } catch {} } }
    }
    scrollTo(0, y);
    observedRunning = isRunning();
    if (observedRunning) schedule();
  }
  async function refresh(preserve = true) {
    const response = await fetch(location.pathname + location.search, {credentials: 'same-origin', cache: 'no-store'});
    if (!response.ok) throw new Error('Could not read latest state');
    renderPage(await response.text(), preserve);
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
  if (isRunning()) { observedRunning = true; schedule(); }
})();
