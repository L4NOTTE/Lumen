const fs = require('node:fs');
const crypto = require('node:crypto');
const path = require('node:path');

const [version, windows, linux, output] = process.argv.slice(2);
if (!/^\d+\.\d+\.\d+$/.test(version ?? '') || !windows || !linux || !output || (windows === '-' && linux === '-')) {
  throw Error('Usage: node tools/sign-update.cjs VERSION WINDOWS_EXE|- LINUX_BINARY|- OUTPUT_JSON');
}
const secret = (process.env.LUMEN_UPDATE_PRIVATE_KEY_BASE64 || fs.readFileSync(0, 'utf8')).trim();
const key = crypto.createPrivateKey({key: Buffer.from(secret, 'base64'), format: 'der', type: 'pkcs8'});
const trusted = fs.readFileSync(path.join(__dirname, '../assets/update-public.bin'));
const publicKey = crypto.createPublicKey(key).export({format:'der', type:'spki'}).subarray(-32);
if (key.asymmetricKeyType !== 'ed25519' || !publicKey.equals(trusted)) throw Error('Wrong Lumen update signing key');

function asset(file) {
  const size = fs.statSync(file).size;
  if (!size || size > 64 * 1024 * 1024) throw Error('Update outside size limit');
  return { size, sha256: crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex') };
}
const manifest = {version, expires: Math.floor(Date.now()/1000) + 30*86400};
if (windows !== '-') manifest.windows_x64 = asset(windows);
if (linux !== '-') manifest.linux_x64 = asset(linux);
const payload = Buffer.from(JSON.stringify(manifest));
const signature = crypto.sign(null, payload, key);
fs.writeFileSync(output, JSON.stringify({payload:payload.toString('base64'), signature:signature.toString('base64')}));
