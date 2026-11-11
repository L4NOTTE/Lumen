// Косметический движок Lumen: процедурные правила фильтров и скрытие карточек с пометкой «Реклама».
(() => {
  if (window.__lumenRt) return;
  window.__lumenRt = 1;
  window.__lumenCfg = window.__lumenCfg || { labels: true };
  let rules = [];
  let timer = 0, last = 0, fullScan = true;
  const dirty = new Set();
  // Keep access to publisher-created closed roots without exposing them to pages.
  const shadowRoots = new Set();
  const attachShadow = Element.prototype.attachShadow;
  const yandexHost = /(^|\.)(yandex\.(ru|com|by|kz|uz|com\.tr)|ya\.ru)$/.test(location.hostname);
  if (yandexHost) Element.prototype.attachShadow = function(options) {
    const root = attachShadow.call(this, options);
    shadowRoots.add(new WeakRef(root));
    new MutationObserver(() => schedule()).observe(root, {childList:true, subtree:true, characterData:true, attributes:true, attributeFilter:['href','aria-label','title','src']});
    schedule();
    return root;
  };

  const hide = (el) => {
    if (el && el.style && el.style.getPropertyValue('display') !== 'none') el.style.setProperty('display', 'none', 'important');
  };

  // ---------- процедурные правила (:has-text, :upward, :matches-css …) ----------
  const matcher = (arg) => {
    const m = /^\/(.+)\/([a-z]*)$/.exec(arg);
    if (m) { try { const re = new RegExp(m[1], m[2]); return (s) => re.test(s); } catch (_) {} }
    return (s) => s.includes(arg);
  };
  const globRe = (g) => new RegExp('^' + g.replace(/[.+?^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*') + '$');
  const cssMatch = (el, arg, pseudo) => {
    const i = arg.indexOf(':');
    if (i < 0) return false;
    const name = arg.slice(0, i).trim(), want = arg.slice(i + 1).trim();
    let val = '';
    try { val = getComputedStyle(el, pseudo || null).getPropertyValue(name); } catch (_) {}
    const m = /^\/(.+)\/([a-z]*)$/.exec(want);
    if (m) { try { return new RegExp(m[1], m[2]).test(val); } catch (_) { return false; } }
    return globRe(want.replace(/^"(.*)"$/, '$1')).test(val);
  };
  const evalSel = (ops) => {
    let cur = null;
    for (const op of ops) {
      if (op.type === 'css-selector') {
        if (cur === null) { try { cur = [...document.querySelectorAll(op.arg)]; } catch (_) { return []; } }
        else {
          const out = [], sel = /^\s*[>+~]/.test(op.arg) ? ':scope ' + op.arg : op.arg;
          for (const e of cur) { try { out.push(...e.querySelectorAll(sel)); } catch (_) {} }
          cur = out;
        }
      } else if (cur === null) { return []; }
      else if (op.type === 'has-text') { const f = matcher(op.arg); cur = cur.filter((e) => f(e.textContent || '')); }
      else if (op.type === 'min-text-length') { const n = +op.arg || 0; cur = cur.filter((e) => (e.textContent || '').length >= n); }
      else if (op.type === 'upward') {
        cur = cur.map((e) => {
          if (/^\d+$/.test(op.arg)) { let x = e; for (let i = 0; i < +op.arg && x; i++) x = x.parentElement; return x; }
          return e.parentElement ? e.parentElement.closest(op.arg) : null;
        }).filter(Boolean);
      }
      else if (op.type === 'matches-css') cur = cur.filter((e) => cssMatch(e, op.arg, null));
      else if (op.type === 'matches-css-before') cur = cur.filter((e) => cssMatch(e, op.arg, '::before'));
      else if (op.type === 'matches-css-after') cur = cur.filter((e) => cssMatch(e, op.arg, '::after'));
      else if (op.type === 'matches-path') { const f = matcher(op.arg); if (!f(location.pathname + location.search)) cur = []; }
      else if (op.type === 'matches-attr') {
        const m = /^"?([^"=]+)"?(?:="?([^"]*)"?)?$/.exec(op.arg);
        if (m) {
          const nre = globRe(m[1]), vre = m[2] === undefined ? null : globRe(m[2]);
          cur = cur.filter((e) => [...e.attributes].some((a) => nre.test(a.name) && (!vre || vre.test(a.value))));
        }
      }
      else if (op.type === 'xpath') {
        const out = [];
        for (const e of cur) {
          try {
            const r = document.evaluate(op.arg, e, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null);
            for (let i = 0; i < r.snapshotLength; i++) if (r.snapshotItem(i).nodeType === 1) out.push(r.snapshotItem(i));
          } catch (_) {}
        }
        cur = out;
      }
      if (!cur.length) return [];
    }
    return cur || [];
  };
  const act = (el, a) => {
    if (!a) return hide(el);
    if (a.type === 'remove') el.remove();
    else if (a.type === 'style') el.style.cssText += ';' + a.arg;
    else if (a.type === 'remove-attr') el.removeAttribute(a.arg);
    else if (a.type === 'remove-class') el.classList.remove(a.arg);
    else hide(el);
  };
  const runRules = () => {
    for (const r of rules) { try { for (const el of evalSel(r.selector)) act(el, r.action); } catch (_) {} }
  };

  // ---------- карточки с пометкой «Реклама» ----------
  const LABEL = /^(?:соц)?реклама(?:\s*[·•|.:,\-–—]?\s*\d{1,2}\+)?$|^erid:?\s*\S+$|^на правах рекламы$|^партн[её]рский материал$|^advertisement$|^sponsored$|^promoted$/i;
  const isLabel = (s) => s && s.length < 40 && LABEL.test(s.trim());
  const labelsIn = (root, stopAt2) => {
    let n = 0;
    const tw = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    for (let t; (t = tw.nextNode());) { if (isLabel(t.nodeValue) && ++n >= stopAt2) break; }
    return n;
  };
  const cardOf = (el) => {
    let best = null, cur = el;
    for (let i = 0; i < 10 && cur && cur !== document.body && cur !== document.documentElement; i++) {
      const r = cur.getBoundingClientRect();
      if (r.width > 760 || r.height > 760) break;          // слишком большой контейнер
      if (labelsIn(cur, 2) > 1) break;                     // внутри несколько реклам — это уже контейнер
      best = cur;
      const p = cur.parentElement;
      if (!p) break;
      // родитель содержит много постороннего текста — это не карточка, а часть страницы
      if ((p.textContent || '').length > (cur.textContent || '').length + 220) break;
      cur = p;
    }
    return best;
  };
  const scanLabels = (root = document.body) => {
    if (!document.body) return;
    const found = [];
    const tw = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    for (let t; (t = tw.nextNode());) { if (isLabel(t.nodeValue) && t.parentElement) found.push(t.parentElement); }
    for (const el of found) {
      if (el.closest('nav,header,footer,[role=navigation],[role=menubar]')) continue; // пункты меню сайта
      const c = cardOf(el);
      if (c) hide(c);
    }
  };

  // Yandex cards can split the advertising label across spans and load the
  // creative after the label. Select the card, never the whole forecast/feed.
  const queryWithin = (root, selector) => [...(root.matches?.(selector) ? [root] : []), ...root.querySelectorAll(selector)];
  const scanYandex = (scope = document) => {
    if (!yandexHost) return;
    for (const ref of shadowRoots) {
      const root = ref.deref();
      if (!root) { shadowRoots.delete(ref); continue; }
      if (!root.host.isConnected || root.__lumenAdHidden) continue;
      const label = [...root.querySelectorAll('span,small,[aria-label],[title]')].some(el =>
        isLabel(el.getAttribute('aria-label')) || isLabel(el.getAttribute('title')) ||
        (el.children.length < 4 && isLabel((el.textContent || '').replace(/\s+/g,' ').trim())));
      if (label && root.querySelector('a[href]') && root.querySelector('img,video,iframe')) {
        // Shadow :host {all:initial!important} can override outer page styles.
        const sheet = new CSSStyleSheet(); sheet.replaceSync(':host{display:none!important}');
        root.adoptedStyleSheets = [...root.adoptedStyleSheets, sheet];
        root.__lumenAdHidden = true; hide(root.host);
      }
    }
    queryWithin(scope, '[id^="yandex_rtb"],[id^="yandex_ad"],[id*="adfox"],[class*="adfox"],.ya-partner__ads').forEach(hide);
    const seeds = new Set();
    queryWithin(scope, 'a[href], [aria-label], [title], span, small').forEach(el => {
      const href=el.getAttribute('href')||'';
      const text=(el.textContent||'').replace(/\s+/g,' ').trim();
      if (/^https?:\/\/(?:yabs\.yandex\.[^/]+|an\.yandex\.[^/]+)\//i.test(href) ||
          isLabel(el.getAttribute('aria-label')) || isLabel(el.getAttribute('title')) ||
          (el.children.length < 4 && isLabel(text))) seeds.add(el);
    });
    for (const seed of seeds) {
      let best=null;
      for(let el=seed,depth=0;el&&el!==document.body&&depth<12;el=el.parentElement,depth++){
        if(el.matches('main,header,nav,footer,[role="main"]'))break;
        if(best && el.querySelectorAll('img,video,iframe').length>best.querySelectorAll('img,video,iframe').length)break;
        const rect=el.getBoundingClientRect();
        if(rect.width>650||rect.height>1000)break;
        if(labelsIn(el,2)>1)break;
        if(rect.width>=140&&rect.height>=80&&el.querySelector('img,video,iframe')&&el.querySelector('a[href]'))best=el;
      }
      if(best)hide(best);
    }
  };

  // ---------- incremental cosmetic scans ----------
  const changed = records => {
    if (!fullScan) for (const record of records) {
      let el = record.target.nodeType === 1 ? record.target : record.target.parentElement;
      if (!el) continue;
      if (el.matches('script,style,link,meta')) continue;
      // Include the immediate surroundings: a label/media may complete a card.
      el = el.parentElement || el;
      dirty.add(el);
      if (dirty.size > 64) { fullScan = true; dirty.clear(); break; }
    }
    schedule();
  };
  const run = () => {
    timer = 0; last = Date.now();
    if (!document.body) return;
    const scopes = fullScan ? [document.body] : [...dirty].filter(el => el.isConnected);
    fullScan = false; dirty.clear();
    // Relational procedural selectors still need a document-wide evaluation.
    if (rules.length) runRules();
    if (window.__lumenCfg.labels) {
      const roots = scopes.filter(el => !scopes.some(other => other !== el && other.contains(el)));
      for (const root of roots) { try { scanYandex(root); scanLabels(root); } catch (_) {} }
      // Closed roots are not descendants visible to page queries.
      if (!roots.length && yandexHost) { try { scanYandex(document.createDocumentFragment()); } catch (_) {} }
    }
  };
  const schedule = () => {
    if (timer) return;
    timer = setTimeout(run, Math.max(300, 1000 - (Date.now() - last)));
  };
  window.__lumenSetRules = (arr) => { rules = Array.isArray(arr) ? arr : []; fullScan = true; schedule(); };
  window.__lumenSetLabels = enabled => { window.__lumenCfg.labels = !!enabled; fullScan = true; schedule(); };
  new MutationObserver(changed).observe(document, { childList:true, subtree:true, characterData:true, attributes:true, attributeFilter:['href','aria-label','title','src'] });
  document.addEventListener('DOMContentLoaded', () => { fullScan = true; schedule(); });
  schedule();
})();
