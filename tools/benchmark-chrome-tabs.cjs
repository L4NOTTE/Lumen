const fs = require('node:fs');
const path = require('node:path');

const [profile, action, url, readyFile, tabs] = process.argv.slice(2);
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));

async function main() {
  const [port, endpoint] = fs.readFileSync(path.join(profile, 'DevToolsActivePort'), 'utf8').trim().split(/\r?\n/);
  const socket = new WebSocket('ws://127.0.0.1:' + port + endpoint);
  await new Promise((resolve, reject) => {
    socket.onopen = resolve;
    socket.onerror = reject;
  });
  let seq = 0;
  function call(method, params) {
    const id = ++seq;
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error(method + ' timed out')), 6000);
      socket.onmessage = event => {
        const reply = JSON.parse(event.data);
        if (reply.id !== id) return;
        clearTimeout(timeout);
        if (reply.error) reject(new Error(reply.error.message));
        else resolve(reply.result);
      };
      socket.send(JSON.stringify({ id, method, params }));
    });
  }
  try {
    if (action === 'open') {
      const count = () => fs.readFileSync(readyFile, 'utf8').trim().split('\n').length;
      let expected = count();
      for (let index = 0; index < Number(tabs); index++) {
        await call('Target.createTarget', { url, background: false });
        expected++;
        const deadline = Date.now() + 6000;
        while (count() < expected && Date.now() < deadline) await pause(80);
        if (count() < expected) throw new Error('Chrome page did not finish loading');
      }
    } else if (action === 'close') {
      const { targetInfos } = await call('Target.getTargets', {});
      const open = targetInfos.filter(target => target.type === 'page' && target.url === url);
      if (open.length !== Number(tabs)) throw new Error('Unexpected Chrome tab count');
      for (const target of open) await call('Target.closeTarget', { targetId: target.targetId });
    } else {
      throw new Error('Unknown workload action');
    }
  } finally {
    socket.close();
  }
}

main().catch(error => {
  console.error(error);
  process.exitCode = 1;
});

