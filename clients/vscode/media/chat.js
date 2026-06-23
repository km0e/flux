(function () {
  const vscode = acquireVsCodeApi();
  const messages = document.getElementById('messages');
  const input = document.getElementById('input');
  const sendBtn = document.getElementById('send');

  function scrollToBottom() {
    messages.scrollTop = messages.scrollHeight;
  }

  function append(text, role) {
    const div = document.createElement('div');
    div.className = 'message ' + role;
    div.textContent = text;
    messages.appendChild(div);
    scrollToBottom();
  }

  function appendHtml(html, role) {
    const div = document.createElement('div');
    div.className = 'message ' + role;
    div.innerHTML = html;
    messages.appendChild(div);
    scrollToBottom();
  }

  function send() {
    const text = input.value.trim();
    if (!text) return;
    input.value = '';
    vscode.postMessage({ type: 'send', text });
  }

  sendBtn.addEventListener('click', send);
  input.addEventListener('keydown', (e) => { if (e.key === 'Enter') send(); });

  const streams = {};

  function escapeHtml(text) {
    return text
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;');
  }

  function renderMarkdown(text) {
    try {
      const raw = marked.parse(text, {
        gfm: true,
        breaks: true,
        headerIds: false,
        mangle: false,
      });
      return DOMPurify.sanitize(raw, { USE_PROFILES: { html: true } });
    } catch (err) {
      return escapeHtml(text);
    }
  }

  function ensureStream(id, role) {
    if (!streams[id]) {
      const div = document.createElement('div');
      div.className = 'message ' + role;
      div.id = id;
      div.dataset.raw = '';
      messages.appendChild(div);
      streams[id] = div;
      scrollToBottom();
    }
    return streams[id];
  }

  window.addEventListener('message', (event) => {
    const msg = event.data;
    try {
      if (msg.type === 'message') append(msg.text, msg.role);
      else if (msg.type === 'restore') {
        messages.innerHTML = '';
        for (const m of msg.messages || []) {
          if (m.role === 'assistant') {
            appendHtml(renderMarkdown(m.content), 'assistant');
          } else if (m.role === 'user') {
            append(m.content, 'user');
          } else if (m.role === 'error') {
            append(m.content, 'error');
          }
        }
      }
      else if (msg.type === 'stream-start') ensureStream(msg.id, 'assistant');
      else if (msg.type === 'stream-chunk') {
        const el = ensureStream(msg.id, 'assistant');
        el.dataset.raw += msg.delta;
        el.textContent = el.dataset.raw;
        scrollToBottom();
      }
      else if (msg.type === 'stream-done') {
        const el = streams[msg.id];
        if (el) {
          if (msg.content !== undefined) { el.dataset.raw = msg.content; }
          el.innerHTML = renderMarkdown(el.dataset.raw);
        }
        delete streams[msg.id];
      }
      else if (msg.type === 'stream-error') {
        const el = ensureStream(msg.id, 'assistant');
        el.classList.add('error');
        el.textContent = msg.text;
        delete streams[msg.id];
      }
      else if (msg.type === 'error') append(msg.text, 'error');
      else if (msg.type === 'status') append(msg.text, 'status');
    } catch (err) {
      // Swallow rendering errors to avoid breaking the chat UI.
    }
  });
})();
