'use strict';
// Ditto local web app. Every authority stays in the daemon: this page only
// calls the typed HTTP API and renders durable events. Model text is always
// escaped before a small, closed markdown subset is applied. The composer is
// never blocked: answers run at once, each under its own message (ADR 0035).

const SESSION = new URLSearchParams(location.search).get('session') || 'personal';
const $ = (id) => document.getElementById(id);

const STRINGS = {
  en: {
    newThread: 'New conversation', memories: 'Memories', schedules: 'Schedules',
    memoryPlaceholder: 'Something to remember…', save: 'Save', cancel: 'Cancel', loadMore: 'Load more',
    schedulePlaceholder: 'What should Ditto do later?', startAt: 'Start at', window: 'Start window',
    repeat: 'Repeat', once: 'Once', daily: 'Daily', weekly: 'Weekly', times: 'Times', scheduleIt: 'Schedule',
    conversation: 'Conversation', emptyTitle: 'Ask anything. Ditto remembers what you tell it.',
    emptyHint: 'Save memories on the left, or type /remember followed by a fact.',
    inputPlaceholder: 'Message Ditto…',
    whyTitle: 'Why this answer', online: 'live', offline: 'reconnecting…', unverified: 'unverified',
    unverifiedHint: 'Model answers are not verified task completion.',
    why: 'Why?', correct: 'Correct', forget: 'Forget', forgetConfirm: 'Forget it?', newDivider: 'new conversation',
    noMemories: 'No memories yet.', noSchedules: 'Nothing scheduled.',
    disabled: 'No model is configured. Start the daemon with --provider openai-compatible (for example a local Ollama model) or --provider openai.',
    busy: 'Ditto is already working on four requests; try again in a moment.', stop: 'Stop',
    failed: 'No answer', interrupted: 'Stopped', usedMemories: 'Memories sent to the model',
    history: 'Earlier exchanges included', tools: 'Tools used', none: 'None', excluded: 'Memories left out',
    status: 'Status', reasonRelevance: 'matched your words', reasonComplete: 'all memories fit, so all were sent',
    reasonPinned: 'pinned', model: 'Model requests', running: 'running', events: 'events',
    every: 'every', left: 'left', hours: 'h', earlier: 'an earlier exchange',
    invalid: 'invalid', disputed_or_expired: 'disputed or expired', irrelevant: 'unrelated to the question',
    token_budget: 'over the context budget',
    pending: 'pending', unverifiedStatus: 'answered', cancelled: 'cancelled', missed: 'missed',
    active: 'active', exhausted: 'finished', waiting: 'waiting',
    provider_disabled: 'waiting for a model', runtime_busy: 'waiting for another run', due_time: 'waiting for its time',
    scheduler_stopped: 'scheduler stopped', dispatch: 'starting',
    deadline_exceeded: 'took too long', model_failure: 'the model failed', driver_contract: 'unsupported model response',
    protocol: 'invalid model response', bound_exceeded: 'response too large', context_compilation: 'memory could not be read',
    capability_unavailable: 'a tool is unavailable', capability_contract: 'a tool call was invalid', invalid_input: 'invalid request',
    'memory.search': 'Searching memories…', 'web.fetch': 'Reading the linked page…',
    'artifact.read': 'Reading the attachment…', 'artifact.sort': 'Sorting the attachment…',
    'memory.remember': 'Saving to memory…', 'memory.forget': 'Forgetting a memory…', 'web.search': 'Searching the web…',
    remembered: 'Remembered', updatedMemory: 'Updated a memory', forgotMemory: 'Forgot a memory',
    byDitto: 'by Ditto', byDittoHint: 'Ditto inferred this from your conversation',
  },
  ko: {
    newThread: '새 대화', memories: '기억', schedules: '예약',
    memoryPlaceholder: '기억할 내용…', save: '저장', cancel: '취소', loadMore: '더 보기',
    schedulePlaceholder: '나중에 할 일은?', startAt: '시작 시각', window: '시작 허용 범위',
    repeat: '반복', once: '한 번', daily: '매일', weekly: '매주', times: '횟수', scheduleIt: '예약하기',
    conversation: '대화', emptyTitle: '무엇이든 물어보세요. 알려주신 것은 기억합니다.',
    emptyHint: '왼쪽에서 기억을 저장하거나 /remember 뒤에 내용을 입력하세요.',
    inputPlaceholder: 'Ditto에게 메시지…',
    whyTitle: '이 답변의 근거', online: '연결됨', offline: '재연결 중…', unverified: '미검증',
    unverifiedHint: '모델 답변은 검증된 작업 완료가 아닙니다.',
    why: '근거', correct: '수정', forget: '지우기', forgetConfirm: '정말 지울까요?', newDivider: '새 대화',
    noMemories: '아직 기억이 없습니다.', noSchedules: '예약된 일이 없습니다.',
    disabled: '모델이 설정되지 않았습니다. 데몬을 --provider openai-compatible(예: 로컬 Ollama 모델) 또는 --provider openai로 시작하세요.',
    busy: '이미 요청 네 개를 처리하고 있습니다. 잠시 후 다시 시도하세요.', stop: '중단',
    failed: '답변 없음', interrupted: '중단됨', usedMemories: '모델에 보낸 기억',
    history: '포함된 이전 대화', tools: '사용한 도구', none: '없음', excluded: '제외된 기억',
    status: '상태', reasonRelevance: '질문 단어와 일치', reasonComplete: '기억 전체가 예산에 들어가 모두 전송',
    reasonPinned: '고정', model: '모델 요청 수', running: '실행 중', events: '이벤트',
    every: '주기', left: '남음', hours: '시간', earlier: '이전 대화',
    invalid: '유효하지 않음', disputed_or_expired: '이의 제기 또는 만료', irrelevant: '질문과 무관',
    token_budget: '문맥 예산 초과',
    pending: '대기', unverifiedStatus: '답변함', cancelled: '취소됨', missed: '놓침',
    active: '진행 중', exhausted: '완료', waiting: '대기 중',
    provider_disabled: '모델 설정 대기', runtime_busy: '다른 실행 대기', due_time: '시각 대기',
    scheduler_stopped: '스케줄러 중지', dispatch: '시작 중',
    deadline_exceeded: '시간 초과', model_failure: '모델 오류', driver_contract: '지원하지 않는 모델 응답',
    protocol: '잘못된 모델 응답', bound_exceeded: '응답이 너무 큼', context_compilation: '기억을 읽지 못함',
    capability_unavailable: '도구를 사용할 수 없음', capability_contract: '잘못된 도구 호출', invalid_input: '잘못된 요청',
    'memory.search': '기억을 찾는 중…', 'web.fetch': '링크한 페이지를 읽는 중…',
    'artifact.read': '첨부를 읽는 중…', 'artifact.sort': '첨부를 정렬하는 중…',
    'memory.remember': '기억하는 중…', 'memory.forget': '기억을 지우는 중…', 'web.search': '웹을 검색하는 중…',
    remembered: '기억함', updatedMemory: '기억을 고침', forgotMemory: '기억을 지움',
    byDitto: 'Ditto', byDittoHint: '대화에서 Ditto가 추론한 기억',
  },
};
const LANG = (navigator.language || 'en').toLowerCase().startsWith('ko') ? 'ko' : 'en';
const t = (key) => STRINGS[LANG][key] || STRINGS.en[key] || key;

