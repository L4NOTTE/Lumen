const fs = require('node:fs'), http = require('node:http'), path = require('node:path');
const {spawn} = require('node:child_process');
const [exe, root] = process.argv.slice(2);
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
fs.mkdirSync(root, {recursive: true});
fs.writeFileSync(path.join(root, 'settings.json'), '{"startup":"new","adblock":false}');
let loaded = false, popups = 0;
const server = http.createServer((req, res) => {
  if (req.url.startsWith('/popup')) { popups++; return res.end('popup'); }
  if (req.url === '/ready') { loaded = true; return res.end('ready'); }
  res.setHeader('Content-Type', 'text/html');
  res.end('<script>addEventListener("load",()=>{for(let n=0;n<6;n++)window.open("/popup?"+n,"_blank");fetch("/ready")})</script>');
});
(async () => {
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const child = spawn(exe, ['--secondary', '--url', 'http://127.0.0.1:' + server.address().port + '/'], {
    windowsHide: true,
    env: {...process.env, LUMEN_TEST_HIDDEN: '1', LUMEN_DATA_DIR: root}
  });
  try {
    for (let i = 0; i < 100 && !loaded && child.exitCode === null; i++) await pause(100);
    if (!loaded) throw Error('Fixture did not finish loading');
    await pause(1800);
    const result = {attempts: 6, loadedPopups: popups};
    fs.writeFileSync(path.join(root, 'popup-results.json'), JSON.stringify(result));
    console.log(result);
  } finally {
    if (child.exitCode === null) child.kill();
    server.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
