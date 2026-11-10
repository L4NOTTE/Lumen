import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';
const source=readFileSync(new URL('../src/shazam-capture.js',import.meta.url),'utf8');
const ui=readFileSync(new URL('../ui/ui.html',import.meta.url),'utf8');
const ids=new Set([...ui.matchAll(/\bid="([^"]+)"/g)].map(m=>m[1]));
for(const match of ui.matchAll(/\$\('([^']+)'\)/g))assert.ok(ids.has(match[1]),'UI references removed element: '+match[1]);
const commands=[],starts=[];let connections=0,closed=0;
const track={stop(){},};
class AudioDestinationNode{}
class AudioContext{
 constructor(){this.currentTime=0;}
 resume(){return Promise.resolve()}
 close(){closed++;return Promise.resolve()}
 createMediaStreamSource(){return {connect(){connections++}}}
 createMediaStreamDestination(){return {stream:{getAudioTracks:()=>[track]}}}
 createBuffer(channels,length,rate){return {duration:length/rate,copyToChannel(data){assert.equal(data.length,length)}}}
 createBufferSource(){return {connect(){},disconnect(){},start(t){starts.push(t)}}}
}
const context={location:{protocol:'chrome-extension:',hostname:'mmioliijnhnoblpgimnlajmefafdfilb'},ipc:{postMessage:m=>commands.push(JSON.parse(m))},AudioContext,AudioDestinationNode,chrome:{tabCapture:{}},addEventListener(){},WeakSet,atob,Uint8Array,Float32Array};context.window=context;
vm.runInNewContext(source,context);
assert.equal(commands.length,0,'no capture before recognition click');
let stream;context.chrome.tabCapture.capture({audio:true},s=>stream=s);
assert.ok(stream);assert.equal(commands[0].cmd,'capture_start');
const pcm=new Float32Array([0.25,-0.25,0.1,-0.1]);context.__lumenAudio(48000,Buffer.from(pcm.buffer).toString('base64'));assert.equal(starts.length,1);
const node=new context.AudioContext().createMediaStreamSource(stream);node.connect(new AudioDestinationNode());assert.equal(connections,0);node.connect({});assert.equal(connections,1);
stream.getAudioTracks()[0].stop();assert.equal(commands.at(-1).cmd,'capture_stop');assert.equal(closed,1);
context.__lumenAudio(48000,Buffer.from(pcm.buffer).toString('base64'));assert.equal(starts.length,1,'ignore packets after stop');
console.log('Native audio bridge: click gate, PCM scheduling, feedback suppression and cleanup OK');
