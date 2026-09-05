const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { JSDOM } = require('jsdom');

const html = readFileSync(`${__dirname}/../src/index.html`, 'utf8');
const message = (team, content = 'hello') => ({ team, from: 'agent', to: 'all', content, timestamp: 1000 });

async function app(t, existing = [], failures = [], handlers = {}) {
  const listeners = new Map();
  const calls = [];
  const frames = new Map();
  let nextFrame = 0;
  const paint = () => {
    const callbacks = [...frames.values()];
    frames.clear();
    callbacks.forEach(callback => callback(0));
  };
  const dom = new JSDOM(html, {
    runScripts: 'dangerously',
    beforeParse(window) {
      window.requestAnimationFrame = callback => { frames.set(++nextFrame, callback); return nextFrame; };
      window.cancelAnimationFrame = id => frames.delete(id);
      window.__TAURI__ = {
        core: { invoke: async (command, args) => {
          calls.push({ command, args });
          if (failures.includes(command)) throw new Error('test request rejected');
          if (handlers[command]) return handlers[command](args);
          if (command === 'get_teams') return ['alpha', 'beta'];
          if (command === 'get_messages') return existing;
          if (command === 'get_debate_status') throw `no debate '${args.team}'`;
          if (command === 'get_config') return { providers: {} };
          if (['list_debate_presets', 'list_role_presets', 'list_team_configs', 'list_models'].includes(command)) return [];
          return {};
        } },
        event: { listen: async (event, handler) => listeners.set(event, handler) },
      };
    },
  });
  t.after(() => dom.window.close());
  await new Promise(resolve => setImmediate(resolve));
  const document = dom.window.document;
  const select = team => {
    document.querySelector('#team-select').value = team;
    document.querySelector('#team-select').dispatchEvent(new dom.window.Event('change'));
  };
  return { window: dom.window, document, calls, select, paint, emit: (event, payload, render = true) => {
    assert.ok(listeners.has(event), `listener registered: ${event}`);
    listeners.get(event)({ payload });
    if (render) paint();
  } };
}

test('incoming messages preserve the reading position when scrolled up', async t => {
  const { document, window, emit } = await app(t);
  const chat = document.querySelector('#chat');
  Object.defineProperties(chat, { scrollHeight: { value: 2000 }, clientHeight: { value: 400 } });
  chat.scrollTop = 100;
  chat.dispatchEvent(new window.Event('scroll'));
  emit('new-message', message('alpha'));
  assert.equal(chat.scrollTop, 100);
  document.querySelector('#scroll-btn').click();
  emit('new-message', message('alpha', 'second'));
  assert.equal(chat.scrollTop, 2000);
});

const flush = () => new Promise(resolve => setImmediate(resolve));
const change = (window, element, value, event = 'change') => {
  element.value = value;
  element.dispatchEvent(new window.Event(event, { bubbles: true }));
};

async function openAgents(ui) {
  ui.document.querySelector('#btn-new-debate').click();
  await flush();
  ui.document.querySelector('#wizard-team-name').value = 'test-debate';
  ui.document.querySelector('#wizard-next').click();
  await flush();
}

async function openRules(ui) {
  await openAgents(ui);
  for (const provider of ui.document.querySelectorAll('[data-field="provider"]')) change(ui.window, provider, 'claude-code');
  await flush();
  for (const model of ui.document.querySelectorAll('[data-field="model"]')) model.value = 'test-model';
  ui.document.querySelector('#wizard-next').click();
  await flush();
}

test('fixed rounds reject fractional or empty values and other modes explain their stopping rule', async t => {
  const ui = await app(t);
  await openRules(ui);
  const term = ui.document.querySelector('#wizard-termination');
  const rounds = ui.document.querySelector('#wizard-max-rounds');
  assert.ok(rounds.closest('.field-group').hidden);
  change(ui.window, term, 'fixed');
  assert.equal(rounds.closest('.field-group').hidden, false);
  for (const value of ['', '1.5', '101']) {
    rounds.value = value;
    ui.document.querySelector('#wizard-next').click();
    await flush();
    assert.equal(ui.document.querySelector('#wizard-step-indicator').textContent, 'step 3 of 4');
    assert.match(ui.document.querySelector('#wizard-error').textContent, /whole number/);
  }
  change(ui.window, term, 'topic');
  ui.document.querySelector('#wizard-next').click();
  await flush();
  assert.match(ui.document.querySelector('#wizard-error').textContent, /topic/);
  assert.match(ui.document.querySelector('#wizard-termination-help').textContent, /three complete rounds/);
});

