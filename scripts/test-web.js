#!/usr/bin/env node
// Offline checks of the embedded web app: script syntax and the closed
// markdown subset that renders untrusted model text. Node standard library only.
'use strict';
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');

const app = path.join(__dirname, '..', 'apps', 'daemon', 'src', 'web', 'app.js');
execFileSync(process.execPath, ['--check', app], { stdio: 'inherit' });

const source = fs.readFileSync(app, 'utf8');
const start = source.indexOf('function escapeHtml');
const end = source.indexOf('const messages = ');
if (start < 0 || end < start) throw new Error('renderer functions not found in app.js');
const { markdown } = new Function(`${source.slice(start, end)}; return { markdown };`)();

const link = (url) => `<a href="${url}" target="_blank" rel="noopener noreferrer">${url}</a>`;
const cases = [
  ['plain text', 'Hello', '<p>Hello</p>'],
  ['markup is escaped', '<img src=x onerror=alert(1)>', '<p>&lt;img src=x onerror=alert(1)&gt;</p>'],
  ['lists', 'Items:\n- one\n- **two**\n\n1. first\n2. second',
    '<p>Items:</p><ul><li>one</li><li><strong>two</strong></li></ul><ol><li>first</li><li>second</li></ol>'],
  ['heading', '## Plan\nDo it', '<h4>Plan</h4><p>Do it</p>'],
  ['code block is not reformatted', 'Run:\n```sh\nls **x** <b>\n```\nDone',
    '<p>Run:</p><pre><code>ls **x** &lt;b&gt;</code></pre><p>Done</p>'],
  ['inline code is not reformatted', 'Use `**x**` and *em*', '<p>Use <code>**x**</code> and <em>em</em></p>'],
  ['link excludes trailing punctuation', 'See https://example.com/a?b=1&c=2.',
    `<p>See ${link('https://example.com/a?b=1&amp;c=2')}.</p>`],
  ['quotes cannot leave the attribute', 'https://e.com/"onmouseover="x',
    `<p>${link('https://e.com/&quot;onmouseover=&quot;x')}</p>`],
  ['only http(s) becomes a link', 'javascript:alert(1)', '<p>javascript:alert(1)</p>'],
  ['placeholder markers cannot be forged', 'a\u00000\u0000b', '<p>a0b</p>'],
  ['bullet star with emphasis', '* item with *emphasis*', '<ul><li>item with <em>emphasis</em></li></ul>'],
  ['line breaks', 'a\nb', '<p>a<br>b</p>'],
];
let failed = 0;
for (const [name, input, expected] of cases) {
  const actual = markdown(input);
  if (actual !== expected) {
    failed += 1;
    console.error(`FAIL ${name}\n  expected ${expected}\n  actual   ${actual}`);
  }
}
if (failed) process.exit(1);
console.log(`web app checks passed (${cases.length} renderer cases)`);
