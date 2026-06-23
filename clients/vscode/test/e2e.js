const { spawn } = require('child_process');
const path = require('path');

const SERVER = path.join(__dirname, '..', '..', '..', 'target', 'release', 'flux-server');

function request(proc, id, method, params) {
  return new Promise((resolve) => {
    const line = JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n';
    const onData = (chunk) => {
      const lines = chunk.toString().split('\n').filter(Boolean);
      for (const l of lines) {
        try {
          const resp = JSON.parse(l);
          if (resp.id === id) {
            proc.stdout.off('data', onData);
            resolve(resp);
            return;
          }
        } catch {
          // ignore non-JSON lines
        }
      }
    };
    proc.stdout.on('data', onData);
    proc.stdin.write(line);
  });
}

async function main() {
  const proc = spawn(SERVER, ['stdio'], { stdio: ['pipe', 'pipe', 'inherit'] });

  try {
    // Initialize with a fake OpenAI provider so the agent builds without a real key.
    // No network call happens until chat/stream is invoked.
    const init = await request(proc, 1, 'initialize', {
      provider: {
        name: 'openai',
        model: 'gpt-4o-mini',
        apiKey: 'sk-fake-test-key',
      },
    });
    console.log('initialize:', JSON.stringify(init));
    if (init.error) {
      throw new Error(`Initialize failed: ${init.error.message}`);
    }

    const tools = await request(proc, 2, 'tools/list', {});
    console.log('tools/list:', JSON.stringify(tools));
    if (tools.error) {
      throw new Error(`tools/list failed: ${tools.error.message}`);
    }
    if (!Array.isArray(tools.result) || tools.result.length === 0) {
      throw new Error('Expected non-empty tool list');
    }

    const apiKey = process.env.FLUX_TEST_API_KEY;
    if (!apiKey) {
      console.log('Skipping chat test: set FLUX_TEST_API_KEY to exercise the LLM path.');
    } else {
      const chat = await request(proc, 3, 'chat', { message: 'hello flux' });
      console.log('chat:', JSON.stringify(chat));
      if (chat.error) {
        throw new Error(`chat failed: ${chat.error.message}`);
      }
      if (!chat.result || !chat.result.content || typeof chat.result.content !== 'string') {
        throw new Error('Expected chat response content');
      }
    }

    console.log('E2E test passed');
  } finally {
    proc.kill();
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
