const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');

const html = `<!doctype html><title>WebView2 rendering fixture</title>
<style>body{margin:0;background:#222}canvas,video{width:45vw;height:42vh} i{position:absolute;background:#947cef;width:24px;height:24px;opacity:.5}</style>
<canvas id="draw" width="1024" height="576"></canvas>
<canvas id="gl" width="1024" height="576"></canvas>
<video id="output" autoplay muted playsinline></video>
<script>
const draw = document.getElementById('draw');
const paint = draw.getContext('2d');
const gpu = document.getElementById('gl').getContext('webgl', {failIfMajorPerformanceCaveat:true});
const video = document.getElementById('output');
video.srcObject = draw.captureStream(30);
video.play().catch(() => {});
const layers = Array.from({length:160}, () => document.body.appendChild(document.createElement('i')));
const intervals = [];
let previous, frame = 0, hidden = false;
function render(now) {
  if (previous !== undefined && frame > 30) {
    hidden ||= document.hidden;
    intervals.push(now - previous);
  }
  previous = now;
  frame++;
  paint.fillStyle = '#203047';
  paint.fillRect(0, 0, 1024, 576);
  for (let j = 0; j < 250; j++) {
    paint.fillStyle = 'hsl(' + (j * 7) % 360 + ' 70% 60%)';
    paint.fillRect((j * 19 + frame * 6) % 1024, (j * 7) % 576, 14, 14);
  }
  if (gpu) {
    gpu.clearColor((frame % 255) / 255, 0.25, 0.5, 1);
    gpu.clear(gpu.COLOR_BUFFER_BIT);
  }
  for (let j = 0; j < layers.length; j++) {
    layers[j].style.transform = 'translate3d(' + ((j * 23 + frame * 3) % 800) + 'px,' + (j * 31 % 450) + 'px,0)';
  }
  if (intervals.length < 180) requestAnimationFrame(render);
  else {
    video.srcObject.getTracks().forEach(track => track.stop());
    intervals.sort((a,b) => a-b);
    fetch('/done', {method:'POST', body:JSON.stringify({
      acceleratedWebGL: !!gpu, videoFrames: video.getVideoPlaybackQuality().totalVideoFrames,
      hidden, p95FrameMs: intervals[Math.ceil(intervals.length * .95) - 1],
      medianFrameMs: intervals[Math.floor(intervals.length / 2)]
    })});
  }
}
requestAnimationFrame(render);
</script>`;

const historyPages = {
  '/start': `<!doctype html><title>History fixture A</title>
<script>
addEventListener('pageshow', event => {
  if (sessionStorage.getItem('visited-a')) {
    fetch('/done', {method:'POST', body:JSON.stringify({restoredFromCache:event.persisted})});
  } else {
    sessionStorage.setItem('visited-a', 'yes');
    setTimeout(() => location.assign('/second'), 150);
  }
});
</script>`,
  '/second': `<!doctype html><title>History fixture B</title>
<script>addEventListener('pageshow', () => setTimeout(() => history.back(), 150));</script>`,
};

async function run(exe, variant, history) {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'lumen-webview-rendering-'));
  const settings = {adblock: false, startup: 'new'};
  if (['standard', 'balanced', 'economy'].includes(variant)) settings.engine_memory = variant;
  fs.writeFileSync(path.join(profile, 'settings.json'), JSON.stringify(settings));
  let completed;
  const done = new Promise(resolve => { completed = resolve; });
  const server = http.createServer((req, res) => {
    if (req.url === '/done' && req.method === 'POST') {
      let payload = '';
      req.on('data', chunk => {
        payload += chunk;
        if (payload.length > 1024) req.destroy();
      });
      req.on('end', () => {
        res.end();
        completed(JSON.parse(payload));
      });
      return;
    }
    res.writeHead(200, {'Content-Type':'text/html; charset=utf-8'});
    res.end(history ? historyPages[req.url] : html);
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const env = {...process.env, LUMEN_DATA_DIR: profile, LUMEN_WEBVIEW2_STUDY: variant};
  delete env.LUMEN_TEST_HIDDEN;
  const start = 'http://127.0.0.1:' + server.address().port + (history ? '/start' : '/');
  const app = spawn(exe, ['--secondary', '--url', start],
    {env, stdio:'ignore'});
  let timer;
  try {
    const outcome = await Promise.race([
      done,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error('No rendering report for ' + variant)), 18000);
        app.on('error', reject);
        app.on('exit', code => reject(new Error('Browser exited early: ' + code)));
      }),
    ]);
    if (!history) {
      assert(!outcome.hidden, 'Rendering page was backgrounded during the sample');
      assert(outcome.videoFrames > 20, 'The playing video did not deliver enough frames');
    }
    return {variant, ...outcome};
  } finally {
    clearTimeout(timer);
    app.kill();
    server.closeAllConnections();
    server.close();
    await new Promise(resolve => setTimeout(resolve, 350));
  }
}

(async () => {
  const [exe, ...variants] = process.argv.slice(2);
  const history = variants.includes('--history');
  const results = [];
  for (const variant of variants.filter(variant => variant !== '--history')) {
    results.push(await run(path.resolve(exe), variant, history));
  }
  console.log(JSON.stringify(results, null, 2));
})().catch(error => { console.error(error); process.exitCode = 1; });

