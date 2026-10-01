#!/usr/bin/env node
// Manual browser check of the embedded web app against the real daemon and CLI.
//
//   cargo build -p ditto-daemon -p ditto-cli
//   NODE_PATH=<dir containing puppeteer-core> node scripts/web-e2e.js
//
// Starts a mock OpenAI-compatible model on loopback (no network, no key, no
// cost), the built daemon with a temporary data directory, and headless
// Chromium (CHROME_PATH, default /usr/bin/chromium). Screenshots go to
// target/web-e2e/. Exits non-zero if any check fails.
'use strict';
const { execFile, spawn } = require('child_process');
const fs = require('fs');
const http = require('http');
const net = require('net');
const os = require('os');
const path = require('path');

const root = path.join(__dirname, '..');
const shots = path.join(root, 'target', 'web-e2e');
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const results = [];
function check(name, ok, detail = '') {
  results.push(Boolean(ok));
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ` - ${detail}` : ''}`);
}

function event(delta, finish = null) {
  return `data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason: finish }] })}\n\n`;
}

// The mock streams a markdown reply describing what it received, so the page
// shows which memory and how much history reached the model. Asked to recall,
// it first calls memory.search and then answers with what the search found;
// told to remember, it calls memory.remember.
function mockModel() {
  const server = http.createServer((request, response) => {
    let raw = '';
    request.on('data', (chunk) => { raw += chunk; });
    request.on('end', async () => {
      const body = JSON.parse(raw);
      const messages = body.messages;
      const system = (messages.find((message) => message.role === 'system') || {}).content || '';
      const prior = messages.slice(0, -1).filter((message) => message.role !== 'system').length;
      const memory = system.includes('afternoon meetings') ? 'afternoon' : system.includes('morning meetings') ? 'morning' : 'none';
      // Answer the user's words, not Ditto's leading time note.
      const user = [...messages].reverse().find((message) => message.role === 'user');
      const question = user.content.replace(/^\[Ditto:[^\]]*\]\n\n/, '');
      const result = messages[messages.length - 1].role === 'tool' ? JSON.parse(messages[messages.length - 1].content) : null;
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      if (question.includes('recall') && !result) {
        const call = { index: 0, id: 'call-recall', type: 'function', function: { name: 'memory_search', arguments: '{"query":"meetings"}' } };
        response.write(event({ tool_calls: [call] }));
        response.write(event({}, 'tool_calls'));
        response.end('data: [DONE]\n\n');
        return;
      }
      if (question.includes('Remember that') && !result) {
        const call = { index: 0, id: 'call-remember', type: 'function', function: { name: 'memory_remember', arguments: '{"text":"The user plays tennis on Sundays."}' } };
        response.write(event({ tool_calls: [call] }));
        response.write(event({}, 'tool_calls'));
        response.end('data: [DONE]\n\n');
        return;
      }
      // Long enough for the page to show the search in progress.
      if (result) await sleep(800);
      const reply = result && result.remembered
        ? `saved as ${result.remembered}`
        : result
        ? `recalled: ${result.memories.map((found) => found.text).join('; ')}`
        : `You asked: *${question}*\n\n- memory seen: **${memory}**\n- earlier messages: \`${prior}\`\n\n\`\`\`\nstreamed by a mock model\n\`\`\``;
      const delay = question.includes('slowly') ? 400 : 60;
      for (let index = 0; index < reply.length; index += 12) {
        if (response.destroyed) return;
        response.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { content: reply.slice(index, index + 12) } }] })}\n\n`);
        await sleep(delay);
      }
      response.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })}\n\n`);
      response.end('data: [DONE]\n\n');
    });
  });
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server)));
}

function freePort() {
  return new Promise((resolve) => {
    const probe = net.createServer();
    probe.listen(0, '127.0.0.1', () => {
      const { port } = probe.address();
      probe.close(() => resolve(port));
    });
  });
}

