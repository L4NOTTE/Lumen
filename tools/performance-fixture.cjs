const http = require('node:http'), fs = require('node:fs');
const [portFile, readyFile, pages = '12'] = process.argv.slice(2);
const count = Number(pages);
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://localhost');
  const step = Number(url.searchParams.get('step'));
  if (url.pathname === '/ready') {
    fs.appendFileSync(readyFile, JSON.stringify({step, time: Date.now(), loadMs: Number(url.searchParams.get('ms'))}) + '\n');
    return res.end('ok');
  }
  if (url.pathname === '/workload') {
    res.setHeader('Content-Type', 'text/html; charset=utf-8');
    res.setHeader('Cache-Control', 'no-store');
    return res.end('<!doctype html><title>Memory fixture</title><main>'
      + '<p>Controlled local document</p>'.repeat(1500)
      + '</main><script>window.fixtureMemory = new Uint8Array(4 * 1024 * 1024);'
      + 'window.fixtureMemory.fill(91);addEventListener("load",()=>fetch("/ready?step=0&ms="+performance.now()));</script>');
  }
  if (url.pathname !== '/page' || !Number.isInteger(step) || step < 1 || step > count) {
    res.writeHead(404);
    return res.end();
  }
  res.setHeader('Content-Type', 'text/html; charset=utf-8');
  res.setHeader('Cache-Control', 'no-store');
  res.end('<!doctype html><meta charset=utf-8><title>Lumen performance fixture</title>'
    + '<main>' + '<p>Локальный тест загрузки страницы.</p>'.repeat(1500) + '</main>'
    + '<script>addEventListener("load",async()=>{const ms=performance.now();'
    + 'await fetch("/ready?step=' + step + '&ms="+ms);'
    + (step < count ? 'location.replace("/page?step=' + (step + 1) + '");' : '')
    + '});</script>');
});
server.listen(0, '127.0.0.1', () => fs.writeFileSync(portFile, String(server.address().port)));