test('late canceled setup requests do not duplicate the reopened form', async t => {
  const pending = [];
  const ui = await app(t, [], [], { list_team_configs: () => new Promise(resolve => pending.push(resolve)) });
  ui.document.querySelector('#btn-new-debate').click();
  await flush();
  ui.document.querySelector('#wizard-cancel').click();
  ui.document.querySelector('#btn-new-debate').click();
  await flush();
  pending[1]([]);
  await flush();
  ui.document.querySelector('#wizard-team-name').value = 'keep-new-draft';
  pending[0]([]);
  await flush();
  assert.equal(ui.document.querySelectorAll('#wizard-team-name').length, 1);
  assert.equal(ui.document.querySelector('#wizard-team-name').value, 'keep-new-draft');
});

test('canceling a pending create cannot start or close a replacement wizard', async t => {
  let finishCreate;
  const ui = await app(t, [], [], { create_debate: () => new Promise(resolve => { finishCreate = resolve; }) });
  await openRules(ui);
  ui.document.querySelector('#wizard-next').click();
  await flush();
  ui.document.querySelector('#wizard-next').click();
  await flush();
  ui.document.querySelector('#wizard-cancel').click();
  ui.document.querySelector('#btn-new-debate').click();
  await flush();
  finishCreate('test-debate');
  await flush();
  assert.ok(ui.document.querySelector('#wizard-overlay').classList.contains('visible'));
  assert.equal(ui.calls.filter(call => call.command === 'start_debate_cmd').length, 0);
});

test('switching teams reuses rendered messages and preserves collapse state', async t => {
  const ui = await app(t, [message('alpha', 'long response')]);
  const original = ui.document.querySelector('.msg');
  original.querySelector('button').click();
  ui.select('beta');
  ui.select('alpha');
  assert.equal(ui.document.querySelector('.msg'), original);
  assert.ok(original.querySelector('.msg-body').classList.contains('collapsed'));
  assert.ok(original.querySelector('.msg-badge').hidden);
  ui.select('');
  assert.equal(original.querySelector('.msg-badge').hidden, false);
});

test('switching teams clears a search that previously hid a live stream', async t => {
  const ui = await app(t);
  ui.emit('debate-message-chunk', { team: 'alpha', agent: 'agent', chunk: 'visible again' });
  change(ui.window, ui.document.querySelector('#search-input'), 'missing', 'input');
  assert.equal(ui.document.querySelector('.streaming-bubble').style.display, 'none');
  ui.select('alpha');
  assert.equal(ui.document.querySelector('.streaming-bubble').style.display, '');
});

test('filtering providers does not silently change the current selection', async t => {
  const ui = await app(t);
  await openAgents(ui);
  const provider = ui.document.querySelector('[data-field="provider"]');
  change(ui.window, provider, 'openai');
  await flush();
  const search = provider.previousElementSibling;
  change(ui.window, search, 'anthropic', 'input');
  assert.equal(provider.value, 'openai');
  change(ui.window, search, '', 'input');
  assert.equal(provider.value, 'openai');
});

test('late model responses cannot replace the currently selected provider models', async t => {
  const pending = {};
  const ui = await app(t, [], [], { list_models: ({ providerName }) => new Promise(resolve => { pending[providerName] = resolve; }) });
  await openAgents(ui);
  const provider = ui.document.querySelector('[data-field="provider"]');
  change(ui.window, provider, 'openai');
  change(ui.window, provider, 'anthropic');
  pending.openai([{ id: 'old-model' }]);
  await flush();
  pending.anthropic([{ id: 'current-model' }]);
  await flush();
  const options = [...ui.document.querySelector('[data-field="model"]').options].map(option => option.value);
  assert.ok(options.includes('current-model'));
  assert.ok(!options.includes('old-model'));
});