// ---- identity ------------------------------------------------------------
const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
function ulid() {
  let time = Date.now();
  let head = '';
  for (let i = 0; i < 10; i += 1) {
    head = CROCKFORD[time % 32] + head;
    time = Math.floor(time / 32);
  }
  let tail = '';
  for (const byte of crypto.getRandomValues(new Uint8Array(16))) tail += CROCKFORD[byte % 32];
  return head + tail;
}

// ---- HTTP -----------------------------------------------------------------
class ApiError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}
async function api(method, path, body) {
  const response = await fetch(path, {
    method,
    headers: body ? { 'content-type': 'application/json' } : {},
    body: body ? JSON.stringify(body) : undefined,
  });
  const text = await response.text();
  let value = null;
  try { value = text ? JSON.parse(text) : null; } catch (_) { value = null; }
  if (!response.ok) throw new ApiError(response.status, (value && value.error) || response.statusText);
  return value;
}
const query = (params) => new URLSearchParams(params).toString();

// ---- rendering --------------------------------------------------------------
function escapeHtml(text) {
  return text.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
}
// Escape first, then a closed subset: fenced and inline code, headings,
// lists, bold, italics and links. Code is held aside so nothing inside it is
// reformatted.
function markdown(text) {
  const held = [];
  const hold = (html) => `\u0000${held.push(html) - 1}\u0000`;
  const source = escapeHtml(text.replace(/\u0000/g, ''))
    .replace(/```[\w-]*\n?([\s\S]*?)```/g, (_, code) => `\n${hold(`<pre><code>${code.replace(/\n$/, '')}</code></pre>`)}\n`)
    .replace(/`([^`\n]+)`/g, (_, code) => hold(`<code>${code}</code>`));
  const inline = (line) => line
    .replace(/\*\*([^*\n]+)\*\*/g, '<strong>$1</strong>')
    .replace(/(^|[\s(])\*([^*\s][^*\n]*)\*/g, '$1<em>$2</em>')
    .replace(/\bhttps?:\/\/[^\s<)]*[^\s<).,;:!?]/g, (url) => `<a href="${url}" target="_blank" rel="noopener noreferrer">${url}</a>`);
  let html = '';
  let paragraph = [];
  let list = null;
  const flush = () => {
    if (paragraph.length) html += `<p>${paragraph.map(inline).join('<br>')}</p>`;
    if (list) html += `<${list.tag}>${list.items.map((item) => `<li>${inline(item)}</li>`).join('')}</${list.tag}>`;
    paragraph = [];
    list = null;
  };
  const item = (tag, content) => {
    if (paragraph.length || (list && list.tag !== tag)) flush();
    if (!list) list = { tag, items: [] };
    list.items.push(content);
  };
  for (const line of source.split('\n')) {
    let match;
    if (!line.trim()) flush();
    else if (/^\u0000\d+\u0000$/.test(line.trim()) && held[Number(line.trim().slice(1, -1))].startsWith('<pre>')) {
      flush();
      html += line.trim();
    } else if ((match = /^#{1,4}\s+(.+)$/.exec(line))) {
      flush();
      html += `<h4>${inline(match[1])}</h4>`;
    } else if ((match = /^\s*[-*•]\s+(.+)$/.exec(line))) item('ul', match[1]);
    else if ((match = /^\s*\d+[.)]\s+(.+)$/.exec(line))) item('ol', match[1]);
    else {
      if (list) flush();
      paragraph.push(line);
    }
  }
  flush();
  return html.replace(/\u0000(\d+)\u0000/g, (_, index) => held[Number(index)] || '');
}

const messages = $('messages');
const bubbles = new Map(); // turn id -> assistant bubble
const pendingByRequest = new Map(); // request id -> bubble before its turn id is known
const userTextByTurn = new Map(); // turn id -> user text, for the inspector

function scrollToEnd() {
  messages.scrollTop = messages.scrollHeight;
}
function addUser(text) {
  $('empty').classList.add('hidden');
  const node = document.createElement('div');
  node.className = 'msg user';
  node.textContent = text;
  messages.appendChild(node);
  scrollToEnd();
  return node;
}
function addAssistant(state) {
  const node = document.createElement('div');
  node.className = 'msg assistant';
  node.innerHTML = '<div class="body typing"></div><div class="foot"></div>';
  messages.appendChild(node);
  const bubble = { node, body: node.querySelector('.body'), foot: node.querySelector('.foot'), text: '', request: -1, done: false, ...state };
  // A running answer can be stopped on its own; the others keep going.
  if (bubble.requestId) {
    const stop = document.createElement('button');
    stop.className = 'link stop';
    stop.textContent = t('stop');
    stop.addEventListener('click', () => cancel(bubble));
    bubble.foot.appendChild(stop);
  }
  scrollToEnd();
  return bubble;
}
function addExchange(user, state) {
  const asked = addUser(user);
  const bubble = addAssistant({ ...state, user, asked });
  if (state.turnId) bind(bubble, state.turnId);
  return bubble;
}
// What a model request wrote before its tool call stays as a message of its
// own; the bubble goes on with the next step (ADR 0035).
function settleSaid(bubble) {
  if (bubble.text.trim()) {
    const node = document.createElement('div');
    node.className = 'msg assistant said';
    node.innerHTML = `<div class="body">${markdown(bubble.text)}</div>`;
    messages.insertBefore(node, bubble.node);
    bubble.body.innerHTML = '';
    bubble.body.classList.add('typing');
  }
  bubble.text = '';
}
// An acknowledgment starts no answer: a reaction on it says it was read.
function acknowledge(bubble) {
  if (bubble.done) return;
  bubble.done = true;
  bubble.node.remove();
  const reaction = document.createElement('span');
  reaction.className = 'reaction';
  reaction.textContent = '👍';
  bubble.asked.appendChild(reaction);
}
function failureText(failure) {
  const code = failure.code || '';
  const label = code === 'cancelled' ? t('interrupted') : t('failed');
  const detail = STRINGS.en[code] ? t(code) : failure.message || code;
  return `${label}: ${detail}`;
}
function finishBubble(bubble, text, failure) {
  bubble.done = true;
  bubble.body.classList.remove('typing', 'progress');
  if (failure) {
    bubble.node.classList.add('failed');
    bubble.body.textContent = failureText(failure);
    if (failure.message) bubble.body.title = failure.message;
  } else {
    bubble.body.innerHTML = markdown(text);
  }
  bubble.foot.innerHTML = '';
  if (!failure) {
    const tag = document.createElement('span');
    tag.className = 'tag unverified';
    tag.textContent = t('unverified');
    tag.title = t('unverifiedHint');
    bubble.foot.appendChild(tag);
  }
  if (bubble.tool) {
    const tool = document.createElement('span');
    tool.className = 'tag tool';
    tool.textContent = bubble.tool;
    bubble.foot.appendChild(tool);
  }
  if (bubble.taskId) {
    const why = document.createElement('button');
    why.className = 'link';
    why.textContent = t('why');
    why.addEventListener('click', () => inspect(bubble.taskId).catch((error) => banner(error.message)));
    bubble.foot.appendChild(why);
  }
  scrollToEnd();
}
// What Ditto remembered or forgot during the answer (ADR 0031), kept apart
// from the answer text so finishing the answer leaves it in place.
function noticesFor(bubble) {
  if (!bubble.notices) {
    bubble.notices = document.createElement('div');
    bubble.notices.className = 'notices';
    bubble.node.insertBefore(bubble.notices, bubble.foot);
  }
  return bubble.notices;
}
function divider() {
  const node = document.createElement('div');
  node.className = 'divider';
  node.textContent = t('newDivider');
  messages.appendChild(node);
  scrollToEnd();
}
function banner(text) {
  const node = $('banner');
  node.textContent = text || '';
  node.classList.toggle('hidden', !text);
}

// ---- conversation -----------------------------------------------------------
let latestSeq = 0;

// Render finished exchanges of the current thread that are not on screen yet.
async function syncThread() {
  const thread = await api('GET', `/v1/conversation?${query({ session_id: SESSION })}`);
  for (const exchange of thread.exchanges) {
    if (bubbles.has(exchange.turn_id)) continue;
    const bubble = addExchange(exchange.user, { taskId: exchange.task_id, turnId: exchange.turn_id });
    finishBubble(bubble, exchange.assistant);
  }
  return thread.through_seq;
}

async function send(text) {
  if (text === '/new') return newThread();
  if (text.startsWith('/remember ')) return remember(text.slice('/remember '.length).trim());
  banner('');
  const requestId = ulid();
  const bubble = addExchange(text, { requestId, taskId: `run_${requestId}` });
  pendingByRequest.set(requestId, bubble);
  try {
    const accepted = await api('POST', '/v1/commands/run', { request_id: requestId, session_id: SESSION, text });
    bind(bubble, accepted.turn_id);
    settleFromStatus(bubble, accepted);
  } catch (error) {
    pendingByRequest.delete(requestId);
    const message = error.status === 503 ? t('disabled') : error.status === 429 ? t('busy') : error.message;
    finishBubble(bubble, '', { code: 'rejected', message });
    if (error.status === 503) banner(t('disabled'));
  }
}

function bind(bubble, turnId) {
  if (!turnId || bubbles.get(turnId) === bubble) return;
  bubble.turnId = turnId;
  bubbles.set(turnId, bubble);
  if (bubble.user) userTextByTurn.set(turnId, bubble.user);
  if (bubble.requestId) pendingByRequest.delete(bubble.requestId);
}

function settleFromStatus(bubble, status) {
  if (bubble.done || status.status === 'running') return;
  if (status.status === 'acknowledged') acknowledge(bubble);
  else if (status.status === 'unverified') finishBubble(bubble, status.response || '');
  else finishBubble(bubble, '', { code: status.failure_code || status.status, message: status.failure_code || status.status });
}

async function cancel(bubble) {
  try {
    await api('POST', '/v1/commands/run/cancel', { request_id: bubble.requestId, session_id: SESSION });
  } catch (error) {
    banner(error.message);
  }
}

async function newThread() {
  await api('POST', '/v1/commands/conversation/reset', { session_id: SESSION });
}

// ---- live events ------------------------------------------------------------
function onEvent(event) {
  if (event.seq <= latestSeq) return;
  latestSeq = event.seq;
  const payload = event.payload || {};
  switch (event.kind) {
    case 'input.received': {
      const run = payload.agent_run;
      if (!run || !event.correlation_id) return;
      let bubble = pendingByRequest.get(run.request_id);
      if (bubble) {
        bind(bubble, event.correlation_id);
      } else if (!bubbles.has(event.correlation_id)) {
        // A run started elsewhere (CLI, schedule, messaging) appears live.
        bubble = addExchange(payload.text, { requestId: run.request_id, taskId: event.task_id, turnId: event.correlation_id });
      }
      if (bubble && run.acknowledged) acknowledge(bubble);
      return;
    }
    case 'model.output': {
      const bubble = bubbles.get(payload.turn_id);
      const model = payload.stream_event && payload.stream_event.event;
      if (!bubble || bubble.done || !model) return;
      if (payload.request_index !== bubble.request || model.type === 'tool_call_started') {
        bubble.request = payload.request_index;
        settleSaid(bubble);
      }
      if (model.type === 'text_delta') {
        bubble.text += model.text;
        bubble.body.classList.remove('typing', 'progress');
        bubble.body.innerHTML = markdown(bubble.text);
        scrollToEnd();
      } else if (model.type === 'tool_call_started') {
        bubble.tool = model.capability_id;
        // Say what the tool is doing until the model writes again.
        if (!bubble.text) {
          bubble.body.classList.remove('typing');
          bubble.body.classList.add('progress');
          bubble.body.textContent = t(model.capability_id);
        }
      }
      return;
    }
    case 'turn.finished': {
      const bubble = bubbles.get(payload.turn_id);
      if (!bubble) syncThread().catch(() => {});
      else if (!bubble.done) finishBubble(bubble, payload.outcome.response);
      return;
    }
    case 'turn.failed': {
      const bubble = bubbles.get(payload.turn_id);
      if (bubble && !bubble.done) finishBubble(bubble, '', payload.failure);
      return;
    }
    case 'agent.memory_write.requested': {
      const bubble = bubbles.get(payload.turn_id);
      if (bubble && payload.write) (bubble.writes = bubble.writes || new Map()).set(payload.call_id, payload.write);
      return;
    }
    case 'agent.memory_write.output': {
      const bubble = bubbles.get(payload.turn_id);
      const write = bubble && bubble.writes && bubble.writes.get(payload.call_id);
      if (!write || payload.result.outcome === 'refused') return;
      const line = document.createElement('div');
      line.textContent = payload.result.outcome === 'forgotten'
        ? t('forgotMemory')
        : `${t(write.replaces ? 'updatedMemory' : 'remembered')}: ${write.text}`;
      noticesFor(bubble).appendChild(line);
      scrollToEnd();
      return;
    }
    case 'conversation.reset':
      divider();
      return;
    case 'context.node.recorded':
      refreshMemories().catch(() => {});
      return;
    default:
      if (event.kind.startsWith('schedule.')) refreshSchedules();
  }
}

const STREAM_KINDS = ['input.received', 'model.output', 'turn.finished', 'turn.failed', 'conversation.reset',
  'context.node.recorded', 'agent.memory_write.requested', 'agent.memory_write.output', 'schedule.requested', 'schedule.claimed', 'schedule.cancelled', 'schedule.expired',
  'schedule.repeat.requested', 'schedule.repeat.cancelled', 'schedule.repeat.claimed', 'schedule.repeat.skipped'];

function connect() {
  const source = new EventSource(`/v1/stream?${query({ session_id: SESSION, after_seq: latestSeq })}`);
  for (const kind of STREAM_KINDS) source.addEventListener(kind, (message) => onEvent(JSON.parse(message.data)));
  source.onopen = () => setConnection(true);
  source.onerror = () => {
    setConnection(false);
    source.close();
    // Resume after the last applied sequence; the daemon replays the gap.
    setTimeout(connect, 1500);
  };
}
function setConnection(online) {
  const badge = $('connection');
  badge.dataset.state = online ? 'online' : 'offline';
  badge.textContent = online ? t('online') : t('offline');
}

// ---- memories ---------------------------------------------------------------
let memoryCursor = null;
async function refreshMemories(append = false) {
  const params = { session_id: SESSION, limit: 100 };
  if (append && memoryCursor) params.after_id = memoryCursor;
  const page = await api('GET', `/v1/memories?${query(params)}`);
  const list = $('memory-list');
  if (!append) list.innerHTML = '';
  for (const memory of page.memories) {
    const item = document.createElement('li');
    item.textContent = memory.text;
    const meta = document.createElement('div');
    meta.className = 'meta';
    if (memory.inferred) {
      const tag = document.createElement('span');
      tag.className = 'tag tool';
      tag.textContent = t('byDitto');
      tag.title = t('byDittoHint');
      meta.appendChild(tag);
    }
    const fix = document.createElement('button');
    fix.className = 'link';
    fix.textContent = t('correct');
    fix.addEventListener('click', () => {
      $('memory-text').value = memory.text;
      $('memory-replaces').value = memory.id;
      $('memory-cancel').classList.remove('hidden');
      $('memory-text').focus();
    });
    meta.appendChild(fix);
    // A second click within a few seconds forgets it (ADR 0032); no dialog.
    const forget = document.createElement('button');
    forget.className = 'link';
    forget.textContent = t('forget');
    forget.addEventListener('click', () => {
      if (!forget.dataset.armed) {
        forget.dataset.armed = 'yes';
        forget.textContent = t('forgetConfirm');
        setTimeout(() => { delete forget.dataset.armed; forget.textContent = t('forget'); }, 4000);
        return;
      }
      forgetMemory(memory.id).catch((error) => {
        banner(error.message);
        refreshMemories().catch(() => {});
      });
    });
    meta.appendChild(forget);
    item.appendChild(meta);
    list.appendChild(item);
  }
  if (!list.children.length) {
    const empty = document.createElement('li');
    empty.className = 'muted';
    empty.textContent = t('noMemories');
    list.appendChild(empty);
  }
  memoryCursor = page.next_after_id;
  $('memory-more').classList.toggle('hidden', !memoryCursor);
}
async function forgetMemory(id) {
  await api('POST', '/v1/commands/memory/forget', { session_id: SESSION, memory_id: id });
  banner('');
  await refreshMemories();
}
async function remember(text, replaces) {
  if (!text) return;
  const input = await api('POST', '/v1/commands/input', { text, session_id: SESSION });
  const command = { session_id: SESSION, input_event_id: input.event.event_id };
  if (replaces) command.replaces = replaces;
  await api('POST', '/v1/commands/memory', command);
  banner('');
  await refreshMemories();
}

// ---- schedules --------------------------------------------------------------
async function refreshSchedules() {
  const [pending, repeats] = await Promise.all([
    api('GET', `/v1/schedules/pending?${query({ session_id: SESSION })}`).catch(() => []),
    api('GET', `/v1/repeats/active?${query({ session_id: SESSION })}`).catch(() => []),
  ]);
  const list = $('schedule-list');
  list.innerHTML = '';
  const render = (entry, kind) => {
    const item = document.createElement('li');
    const when = new Date(entry.next_due_at || entry.due_at);
    const status = entry.status === 'unverified' ? t('unverifiedStatus') : t(entry.status);
    item.textContent = `${when.toLocaleString()} · ${status}`;
    const meta = document.createElement('div');
    meta.className = 'meta';
    if (entry.waiting_for) meta.append(t(entry.waiting_for));
    if (kind === 'repeat') {
      const left = entry.occurrences - entry.claimed_occurrences - entry.missed_occurrences;
      meta.append(`${t('every')} ${Math.round(entry.every_seconds / 3600)}${t('hours')} · ${left} ${t('left')}`);
    }
    const cancel = document.createElement('button');
    cancel.className = 'link';
    cancel.textContent = t('cancel');
    cancel.addEventListener('click', async () => {
      const path = kind === 'repeat' ? '/v1/commands/repeat/cancel' : '/v1/commands/schedule/cancel';
      await api('POST', path, { request_id: entry.request_id, session_id: SESSION }).catch((error) => banner(error.message));
      refreshSchedules();
    });
    meta.appendChild(cancel);
    item.appendChild(meta);
    list.appendChild(item);
  };
  (pending || []).forEach((entry) => render(entry, 'schedule'));
  (repeats || []).forEach((entry) => render(entry, 'repeat'));
  if (!list.children.length) {
    const empty = document.createElement('li');
    empty.className = 'muted';
    empty.textContent = t('noSchedules');
    list.appendChild(empty);
  }
}
async function createSchedule() {
  const text = $('schedule-text').value.trim();
  const at = new Date($('schedule-at').value);
  if (!text || Number.isNaN(at.getTime())) return;
  const window = Number($('schedule-window').value);
  const every = Number($('schedule-repeat').value);
  const due = new Date(Math.ceil(at.getTime() / 1000) * 1000);
  const body = {
    request_id: ulid(), session_id: SESSION, text,
    due_at: due.toISOString(), expires_at: new Date(due.getTime() + Math.min(window, every || window) * 1000).toISOString(),
  };
  try {
    if (every) {
      await api('POST', '/v1/commands/repeat', { ...body, every_seconds: every, occurrences: Number($('schedule-count').value) });
    } else {
      await api('POST', '/v1/commands/schedule', body);
    }
    $('schedule-text').value = '';
    banner('');
  } catch (error) {
    banner(error.message);
  }
  refreshSchedules();
}

// ---- inspector ----------------------------------------------------------------
async function taskEvents(taskId) {
  const events = [];
  let after = 0;
  for (let page = 0; page < 20; page += 1) {
    const batch = await api('GET', `/v1/events?${query({ session_id: SESSION, task_id: taskId, after_seq: after, limit: 1000 })}`);
    events.push(...batch);
    if (batch.length < 1000) break;
    after = batch[batch.length - 1].seq;
  }
  return events;
}

async function inspect(taskId) {
  const events = await taskEvents(taskId);
  const body = $('inspector-body');
  body.innerHTML = '';
  const section = (title, items) => {
    const node = document.createElement('section');
    const heading = document.createElement('h4');
    heading.textContent = title;
    node.appendChild(heading);
    if (!items.length) {
      const none = document.createElement('div');
      none.className = 'muted';
      none.textContent = t('none');
      node.appendChild(none);
    }
    for (const [text, why] of items) {
      const item = document.createElement('div');
      item.className = 'item';
      item.textContent = text;
      if (why) {
        const reason = document.createElement('div');
        reason.className = 'why';
        reason.textContent = why;
        item.appendChild(reason);
      }
      node.appendChild(item);
    }
    body.appendChild(node);
  };
  const context = events.find((event) => event.kind === 'context.compiled');
  if (context) {
    const receipt = context.payload.compiled.receipt;
    const reasons = new Map(receipt.included.map((entry) => [entry.node_id, entry.reason]));
    const label = (reason) => ({ 'task-relevance': t('reasonRelevance'), 'complete-set': t('reasonComplete'), 'user-pinned': t('reasonPinned') })[reason] || reason;
    // Version 7 records the compiled nodes only; the capsule derives from them.
    const used = (context.payload.capsule || context.payload.compiled).nodes;
    section(t('usedMemories'), used.map((node) => [node.summary, label(reasons.get(node.id))]));
    const excluded = new Map();
    for (const entry of receipt.excluded) excluded.set(entry.reason, (excluded.get(entry.reason) || 0) + 1);
    section(t('excluded'), [...excluded].map(([reason, count]) => [`${t(reason)}: ${count}`]));
    section(t('history'), (context.payload.history_turn_ids || []).map((turn) => [userTextByTurn.get(turn) || t('earlier')]));
  }
  const toolKinds = { 'capability.requested': null, 'agent.fetch.requested': 'web.fetch', 'agent.sort.requested': 'artifact.sort', 'agent.memory.requested': 'memory.search', 'agent.memory_write.requested': null, 'agent.search.requested': 'web.search' };
  const tools = events
    .filter((event) => event.kind in toolKinds)
    .map((event) => [toolKinds[event.kind] || event.payload.capability_id, JSON.stringify(event.payload.arguments)]);
  section(t('tools'), tools);
  const requests = events.filter((event) => event.kind === 'model.requested').length;
  const terminal = events.find((event) => event.kind === 'turn.finished' || event.kind === 'turn.failed');
  const state = !terminal ? t('running') : terminal.kind === 'turn.finished' ? t('unverified') : failureText(terminal.payload.failure);
  section(t('status'), [[`${t('model')}: ${requests}`], [state], [`${events.length} ${t('events')}`]]);
  document.querySelector('.app').classList.add('inspecting');
  $('inspector').classList.remove('hidden');
}

// ---- wiring -----------------------------------------------------------------
function localize() {
  document.documentElement.lang = LANG;
  for (const node of document.querySelectorAll('[data-i18n]')) node.textContent = t(node.dataset.i18n);
  for (const node of document.querySelectorAll('[data-i18n-placeholder]')) node.placeholder = t(node.dataset.i18nPlaceholder);
}

function wire() {
  $('session-name').textContent = SESSION;
  const input = $('input');
  const autosize = () => {
    input.style.height = 'auto';
    input.style.height = `${Math.min(input.scrollHeight, 220)}px`;
  };
  input.addEventListener('input', autosize);
  input.addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
      event.preventDefault();
      $('composer').requestSubmit();
    }
  });
  $('composer').addEventListener('submit', (event) => {
    event.preventDefault();
    const text = input.value.trim();
    if (!text) return;
    input.value = '';
    autosize();
    send(text).catch((error) => banner(error.message));
  });
  $('new-thread').addEventListener('click', () => newThread().catch((error) => banner(error.message)));
  $('memory-form').addEventListener('submit', (event) => {
    event.preventDefault();
    const text = $('memory-text').value.trim();
    const replaces = $('memory-replaces').value || undefined;
    remember(text, replaces)
      .then(() => {
        $('memory-text').value = '';
        $('memory-replaces').value = '';
        $('memory-cancel').classList.add('hidden');
      })
      .catch((error) => banner(error.message));
  });
  $('memory-cancel').addEventListener('click', () => {
    $('memory-text').value = '';
    $('memory-replaces').value = '';
    $('memory-cancel').classList.add('hidden');
  });
  $('memory-more').addEventListener('click', () => refreshMemories(true).catch((error) => banner(error.message)));
  $('schedule-form').addEventListener('submit', (event) => {
    event.preventDefault();
    createSchedule();
  });
  $('schedule-repeat').addEventListener('change', () => {
    $('schedule-count-field').classList.toggle('hidden', $('schedule-repeat').value === '0');
  });
  const soon = new Date(Date.now() + 10 * 60 * 1000);
  soon.setSeconds(0, 0);
  $('schedule-at').value = new Date(soon.getTime() - soon.getTimezoneOffset() * 60000).toISOString().slice(0, 16);
  for (const tab of document.querySelectorAll('.tab')) {
    tab.addEventListener('click', () => {
      for (const other of document.querySelectorAll('.tab')) other.classList.toggle('active', other === tab);
      $('panel-memories').classList.toggle('hidden', tab.dataset.panel !== 'memories');
      $('panel-schedules').classList.toggle('hidden', tab.dataset.panel !== 'schedules');
      if (tab.dataset.panel === 'schedules') refreshSchedules();
    });
  }
  $('toggle-sidebar').addEventListener('click', () => $('sidebar').classList.toggle('open'));
  $('inspector-close').addEventListener('click', () => {
    $('inspector').classList.add('hidden');
    document.querySelector('.app').classList.remove('inspecting');
  });
}

async function health() {
  try {
    const status = await api('GET', '/health');
    $('status').textContent = `Ditto v${status.version}`;
  } catch (_) {
    $('status').textContent = t('offline');
  }
}

async function main() {
  localize();
  wire();
  setConnection(false);
  // Follow events after the rendered thread, or after the current end if the
  // thread could not be read, so older history is never replayed as new.
  const [through] = await Promise.all([
    syncThread().catch(async (error) => {
      banner(error.message);
      const status = await api('GET', '/health').catch(() => null);
      return status ? status.latest_seq : 0;
    }),
    refreshMemories().catch(() => {}),
    health(),
  ]);
  latestSeq = through;
  connect();
  $('input').focus();
}

main();
