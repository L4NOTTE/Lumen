const assert = require('node:assert/strict');
const { execFileSync, spawn } = require('node:child_process');
const fs = require('node:fs');
const http = require('node:http');
const http2 = require('node:http2');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const { createPrivateKey, randomUUID } = require('node:crypto');

const exe = path.resolve(process.argv[2]);
const xray = path.join(path.dirname(exe), 'xray.exe');
const transport = process.argv.includes('--xhttp') ? 'xhttp' : 'raw';
const powershell = path.join(process.env.SystemRoot, 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe');
const curl = path.join(process.env.SystemRoot, 'System32', 'curl.exe');
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'lumen-vpn-'));
const listen = server => new Promise(resolve => server.listen(0, '127.0.0.1', () => resolve(server.address().port)));
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));

function ps(script, input) {
  return execFileSync(powershell, ['-NoProfile', '-NonInteractive', '-Command', script],
    { encoding: 'utf8', input, windowsHide: true, timeout: 15000 }).trim();
}

async function run() {
  const serverArgs = { encoding: 'utf8', windowsHide: true, timeout: 5000 };
  const keys = execFileSync(xray, ['x25519'], serverArgs);
  const key = name => keys.match(new RegExp('^' + name + ': (.+)$', 'm'))?.[1].trim();
  const privateKey = key('PrivateKey');
  const publicKey = key('Password \\(PublicKey\\)');
  assert(privateKey && publicKey);
  const name = 'lumen-vpn-fixture.test';
  const cert = JSON.parse(execFileSync(xray, ['tls', 'cert', '-json', '-domain', name], serverArgs));
  const target = http2.createSecureServer({
    key: createPrivateKey({ key: Buffer.from(cert.key.slice(1, -1).join(''), 'base64'), format: 'der', type: 'pkcs8' })
      .export({ format: 'pem', type: 'pkcs8' }),
    cert: cert.certificate.join('\n') + '\n',
    allowHTTP1: true,
    minVersion: 'TLSv1.3',
  }, (_, res) => res.end('TLS fixture'));
  const sockets = new Set();
  target.on('connection', socket => {
    sockets.add(socket);
    socket.on('close', () => sockets.delete(socket));
  });
  const targetPort = await listen(target);
  let requests = 0;
  const origin = http.createServer((req, res) => {
    if (req.url === '/probe') requests++;
    res.end('through REALITY');
  });
  const originPort = await listen(origin);
  const reserve = net.createServer();
  const port = await listen(reserve);
  await new Promise(resolve => reserve.close(resolve));
  const uuid = randomUUID();
  const shortId = 'd0e1f2a3';
  const server = spawn(xray, ['run', '-format', 'json', '-config', 'stdin:'],
    { windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
  // Keep fixture logs in memory. Only print them if the local test fails.
  let serverLog = '';
  const remember = chunk => { if (serverLog.length < 4096) serverLog += chunk.toString(); };
  server.stdout.on('data', remember);
  server.stderr.on('data', remember);
  let browser;
  try {
    server.stdin.end(JSON.stringify({
      log: { loglevel: 'warning' },
      inbounds: [{
        listen: '127.0.0.1', port, protocol: 'vless', settings: { clients: [{ id: uuid }], decryption: 'none' },
        streamSettings: {
          network: transport, security: 'reality',
          ...(transport === 'xhttp' && { xhttpSettings: { path: '/fixture', mode: 'stream-one' } }),
          realitySettings: { target: '127.0.0.1:' + targetPort, serverNames: [name], privateKey, shortIds: [shortId] },
        },
      }],
      outbounds: [{
        protocol: 'freedom',
        settings: {
          redirect: '127.0.0.1:' + originPort,
          finalRules: [{ action: 'allow', network: 'tcp', ip: ['127.0.0.1/32'], port: String(originPort) }],
        },
      }],
    }));
    const link = 'vless://' + uuid + '@127.0.0.1:' + port +
      '?type=' + transport + '&security=reality&sni=' + name + '&pbk=' + publicKey + '&sid=' + shortId + '&fp=chrome' +
      (transport === 'xhttp' ? '&path=%2Ffixture&mode=stream-one' : '');
    const encrypted = ps(
      'Add-Type -AssemblyName System.Security; $bytes = [Convert]::FromBase64String([Console]::In.ReadToEnd());' +
      '[Convert]::ToBase64String([Security.Cryptography.ProtectedData]::Protect($bytes, $null, "CurrentUser"))',
      Buffer.from(link).toString('base64'));
    fs.writeFileSync(path.join(profile, 'vpn.dpapi'), Buffer.from(encrypted, 'base64'));
    fs.writeFileSync(path.join(profile, 'settings.json'), '{"adblock":false,"startup":"new"}');
    browser = spawn(exe, ['--secondary'], {
      env: { ...process.env, LUMEN_DATA_DIR: profile, LUMEN_TEST_HIDDEN: '1' },
      windowsHide: true, stdio: 'ignore',
    });
    let webviewPort;
    let clientId;
    for (let attempt = 0; attempt < 10 && (!webviewPort || !clientId); attempt++) {
      assert.equal(browser.exitCode, null, 'Lumen exited before creating WebView2');
      const snapshot = ps(`$all = Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe' OR Name='xray.exe'";` +
        '@($all | Where-Object ParentProcessId -eq ' + browser.pid +
        ' | Select-Object ProcessId,Name,CommandLine) | ConvertTo-Json -Compress');
      const children = snapshot ? [JSON.parse(snapshot)].flat() : [];
      const command = children.find(p => p.Name === 'msedgewebview2.exe' &&
        p.CommandLine.includes('--type=') === false)?.CommandLine ?? '';
      webviewPort = command.match(/--proxy-server=socks5:\/\/127\.0\.0\.1:(\d+)/)?.[1];
      if (webviewPort) {
        assert(command.includes('--disable-quic'));
        assert(command.includes('--host-resolver-rules'));
        assert(command.includes('<-loopback>'));
      }
      clientId = children.find(p => p.Name === 'xray.exe' && p.CommandLine.includes('-config stdin'))?.ProcessId;
      await pause(200);
    }
    assert(webviewPort && clientId, 'WebView2 proxy flags or attached Xray process missing');

    const args = ['-q', '-fsS', '--max-time', '4', '--noproxy', '127.0.0.1',
      '--proxy', 'socks5h://127.0.0.1:' + webviewPort, 'http://lumen-vpn-origin.test/probe'];
    // Use async I/O so the fixture can accept the TLS handshake and HTTP request.
    function request() {
      return new Promise(resolve => {
        const client = spawn(curl, args, { windowsHide: true });
        let output = '';
        client.stdout.on('data', chunk => { output += chunk; });
        client.on('exit', code => resolve({ code, output }));
      });
    }
    assert.deepEqual(await request(), { code: 0, output: 'through REALITY' });
    assert.equal(requests, 1);
    process.kill(clientId);
    assert.notEqual((await request()).code, 0);
    assert.equal(requests, 1);
    console.log(JSON.stringify({ transport, xrayRealityTunnel: true, webViewProxyFlags: true, afterCoreExit: 'no fixture request' }, null, 2));
  } catch (error) {
    console.error(serverLog.trim());
    throw error;
  } finally {
    if (browser?.exitCode === null) browser.kill();
    server.kill();
    for (const socket of sockets) socket.destroy();
    origin.closeAllConnections();
    target.close();
    origin.close();
  }
}

run().catch(error => { console.error(error); process.exitCode = 1; });