test('model discovery preserves a custom ID typed while loading', async t => {
  let resolveModels;
  const ui = await app(t, [], [], { list_models: () => new Promise(resolve => { resolveModels = resolve; }) });
  await openAgents(ui);
  change(ui.window, ui.document.querySelector('[data-field="provider"]'), 'openai');
  ui.document.querySelector('[data-field="model"]').value = 'my-custom-model';
  resolveModels([{ id: 'listed-model' }]);
  await flush();
  assert.equal(ui.document.querySelector('[data-field="model"]').value, 'my-custom-model');
});

test('a blank team name is reported on its step before advancing', async t => {
  const ui = await app(t);
  ui.document.querySelector('#btn-new-debate').click();
  await flush();
  ui.document.querySelector('#wizard-next').click();
  await flush();
  assert.equal(ui.document.querySelector('#wizard-step-indicator').textContent, 'step 1 of 4');
  assert.match(ui.document.querySelector('#wizard-error').textContent, /name/i);
  assert.equal(ui.document.activeElement.id, 'wizard-team-name');
});

test('duplicate agent names are rejected before moving to debate rules', async t => {
  const ui = await app(t);
  await openAgents(ui);
  for (const field of ui.document.querySelectorAll('[data-field="name"]')) field.value = 'duplicate';
  ui.document.querySelector('#wizard-next').click();
  await flush();
  assert.equal(ui.document.querySelector('#wizard-step-indicator').textContent, 'step 2 of 4');
  assert.match(ui.document.querySelector('#wizard-error').textContent, /unique|different|duplicate/i);
});

test('settings save failure leaves inputs available with an actionable error', async t => {
  const ui = await app(t, [], ['save_config']);
  ui.document.querySelector('#btn-settings').click();
  await flush();
  ui.document.querySelector('#settings-save').click();
  await flush();
  assert.ok(ui.document.querySelector('#settings-overlay').classList.contains('visible'));
  assert.match(ui.document.querySelector('#toast').textContent, /could not save settings/i);
  assert.equal(ui.document.querySelector('#settings-save').disabled, false);
});

test('a late provider test cannot repopulate the model cache after settings change', async t => {
  let resolveOld;
  let requests = 0;
  const ui = await app(t, [], [], { list_models: () => {
    requests++;
    return requests === 1 ? new Promise(resolve => { resolveOld = resolve; }) : [{ id: 'new-model' }];
  } });
  ui.document.querySelector('#btn-settings').click();
  await flush();
  const key = ui.document.querySelector('input[data-provider="openai"]');
  key.value = 'synthetic-old-key';
  ui.document.querySelector('#settings-providers button').click();
  await flush();
  key.value = 'synthetic-new-key';
  ui.document.querySelector('#settings-save').click();
  await flush();
  resolveOld([{ id: 'stale-model' }]);
  await flush();
  await openAgents(ui);
  change(ui.window, ui.document.querySelector('[data-field="provider"]'), 'openai');
  await flush();
  assert.equal(requests, 2);
  assert.match(ui.document.querySelector('[data-field="model"]').textContent, /new-model/);
  assert.doesNotMatch(ui.document.querySelector('[data-field="model"]').textContent, /stale-model/);
});

test('a burst of stream chunks uses one text node and never rescans completed messages', async t => {
  const ui = await app(t, Array.from({ length: 100 }, (_, i) => message('alpha', `history ${i}`)));
  const chat = ui.document.querySelector('#chat');
  let historyQueries = 0;
  const query = chat.querySelectorAll.bind(chat);
  chat.querySelectorAll = selector => {
    if (selector === '.msg') historyQueries++;
    return query(selector);
  };
  for (let i = 0; i < 1000; i++) {
    ui.emit('debate-message-chunk', { team: 'alpha', agent: 'agent', chunk: 'word ' }, false);
  }
  ui.paint();
  const body = ui.document.querySelector('.streaming-bubble-body');
  assert.equal(body.textContent, 'word '.repeat(1000));
  assert.equal(body.childNodes.length, 2, 'one text node plus the cursor');
  assert.equal(historyQueries, 0);
});

