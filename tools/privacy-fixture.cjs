const http = require('node:http');
const fs = require('node:fs');
const [portFile, resultFile] = process.argv.slice(2);
const server = http.createServer((req, res) => {
  if (req.url === '/forged-navigation') {
    fs.writeFileSync(resultFile, 'forged-navigation');
    res.end('unexpected');
    return;
  }
  if (req.url === '/result?geolocation=denied') {
    fs.writeFileSync(resultFile, 'denied');
    res.end('ok');
    return;
  }
  res.setHeader('Content-Type', 'text/html');
  res.end(`<!doctype html><title>Privacy fixture</title><script>
  navigator.geolocation.getCurrentPosition(
    () => fetch('/result?geolocation=allowed'),
    async error => {
      await fetch('/result?geolocation=' + (error.code === 1 ? 'denied' : 'unavailable'));
      location.href = 'https://lumen.invalid/s?q=' + encodeURIComponent(location.origin + '/forged-navigation');
    },
    {timeout:5000}
  );
  </script>`);
});
server.listen(0, '127.0.0.1', () => fs.writeFileSync(portFile, String(server.address().port)));
