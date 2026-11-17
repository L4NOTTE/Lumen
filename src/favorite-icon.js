(async nonce => {
  const origin = location.origin;
  if (!['https:', 'http:'].includes(location.protocol)) return;
  const candidates = [...document.querySelectorAll('link[rel][href]')].slice(0, 48).flatMap(link => {
    const rel = link.rel.toLowerCase().split(/\s+/);
    const touch = rel.some(name => name === 'apple-touch-icon' || name === 'apple-touch-icon-precomposed');
    if (!touch && !rel.includes('icon')) return [];
    if (!URL.canParse(link.href)) return [];
    const address = new URL(link.href);
    if (address.origin !== origin && address.protocol !== 'https:') return [];
    if (!['https:', 'http:'].includes(address.protocol)) return [];
    const dimensions = (link.sizes.value.match(/\d+/g) || []).map(Number);
    const vector = link.type === 'image/svg+xml' || address.pathname.toLowerCase().endsWith('.svg');
    return [{address: address.href, score: vector ? 512 : Math.max(touch ? 128 : 0, ...dimensions)}];
  }).sort((a, b) => b.score - a.score);

  const MAX_BYTES = 192 * 1024;
  for (const {address} of candidates.slice(0, 3)) {
    const deadline = new AbortController();
    const timer = setTimeout(() => deadline.abort(), 2500);
    try {
      const response = await fetch(address, {credentials:'omit', redirect:'error', referrerPolicy:'no-referrer', signal:deadline.signal});
      const mime = (response.headers.get('content-type') || '').split(';', 1)[0].trim();
      if (!response.ok || !response.body || !/^image\/(png|svg\+xml|webp|jpeg|x-icon|vnd\.microsoft\.icon)$/.test(mime) ||
          Number(response.headers.get('content-length')) > MAX_BYTES) continue;
      const reader = response.body.getReader();
      const chunks = [];
      let size = 0;
      for (;;) {
        const {done, value} = await reader.read();
        if (done) break;
        size += value.byteLength;
        if (size > MAX_BYTES) break;
        chunks.push(value);
      }
      if (size > MAX_BYTES) { await reader.cancel(); continue; }
      const objectUrl = URL.createObjectURL(new Blob(chunks, {type:mime}));
      try {
        const image = new Image();
        image.src = objectUrl;
        await image.decode();
        if (image.naturalWidth < 32 || image.naturalHeight < 32 || Math.max(image.naturalWidth, image.naturalHeight) > 2048) continue;
        const scale = Math.min(1, 64 / Math.max(image.naturalWidth, image.naturalHeight));
        const canvas = document.createElement('canvas');
        canvas.width = Math.round(image.naturalWidth * scale);
        canvas.height = Math.round(image.naturalHeight * scale);
        const painter = canvas.getContext('2d');
        if (!painter) return;
        painter.imageSmoothingQuality = 'high';
        painter.drawImage(image, 0, 0, canvas.width, canvas.height);
        window.chrome.webview.postMessage('lumen-favorite-icon:' + nonce + ':' + canvas.toDataURL('image/png'));
        return;
      } finally { URL.revokeObjectURL(objectUrl); }
    } catch (_) {
      // A page may supply an incompatible icon or a CSP that disallows loading it.
    } finally { clearTimeout(timer); }
  }
})