test('completion before the next paint does not restore a stale stream', async t => {
  const ui = await app(t);
  ui.emit('debate-message-chunk', { team: 'alpha', agent: 'agent', chunk: 'draft' }, false);
  ui.emit('debate-message-complete', { ...message('alpha', 'final'), agent: 'agent' }, false);
  ui.paint();
  assert.equal(ui.document.querySelectorAll('.streaming-bubble').length, 0);
  assert.equal(ui.document.querySelector('.msg-body').textContent, 'final');
});

test('streams with identical agent names stay separate across teams and filters', async t => {
  const { document, emit, select } = await app(t);
  emit('debate-message-chunk', { team: 'alpha', agent: 'agent', chunk: 'first ' });
  emit('debate-message-chunk', { team: 'beta', agent: 'agent', chunk: 'other' });
  assert.equal(document.querySelectorAll('.streaming-bubble').length, 2);
  select('beta');
  emit('debate-message-chunk', { team: 'alpha', agent: 'agent', chunk: 'second' });
  select('alpha');
  assert.equal(document.querySelectorAll('.streaming-bubble').length, 1);
  assert.equal(document.querySelector('.streaming-bubble-body').textContent, 'first second');
  emit('debate-message-complete', { ...message('beta', 'other'), agent: 'agent' });
  assert.equal(document.querySelectorAll('.streaming-bubble').length, 1);
  emit('debate-message-complete', { ...message('alpha', 'first second'), agent: 'agent' });
  assert.equal(document.querySelectorAll('.streaming-bubble').length, 0);
  assert.equal(document.querySelectorAll('.msg').length, 1);
});

test('search counts remain accurate as messages arrive', async t => {
  const { document, window, emit } = await app(t, [message('alpha', 'needle')]);
  const search = document.querySelector('#search-input');
  search.value = 'needle';
  search.dispatchEvent(new window.Event('input'));
  emit('new-message', message('alpha', 'unrelated'));
  assert.equal(document.querySelector('#msg-count').textContent, '1 of 2 messages');
  emit('debate-message-complete', { ...message('alpha', 'another needle'), agent: 'agent' });
  assert.equal(document.querySelector('#msg-count').textContent, '2 of 3 messages');
});

test('a message discussing a heartbeat is retained but a protocol notification is hidden', async t => {
  const { document, emit } = await app(t);
  emit('new-message', message('alpha', 'The "heartbeat" message indicates health.'));
  emit('new-message', message('alpha', '{"type":"heartbeat"}'));
  assert.equal(document.querySelectorAll('.msg').length, 1);
});

test('debate controls target the selected team, regardless of latest status event', async t => {
  const { document, emit, select, calls } = await app(t);
  emit('debate-status', { team: 'alpha', status: 'running', round: 1 });
  select('alpha');
  emit('debate-status', { team: 'beta', status: 'paused', round: 2 });
  assert.equal(document.querySelector('#debate-status-text').textContent, 'running');
  document.querySelector('#btn-pause').click();
  assert.equal(calls.at(-1).command, 'pause_debate');
  assert.equal(calls.at(-1).args.team, 'alpha');
  select('beta');
  assert.equal(document.querySelector('#btn-pause').textContent, '▶');
});

test('a rejected restart preserves the existing transcript', async t => {
  const { document, emit, select } = await app(t, [message('alpha', 'keep this')], ['restart_debate']);
  emit('debate-status', { team: 'alpha', status: 'running', round: 1 });
  select('alpha');
  document.querySelector('#btn-restart').click();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(document.querySelector('.msg-body').textContent, 'keep this');
  assert.match(document.querySelector('#toast').textContent, /test request rejected/);
});

test('stopping another team leaves the selected team stream intact', async t => {
  const { document, emit, select } = await app(t);
  select('alpha');
  emit('debate-message-chunk', { team: 'alpha', agent: 'agent', chunk: 'still streaming' });
  emit('debate-status', { team: 'beta', status: 'stopped', round: 1 });
  assert.equal(document.querySelector('.streaming-bubble-body').textContent, 'still streaming');
});

