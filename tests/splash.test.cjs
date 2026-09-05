const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { JSDOM } = require('jsdom');

const html = readFileSync(`${__dirname}/../src/splash.html`, 'utf8');
const tick = () => new Promise(resolve => setImmediate(resolve));

function splash(t, { invoke = async () => {}, playbackError = false, bridge = true } = {}) {
  const calls = [];
  const timers = new Map();
  const warnings = [];
  const dom = new JSDOM(html, {
    runScripts: 'dangerously',
    beforeParse(window) {
      window.setTimeout = (callback, delay) => {
        timers.set(delay, callback);
        return delay;
      };
      window.clearTimeout = id => timers.delete(id);
      window.console.warn = (...args) => warnings.push(args);
      window.HTMLMediaElement.prototype.play = () => playbackError
        ? Promise.reject(new Error('video unavailable')) : Promise.resolve();
      if (bridge) {
        window.__TAURI__ = { core: { invoke: async command => {
          calls.push(command);
          return invoke(command);
        } } };
      }
    },
  });
  t.after(() => dom.window.close());
  return { window: dom.window, calls, timers, warnings };
}

test('overlapping splash completion events invoke the handoff only once', async t => {
  let resolve;
  const { window, calls, timers } = splash(t, { invoke: () => new Promise(done => { resolve = done; }) });
  window.document.querySelector('#v').dispatchEvent(new window.Event('ended'));
  window.document.querySelector('#skip').click();
  assert.deepEqual(calls, ['show_main_and_close_splash']);
  resolve();
  await tick();
  window.document.querySelector('#skip').click();
  assert.equal(calls.length, 1);
  assert.equal(timers.size, 0);
});

test('video playback rejection skips the intro without an unhandled rejection', async t => {
  const { calls } = splash(t, { playbackError: true });
  await tick();
  assert.deepEqual(calls, ['show_main_and_close_splash']);
});

test('non-bubbling source errors and Escape can finish the intro', async t => {
  const source = splash(t);
  source.window.document.querySelector('source').dispatchEvent(new source.window.Event('error'));
  await tick();
  assert.equal(source.calls.length, 1);
  const keyboard = splash(t);
  keyboard.window.document.dispatchEvent(new keyboard.window.KeyboardEvent('keydown', { key: 'Escape' }));
  await tick();
  assert.equal(keyboard.calls.length, 1);
});

test('a failed handoff keeps the timeout available for another attempt', async t => {
  let fail = true;
  const { window, calls, timers, warnings } = splash(t, { invoke: async () => {
    if (fail) throw new Error('temporary IPC failure');
  } });
  window.document.querySelector('#skip').click();
  await tick();
  assert.equal(warnings.length, 1);
  assert.ok(timers.has(8000));
  fail = false;
  timers.get(8000)();
  await tick();
  assert.equal(calls.length, 2);
  assert.equal(timers.size, 0);
});

test('missing desktop bridge is handled while native recovery remains available', async t => {
  const { window, warnings, timers } = splash(t, { bridge: false });
  window.document.querySelector('#skip').click();
  await tick();
  assert.equal(warnings.length, 1);
  assert.ok(timers.has(8000));
});
