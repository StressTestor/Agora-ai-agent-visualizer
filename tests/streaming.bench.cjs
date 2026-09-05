// Synthetic DOM workload, not a measurement of WebView paint or provider latency.
const { readFileSync } = require('node:fs');
const { performance } = require('node:perf_hooks');
const { JSDOM } = require('jsdom');

async function run() {
  const html = readFileSync(process.argv[2] || `${__dirname}/../src/index.html`, 'utf8');
  const listeners = new Map();
  const frames = [];
  const history = Array.from({ length: 200 }, (_, i) => ({
    from: 'agent', to: 'all', team: 'synthetic', timestamp: i,
    content: `Historical message ${i}: ${'context '.repeat(40)}`,
  }));
  const dom = new JSDOM(html, {
    runScripts: 'dangerously',
    beforeParse(window) {
      window.requestAnimationFrame = callback => { frames.push(callback); return frames.length; };
      window.__TAURI__ = {
        core: { invoke: async command => command === 'get_teams' ? ['synthetic'] : command === 'get_messages' ? history : {} },
        event: { listen: async (name, handler) => listeners.set(name, handler) },
      };
    },
  });
  await new Promise(resolve => setImmediate(resolve));
  let historyQueries = 0;
  const chat = dom.window.document.querySelector('#chat');
  const query = chat.querySelectorAll.bind(chat);
  chat.querySelectorAll = selector => {
    if (selector === '.msg') historyQueries++;
    return query(selector);
  };
  const paint = () => { for (const callback of frames.splice(0)) callback(0); };
  const started = performance.now();
  for (let i = 0; i < 500; i++) {
    listeners.get('debate-message-chunk')({ payload: { team: 'synthetic', agent: 'agent', chunk: 'word ' } });
    if ((i + 1) % 8 === 0) paint();
  }
  paint();
  const elapsed = performance.now() - started;
  const body = dom.window.document.querySelector('.streaming-bubble-body');
  console.log(JSON.stringify({
    history_messages: history.length, chunks: 500,
    processing_ms: Number(elapsed.toFixed(2)), history_queries: historyQueries,
    stream_text_nodes: [...body.childNodes].filter(node => node.nodeType === 3).length,
    output_correct: body.textContent === 'word '.repeat(500),
  }));
  dom.window.close();
}

run().catch(error => { console.error(error); process.exitCode = 1; });