test('wizard loading failures are visible and cannot advance an empty step', async t => {
  const { document } = await app(t, [], ['list_debate_presets']);
  document.querySelector('#btn-new-debate').click();
  assert.equal(document.querySelector('#wizard-next').disabled, true);
  await new Promise(resolve => setImmediate(resolve));
  assert.match(document.querySelector('#wizard-body').textContent, /Could not load this step/);
  assert.equal(document.querySelector('#wizard-next').disabled, true);
  document.querySelector('#wizard-cancel').click();
  assert.equal(document.querySelector('#wizard-overlay').classList.contains('visible'), false);
});


test('startup merges live messages and snapshots without dropping or duplicating turns', async t => {
  let finishMessages;
  let finishTeams;
  const ui = await app(t, [], [], {
    get_messages: () => new Promise(resolve => { finishMessages = resolve; }),
    get_teams: () => new Promise(resolve => { finishTeams = resolve; }),
  });
  const turn = { ...message('alpha', 'shared turn'), timestamp: 2000 };
  ui.emit('new-message', turn);
  ui.emit('debate-message-complete', { ...turn, agent: turn.from });
  ui.emit('new-message', { ...turn, timestamp: 3000 });
  ui.emit('team-added', 'new-team');
  finishMessages([message('alpha', 'older turn'), turn]);
  finishTeams(['alpha', 'beta']);
  await flush();
  assert.deepEqual([...ui.document.querySelectorAll('.msg-body')].map(node => node.textContent), ['older turn', 'shared turn', 'shared turn']);
  assert.equal(ui.document.querySelector('#msg-count').textContent, '3 messages');
  assert.equal([...ui.document.querySelector('#team-select').options].filter(option => option.value === 'new-team').length, 1);
});

test('paused debate controls are restored from the initial status snapshot', async t => {
  const ui = await app(t, [message('alpha')], [], {
    get_debate_status: ({ team }) => {
      if (team !== 'alpha') throw `no debate '${team}'`;
      return { team, status: 'paused', round: 2 };
    },
  });
  ui.select('alpha');
  assert.ok(ui.document.querySelector('#debate-controls').classList.contains('active'));
  assert.equal(ui.document.querySelector('#debate-status-text').textContent, 'paused');
  assert.equal(ui.document.querySelector('#btn-pause').textContent, '▶');
  ui.document.querySelector('#btn-pause').click();
  assert.equal(ui.calls.at(-1).command, 'pause_debate');
  assert.equal(ui.calls.at(-1).args.team, 'alpha');
  assert.equal(ui.document.querySelector('#toast').textContent, '');
});

test('a late status snapshot cannot replace a newer live status', async t => {
  let finishStatus;
  const ui = await app(t, [], [], {
    get_debate_status: ({ team }) => team === 'alpha' ? new Promise(resolve => { finishStatus = resolve; }) : {},
  });
  ui.select('alpha');
  ui.emit('debate-status', { team: 'alpha', status: 'running', round: 3 });
  finishStatus({ team: 'alpha', status: 'paused', round: 2 });
  await flush();
  assert.equal(ui.document.querySelector('#debate-status-text').textContent, 'running');
  assert.equal(ui.document.querySelector('#debate-round-text').textContent, '· round 3');
});

test('deletion cleans only its captured team and preserves a newer selection', async t => {
  let finishDelete;
  const ui = await app(t, [message('alpha', 'remove alpha'), message('beta', 'keep beta')], [], {
    delete_team: () => new Promise(resolve => { finishDelete = resolve; }),
  });
  ui.window.confirm = () => true;
  ui.emit('debate-status', { team: 'alpha', status: 'stopped', round: 1 });
  ui.emit('debate-message-chunk', { team: 'alpha', agent: 'agent', chunk: 'remove draft' });
  ui.emit('debate-message-chunk', { team: 'beta', agent: 'agent', chunk: 'keep draft' });
  ui.select('alpha');
  ui.document.querySelector('#btn-delete-team').click();
  ui.select('beta');
  finishDelete();
  await flush();
  assert.equal(ui.calls.find(call => call.command === 'delete_team').args.team, 'alpha');
  assert.equal(ui.document.querySelector('#team-select').value, 'beta');
  assert.deepEqual([...ui.document.querySelector('#team-select').options].map(option => option.value), ['', 'beta']);
  assert.equal(ui.document.querySelector('.msg-body').textContent, 'keep beta');
  ui.select('');
  assert.equal(ui.document.querySelectorAll('.msg').length, 1);
  assert.equal(ui.document.querySelectorAll('.streaming-bubble').length, 1);
  assert.equal(ui.document.querySelector('.streaming-bubble-body').textContent, 'keep draft');
  assert.equal(ui.document.querySelector('#debate-controls').classList.contains('active'), false);
});

