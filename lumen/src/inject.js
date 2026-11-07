(() => {
  if (window.__lumen) return;
  window.__lumen = 1;
  const H = new Set(__HOSTS__);
  const CSS = __CSS__;

  const bad = (u) => {
    try {
      let h = new URL(u, location.href).hostname;
      while (h) {
        if (H.has(h)) return true;
        const i = h.indexOf('.');
        if (i < 0) break;
        h = h.slice(i + 1);
      }
    } catch (_) {}
    return false;
  };

  // fetch / XHR / sendBeacon
  const f = window.fetch;
  window.fetch = function (i) {
    const u = typeof i === 'string' ? i : i && i.url;
    return bad(u) ? Promise.reject(new TypeError('blocked')) : f.apply(this, arguments);
  };
  const xo = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function (m, u) {
    const rest = Array.prototype.slice.call(arguments, 2);
    return xo.apply(this, [m, bad(u) ? 'data:,' : u].concat(rest));
  };
  const sb = navigator.sendBeacon;
  if (sb) navigator.sendBeacon = function (u, d) { return bad(u) ? true : sb.call(navigator, u, d); };

  // динамически создаваемые script / iframe / img
  for (const C of [HTMLScriptElement, HTMLIFrameElement, HTMLImageElement]) {
    const d = Object.getOwnPropertyDescriptor(C.prototype, 'src');
    if (!d || !d.set) continue;
    Object.defineProperty(C.prototype, 'src', {
      ...d,
      set(v) { d.set.call(this, bad(v) ? 'about:blank' : v); },
    });
  }
  const sa = Element.prototype.setAttribute;
  Element.prototype.setAttribute = function (n, v) {
    if (n === 'src' && bad(v)) v = 'about:blank';
    return sa.call(this, n, v);
  };

  // удаляем уже вставленные элементы
  const TAGS = ['SCRIPT', 'IFRAME', 'IMG', 'LINK'];
  const kill = (n) => {
    if (n.nodeType === 1 && TAGS.includes(n.tagName)) {
      const u = n.src || n.href;
      if (u && bad(u)) n.remove();
    }
  };
  new MutationObserver((ms) => ms.forEach((r) => r.addedNodes.forEach(kill)))
    .observe(document, { childList: true, subtree: true });

  // косметика
  const st = document.createElement('style');
  st.textContent = CSS;
  (document.head || document.documentElement).appendChild(st);
})();
