/**
 * fake-provider.mjs — scripted OpenAI-compatible SSE provider for the
 * headless UI check (clients/web/e2e).
 *
 * Speaks just enough of the wire format for flux-provider's SSE client:
 *   Round 1  reasoning + a full prose sample + tool_calls (bash echo
 *            success + bash `exit 3` failure) → the kernel executes the
 *            REAL bash tools and issues a follow-up request.
 *   Round 2  closing text → the round wraps with stream_end.
 * Each round ends with a usage chunk + [DONE].
 *
 * Usage: node fake-provider.mjs <port>
 */
import http from 'node:http';

const port = Number(process.argv[2] ?? 9999);
let round = 0;

const chunk = (delta, finish) =>
  `data: ${JSON.stringify({
    id: 'chatcmpl-fake',
    object: 'chat.completion.chunk',
    choices: [{ index: 0, delta, finish_reason: finish ?? null }],
  })}\n\n`;

const usageChunk =
  `data: ${JSON.stringify({
    id: 'chatcmpl-fake',
    object: 'chat.completion.chunk',
    choices: [],
    usage: { prompt_tokens: 120, completion_tokens: 40, prompt_tokens_details: { cached_tokens: 8 } },
  })}\n\n`;

http
  .createServer((req, res) => {
    // Model-catalog endpoint — the Providers dialog's probe target (the
    // same catalog the pickers read from the store cache). One entry
    // carries the non-standard context_length extension on purpose.
    if (req.url?.includes('/models')) {
      req.resume();
      req.on('end', () => {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(
          JSON.stringify({
            object: 'list',
            data: [
              { id: 'ui-check-model' },
              { id: 'ui-check-mini', context_length: 8192 },
            ],
          }),
        );
      });
      return;
    }
    if (!req.url?.includes('/chat/completions')) {
      res.writeHead(404).end();
      return;
    }
    // Drain the request body (the kernel's message history) — not inspected.
    req.resume();
    req.on('end', () => {
      round += 1;
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      if (round === 1) {
        res.write(chunk({ reasoning_content: 'The user wants a tool demo. I will run echo.' }));
        // The text carries PATHOLOGICAL content on purpose: an unbreakable
        // token and a wide table. Their min-content must not widen the
        // shell — the mobile no-horizontal-overflow check below pins it
        // (min-w-0 on the app root + overflow-wrap anywhere on prose).
        res.write(
          chunk({
            content:
              'Let me run a quick command.\n\n' +
              // PROSE REVIEW SURFACE: every Shared-Markdown-Prose voice in one
              // reply — inline code, a fenced block (badge + copy chrome),
              // blockquote, hr, a list — then the pathological pair below.
              'The build runs through `npm run build` — see the **fenced sample**.\n\n' +
              'unbreakable-' +
              'x'.repeat(300) +
              '\n\n' +
              '```bash\n' +
              'flux serve --port 8080  # single port: UI + Connect + terminal\n' +
              'flux serve --help       # every flag, documented\n' +
              '```\n\n' +
              '> Quote voice: recessed fill, strong left rule, `code` stays legible.\n\n' +
              '---\n\n' +
              '- first item carries `inline` code\n' +
              '- second item carries a [link](https://flux.dev)\n\n' +
              '| c1 | c2 | c3 | c4 | c5 | c6 | c7 | c8 | c9 | c10 | c11 | c12 |\n' +
              '|---|---|---|---|---|---|---|---|---|---|---|---|\n' +
              '| aaaaaaaaaaaaaaaaaaaaaa | bbbbbbbbbbbbbbbbbbbbbb | cccccccccccccccccccccc | dddddddddddddddddddddd | eeeeeeeeeeeeeeeeeeeeee | ffffffffffffffffffffff | gggggggggggggggggggggg | hhhhhhhhhhhhhhhhhhhhhh | iiiiiiiiiiiiiiiiiiiiii | jjjjjjjjjjjjjjjjjjjjjj | kkkkkkkkkkkkkkkkkkkkkk | llllllllllllllllllllll |',
          }),
        );
        res.write(
          chunk({
            tool_calls: [
              {
                index: 0,
                id: 'call_demo_1',
                function: { name: 'bash', arguments: '{"command": "echo hello-flux"}' },
              },
              {
                // A FAILING command: no output, non-zero exit → the kernel
                // reports "(exit code: 3)" and the card paints the exit
                // verdict (warn status voice) — the abnormal-completion
                // ladder gets reviewed in the same shots.
                index: 1,
                id: 'call_demo_2',
                function: { name: 'bash', arguments: '{"command": "exit 3"}' },
              },
            ],
          }),
        );
        res.write(chunk({}, 'tool_calls'));
      } else {
        res.write(chunk({ content: 'The tool ran fine — hello-flux was printed.' }));
        res.write(chunk({}, 'stop'));
      }
      res.write(usageChunk);
      res.write('data: [DONE]\n\n');
      res.end();
    });
  })
  .listen(port, '127.0.0.1', () => console.log(`fake provider on :${port}`));