test('rejected deletion preserves the selected team and its transcript', async t => {
  const ui = await app(t, [message('alpha', 'keep history')], ['delete_team']);
  ui.window.confirm = () => true;
  ui.select('alpha');
  ui.document.querySelector('#btn-delete-team').click();
  await flush();
  assert.equal(ui.document.querySelector('#team-select').value, 'alpha');
  assert.equal(ui.document.querySelector('.msg-body').textContent, 'keep history');
});

test('restart retains a new turn delivered before its command response', async t => {
  let finishRestart;
  const ui = await app(t, [message('alpha', 'old turn')], [], {
    restart_debate: () => new Promise(resolve => { finishRestart = resolve; }),
  });
  ui.select('alpha');
  ui.emit('debate-status', { team: 'alpha', status: 'stopped', round: 1 });
  ui.document.querySelector('#btn-restart').click();
  ui.emit('debate-status', { team: 'alpha', status: 'running', round: 0 });
  ui.emit('debate-message-complete', { ...message('alpha', 'new turn'), agent: 'agent' });
  ui.emit('debate-status', { team: 'beta', status: 'paused', round: 2 });
  ui.select('beta');
  finishRestart();
  await flush();
  assert.equal(ui.document.querySelector('#team-select').value, 'beta');
  assert.equal(ui.document.querySelector('#btn-pause').textContent, '▶');
  ui.select('alpha');
  assert.equal(ui.document.querySelectorAll('.msg').length, 1);
  assert.equal(ui.document.querySelector('.msg-body').textContent, 'new turn');
});

test('launch retains a new turn delivered before its command response', async t => {
  let finishStart;
  const ui = await app(t, [message('test-debate', 'old turn')], [], {
    start_debate_cmd: () => new Promise(resolve => { finishStart = resolve; }),
  });
  await openRules(ui);
  ui.document.querySelector('#wizard-next').click();
  await flush();
  ui.document.querySelector('#wizard-next').click();
  await flush();
  ui.emit('debate-message-complete', { ...message('test-debate', 'new turn'), agent: 'agent' });
  finishStart();
  await flush();
  assert.equal(ui.document.querySelectorAll('.msg').length, 1);
  assert.equal(ui.document.querySelector('.msg-body').textContent, 'new turn');
});

test('retry reset discards partial output and queued chunks only for the matching agent', async t => {
  const ui = await app(t);
  ui.emit('debate-message-chunk', { team: 'beta', agent: 'agent', chunk: 'other team' });
  ui.emit('debate-message-chunk', { team: 'alpha', agent: 'agent', chunk: 'failed partial' });
  ui.emit('debate-message-chunk', { team: 'alpha', agent: 'agent', chunk: 'unpainted tail' }, false);
  ui.emit('debate-message-reset', { team: 'alpha', agent: 'agent' }, false);
  ui.emit('debate-thinking', { team: 'alpha', agent: 'agent' }, false);
  ui.emit('debate-message-chunk', { team: 'alpha', agent: 'agent', chunk: 'retry response' }, false);
  ui.paint();
  assert.equal(ui.document.querySelectorAll('.thinking-bubble').length, 0);
  assert.deepEqual([...ui.document.querySelectorAll('.streaming-bubble-body')].map(node => node.textContent), ['other team', 'retry response']);
  ui.emit('debate-message-complete', { ...message('alpha', 'retry response'), agent: 'agent' });
  assert.equal(ui.document.querySelector('.msg-body').textContent, 'retry response');
  assert.equal(ui.document.querySelector('.streaming-bubble-body').textContent, 'other team');
});