async function lastAssistant(page) {
  return page.$$eval('.msg.assistant', (nodes) => {
    const node = nodes[nodes.length - 1];
    return node && { text: node.querySelector('.body').innerText, foot: node.querySelector('.foot').innerText, failed: node.classList.contains('failed') };
  });
}
// A finished bubble, answered or failed, has a footer.
async function waitDone(page, count) {
  await page.waitForFunction((expected) => {
    const nodes = document.querySelectorAll('.msg.assistant');
    return nodes.length >= expected && nodes[nodes.length - 1].querySelector('.foot').innerText.length > 0;
  }, { timeout: 20000 }, count);
  return lastAssistant(page);
}
function statusWithHost(port, host) {
  return new Promise((resolve, reject) => {
    http.get({ host: '127.0.0.1', port, path: '/', headers: { host } }, (response) => {
      response.resume();
      resolve(response.statusCode);
    }).on('error', reject);
  });
}
async function send(page, text) {
  await page.click('#input');
  await page.type('#input', text);
  await page.keyboard.press('Enter');
}
const online = (page) => page.waitForFunction(() => document.querySelector('#connection').dataset.state === 'online', { timeout: 10000 });

async function main() {
  const puppeteer = require('puppeteer-core');
  const daemonBin = path.join(root, 'target', 'debug', 'ditto-daemon');
  const cli = path.join(root, 'target', 'debug', 'ditto');
  for (const binary of [daemonBin, cli]) {
    if (!fs.existsSync(binary)) throw new Error(`build first: missing ${binary}`);
  }
  fs.mkdirSync(shots, { recursive: true });
  const data = fs.mkdtempSync(path.join(os.tmpdir(), 'ditto-web-e2e-'));
  const model = await mockModel();
  const port = await freePort();
  const base = `http://127.0.0.1:${port}`;
  const daemon = spawn(daemonBin, [
    '--provider', 'openai-compatible', '--base-url', `http://127.0.0.1:${model.address().port}/v1`, '--model', 'mock',
    '--data-dir', path.join(data, 'data'), '--capabilities-dir', path.join(root, 'capabilities'), '--bind', `127.0.0.1:${port}`,
  ], { stdio: 'ignore' });
  // An interrupted check still stops the daemon and removes its data.
  const abandon = () => {
    daemon.kill('SIGTERM');
    fs.rmSync(data, { recursive: true, force: true });
    process.exit(130);
  };
  process.once('SIGINT', abandon);
  process.once('SIGTERM', abandon);
  const browser = await puppeteer.launch({
    executablePath: process.env.CHROME_PATH || '/usr/bin/chromium',
    headless: true,
    args: ['--no-sandbox', '--lang=en-US'],
  });
  try {
    for (let attempt = 0; ; attempt += 1) {
      try { await fetch(`${base}/health`); break; } catch (error) { if (attempt > 100) throw error; await sleep(100); }
    }
    const problems = [];
    const page = await browser.newPage();
    await page.setViewport({ width: 1280, height: 820 });
    page.on('console', (message) => { if (['error', 'warn', 'warning'].includes(message.type())) problems.push(`${message.type()}: ${message.text()}`); });
    page.on('pageerror', (error) => problems.push(`pageerror: ${error.message}`));
    page.on('dialog', async (dialog) => { problems.push(`dialog: ${dialog.message()}`); await dialog.dismiss(); });

    await page.goto(`${base}/`, { waitUntil: 'load' });
    check('page loads', (await page.title()) === 'Ditto');
    await online(page);
    check('live event stream connects', true);

    await page.type('#memory-text', 'I prefer afternoon meetings');
    await page.click('#memory-save');
    await page.waitForFunction(() => document.querySelector('#memory-list').innerText.includes('I prefer afternoon meetings'), { timeout: 10000 });
    check('memory saved from the page', true);

    await send(page, 'When should we meet?');
    const lengths = new Set();
    for (let index = 0; index < 60; index += 1) {
      const state = await lastAssistant(page);
      if (state) lengths.add(state.text.length);
      if (state && state.foot) break;
      await sleep(50);
    }
    const first = await waitDone(page, 1);
    check('answer streams incrementally', lengths.size >= 3, `${lengths.size} partial lengths`);
    check('memory reached the model', first.text.includes('memory seen: afternoon'));
    check('answer is labelled unverified', first.foot.includes('unverified'));
    check('markdown renders', await page.$$eval('.msg.assistant', (nodes) => {
      const body = nodes[nodes.length - 1].querySelector('.body');
      return Boolean(body.querySelector('ul li strong') && body.querySelector('pre code') && body.querySelector('em'));
    }));
    await page.screenshot({ path: path.join(shots, '1-chat.png') });

    await page.$$eval('.msg.assistant .foot button.link', (buttons) => buttons[buttons.length - 1].click());
    await page.waitForFunction(() => !document.querySelector('#inspector').classList.contains('hidden'), { timeout: 10000 });
    const why = await page.$eval('#inspector-body', (node) => node.innerText);
    check('inspector shows the memory and why it was sent', why.includes('I prefer afternoon meetings') && why.includes('all memories fit, so all were sent'));
    await page.screenshot({ path: path.join(shots, '2-inspector.png') });
    await page.click('#inspector-close');

    await send(page, 'And on Fridays?');
    check('follow-up carries the thread', (await waitDone(page, 2)).text.includes('earlier messages: 2'));

    await page.reload({ waitUntil: 'load' });
    await page.waitForFunction(() => document.querySelectorAll('.msg.assistant').length === 2, { timeout: 10000 });
    const restored = await page.$$eval('.msg.user', (nodes) => nodes.map((node) => node.innerText));
    check('thread survives a reload', JSON.stringify(restored) === JSON.stringify(['When should we meet?', 'And on Fridays?']));
    await online(page);

    // Asynchronous: the mock model shares this process's event loop.
    const terminal = new Promise((resolve, reject) => {
      execFile(cli, ['--api', base, 'run', 'Asked from the terminal'], (error) => (error ? reject(error) : resolve()));
    });
    await page.waitForFunction(() => [...document.querySelectorAll('.msg.user')].some((node) => node.innerText === 'Asked from the terminal'), { timeout: 20000 });
    check('a CLI run appears live', (await waitDone(page, 3)).text.includes('earlier messages: 4'));
    await terminal;

    await send(page, 'Please answer slowly');
    await page.waitForSelector('#stop:not(.hidden)', { timeout: 10000 });
    await page.waitForFunction(() => {
      const nodes = document.querySelectorAll('.msg.assistant .body');
      return nodes[nodes.length - 1].innerText.length > 0;
    }, { timeout: 10000 });
    await page.click('#stop');
    await page.waitForFunction(() => {
      const nodes = document.querySelectorAll('.msg.assistant');
      return nodes[nodes.length - 1].classList.contains('failed');
    }, { timeout: 20000 });
    const stopped = await lastAssistant(page);
    check('stop cancels the streaming answer', stopped.text.startsWith('Stopped'), stopped.text);
    check('composer is ready again', await page.$eval('#send', (node) => !node.classList.contains('hidden')));

    await page.click('#new-thread');
    await page.waitForSelector('.divider', { timeout: 10000 });
    await send(page, 'Fresh start');
    check('a new conversation has no history', (await waitDone(page, 5)).text.includes('earlier messages: 0'));

    await send(page, '<img src=x onerror=alert(1)> **x**');
    await waitDone(page, 6);
    check('echoed markup stays text', (await page.$$eval('.messages img, .messages [onerror]', (nodes) => nodes.length)) === 0);

    await page.$$eval('#memory-list button.link', (buttons) => buttons[0].click());
    await page.$eval('#memory-text', (node) => { node.value = ''; });
    await page.type('#memory-text', 'I prefer morning meetings');
    await page.click('#memory-save');
    await page.waitForFunction(() => {
      const text = document.querySelector('#memory-list').innerText;
      return text.includes('morning meetings') && !text.includes('afternoon meetings');
    }, { timeout: 10000 });
    await send(page, 'When should we meet now?');
    check('a corrected memory replaces the old one', (await waitDone(page, 7)).text.includes('memory seen: morning'));

    await send(page, 'Please recall my meetings');
    await page.waitForFunction(() => {
      const nodes = document.querySelectorAll('.msg.assistant .body');
      const body = nodes[nodes.length - 1];
      return body.classList.contains('progress') && body.innerText === 'Searching memories…';
    }, { timeout: 10000 });
    check('a running memory search shows progress', true);
    const recalled = await waitDone(page, 8);
    check('the memory search result reaches the model', recalled.text.includes('recalled: I prefer morning meetings') && recalled.foot.includes('memory.search'), recalled.text);

    await send(page, 'Remember that I play tennis on Sundays');
    const saved = await waitDone(page, 9);
    const notice = await page.evaluate(() => {
      const nodes = document.querySelectorAll('.msg.assistant');
      const notices = nodes[nodes.length - 1].querySelector('.notices');
      return notices ? notices.innerText : '';
    });
    check('Ditto says what it remembered', saved.text.includes('saved as memory-') && notice === 'Remembered: The user plays tennis on Sundays.', notice);
    await page.waitForFunction(() => [...document.querySelectorAll('#memory-list li')]
      .some((item) => item.innerText.includes('tennis on Sundays') && item.querySelector('.tag')), { timeout: 10000 });
    check('the memory list marks what Ditto inferred', true);
    const forgetTennis = () => page.evaluate(() => {
      const item = [...document.querySelectorAll('#memory-list li')].find((node) => node.innerText.includes('tennis on Sundays'));
      [...item.querySelectorAll('button.link')].find((button) => button.dataset.armed || button.innerText === 'Forget').click();
    });
    await forgetTennis();
    const armed = await page.evaluate(() => [...document.querySelectorAll('#memory-list button.link')].some((button) => button.innerText === 'Forget it?'));
    await forgetTennis();
    await page.waitForFunction(() => !document.querySelector('#memory-list').innerText.includes('tennis on Sundays'), { timeout: 10000 });
    check('a memory is forgotten from the list after a second click', armed);

    const inView = () => page.evaluate(() => {
      const header = document.querySelector('.chat-header').getBoundingClientRect();
      const composer = document.querySelector('#composer').getBoundingClientRect();
      return header.top >= 0 && composer.bottom <= window.innerHeight + 1;
    });
    check('header and composer stay in view in a long thread', await inView());

    await page.click('.tab[data-panel="schedules"]');
    await page.type('#schedule-text', 'Remind me about the meeting');
    await page.click('#schedule-form button[type="submit"]');
    await page.waitForFunction(() => document.querySelector('#schedule-list').innerText.includes('pending'), { timeout: 10000 });
    check('schedule created and listed', true);
    await page.screenshot({ path: path.join(shots, '3-schedules.png') });
    await page.$$eval('#schedule-list li button.link', (buttons) => buttons[0].click());
    await page.waitForFunction(() => document.querySelector('#schedule-list').innerText.includes('Nothing scheduled.'), { timeout: 10000 });
    check('schedule cancelled from the page', true);

    await page.emulateMediaFeatures([{ name: 'prefers-color-scheme', value: 'dark' }]);
    await page.click('.tab[data-panel="memories"]');
    await page.screenshot({ path: path.join(shots, '4-dark.png') });
    check('dark theme', (await page.$eval('body', (node) => getComputedStyle(node).backgroundColor)) === 'rgb(20, 20, 19)');
    await page.emulateMediaFeatures([{ name: 'prefers-color-scheme', value: 'light' }]);
    await page.setViewport({ width: 390, height: 844, isMobile: true });
    await sleep(200);
    check('no horizontal overflow at phone width', (await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)) <= 0);
    check('header and composer stay in view at phone width', await inView());
    await page.screenshot({ path: path.join(shots, '5-phone.png') });

    const korean = await browser.newPage();
    await korean.evaluateOnNewDocument(() => Object.defineProperty(navigator, 'language', { get: () => 'ko-KR' }));
    korean.on('pageerror', (error) => problems.push(`pageerror(ko): ${error.message}`));
    await korean.setViewport({ width: 1280, height: 820 });
    await korean.goto(`${base}/`, { waitUntil: 'load' });
    check('Korean interface', (await korean.$eval('#new-thread', (node) => node.innerText)) === '새 대화');
    await korean.screenshot({ path: path.join(shots, '6-korean.png') });

    check('a foreign Host name is refused', (await statusWithHost(port, 'evil.example')) === 403);
    check('a loopback Host name is served', (await statusWithHost(port, `localhost:${port}`)) === 200);
    check('no console errors, CSP violations or dialogs', problems.length === 0, problems.join(' ; '));
  } finally {
    await browser.close();
    daemon.kill('SIGTERM');
    model.close();
    await new Promise((resolve) => daemon.once('exit', resolve));
    fs.rmSync(data, { recursive: true, force: true });
  }
  const passed = results.filter(Boolean).length;
  console.log(`\n${passed}/${results.length} checks passed; screenshots in ${path.relative(root, shots)}/`);
  process.exit(passed === results.length ? 0 : 1);
}

main().catch((error) => {
  console.error(error);
  process.exit(2);
});
