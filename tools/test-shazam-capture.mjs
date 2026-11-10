import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';
const source = readFileSync(new URL('../src/shazam-capture.js', import.meta.url), 'utf8');
function environment({audio=true,host='mmioliijnhnoblpgimnlajmefafdfilb'}={}) {
  const elements=[];let requests=0,connections=0,videoStops=0,audioStops=0;
  class Element {constructor(tag){this.tag=tag;this.children=[];this.style={};elements.push(this);}append(...n){this.children.push(...n)}prepend(...n){this.children.unshift(...n)}replaceChildren(){this.children=[]}remove(){this.removed=true}}
  class AudioDestinationNode {}
  class AudioContext {createMediaStreamSource(){return {connect(){connections++}}}}
  const audioTrack={stop(){audioStops++},addEventListener(){}};
  const videoTrack={stop(){videoStops++},addEventListener(){}};
  class MediaStream {constructor(tracks){this.tracks=tracks}getAudioTracks(){return this.tracks}}
  const original=()=>{};
  const context={location:{protocol:'chrome-extension:',hostname:host},AudioDestinationNode,AudioContext,MediaStream,
    chrome:{tabCapture:{capture:original}},document:{body:new Element('body'),createElement:t=>new Element(t)},
    navigator:{mediaDevices:{async getDisplayMedia(){requests++;return {getAudioTracks:()=>audio?[audioTrack]:[],getVideoTracks:()=>[videoTrack],getTracks:()=>audio?[audioTrack,videoTrack]:[videoTrack]}}}},
    addEventListener(){},WeakSet};
  vm.runInNewContext(source,context);
  return {context,original,button:()=>elements.filter(e=>e.tag==='button').at(-1),counts:()=>({requests,connections,videoStops,audioStops})};
}
const test=environment();let captured;
test.context.chrome.tabCapture.capture({audio:true},stream=>{captured=stream});
assert.equal(test.counts().requests,0,'must wait for an explicit click');
await test.button().onclick();assert.ok(captured);assert.equal(test.counts().requests,1);
const sourceNode=new test.context.AudioContext().createMediaStreamSource(captured);
sourceNode.connect(new test.context.AudioDestinationNode());assert.equal(test.counts().connections,0,'must suppress system-audio feedback');
sourceNode.connect({});assert.equal(test.counts().connections,1,'recorder must still receive audio');
captured.getAudioTracks()[0].stop();assert.ok(test.counts().videoStops>=1,'stop screen capture together with audio');
const silent=environment({audio:false});let callback=false;silent.context.chrome.tabCapture.capture({},()=>callback=true);await silent.button().onclick();assert.equal(callback,false);assert.ok(silent.counts().videoStops>=1);
const other=environment({host:'other-extension'});assert.equal(other.context.chrome.tabCapture.capture,other.original);
console.log('Shazam adapter: consent, audio stream, feedback suppression, cleanup, no-audio and origin isolation OK');
