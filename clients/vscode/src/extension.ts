import * as vscode from 'vscode';
import { FluxServer } from './server';

let outputChannel: vscode.OutputChannel | undefined;
let server: FluxServer | undefined;
let extensionContext: vscode.ExtensionContext | undefined;

type ChatMessage = { role: 'user' | 'assistant' | 'error'; content: string };
const HISTORY_KEY = 'flux.chatHistory';

export function activate(context: vscode.ExtensionContext): void {
  extensionContext = context;
  outputChannel = vscode.window.createOutputChannel('Flux');
  context.subscriptions.push(outputChannel);

  context.subscriptions.push(
    vscode.commands.registerCommand('flux.openChat', () => {
      FluxChatPanel.createOrShow(context.extensionUri);
    })
  );
}

export function deactivate(): void {
  server?.stop();
}

class FluxChatPanel {
  public static currentPanel: FluxChatPanel | undefined;
  private readonly panel: vscode.WebviewPanel;
  private readonly extensionUri: vscode.Uri;
  private disposables: vscode.Disposable[] = [];

  public static createOrShow(extensionUri: vscode.Uri): void {
    const column = vscode.window.activeTextEditor?.viewColumn ?? vscode.ViewColumn.One;

    if (FluxChatPanel.currentPanel) {
      FluxChatPanel.currentPanel.panel.reveal(column, true);
      return;
    }

    const panel = vscode.window.createWebviewPanel(
      'flux.chat',
      'Flux Chat',
      { viewColumn: column, preserveFocus: false },
      {
        enableScripts: true,
        localResourceRoots: [extensionUri],
        retainContextWhenHidden: true,
      }
    );

    FluxChatPanel.currentPanel = new FluxChatPanel(panel, extensionUri);
  }

  private constructor(panel: vscode.WebviewPanel, extensionUri: vscode.Uri) {
    this.panel = panel;
    this.extensionUri = extensionUri;

    this.panel.webview.html = this.getHtml();

    const history = this.loadHistory();
    if (history.length > 0) {
      this.post({ type: 'restore', messages: history });
    }

    this.panel.webview.onDidReceiveMessage(
      async (message) => {
        if (message.type === 'send') {
          await this.sendMessage(message.text);
        }
      },
      null,
      this.disposables
    );

    this.panel.onDidDispose(
      () => {
        this.dispose();
      },
      null,
      this.disposables
    );

    void this.startServer();
  }

  private post(message: unknown): void {
    void this.panel.webview.postMessage(message);
  }

  private loadHistory(): ChatMessage[] {
    return extensionContext?.workspaceState.get<ChatMessage[]>(HISTORY_KEY) || [];
  }

  private async saveHistory(history: ChatMessage[]): Promise<void> {
    await extensionContext?.workspaceState.update(HISTORY_KEY, history);
  }

  private async addHistory(role: ChatMessage['role'], content: string): Promise<void> {
    const history = this.loadHistory();
    history.push({ role, content });
    await this.saveHistory(history);
  }

  private async startServer(): Promise<void> {
    const channel = outputChannel;
    if (!channel) {
      return;
    }

    try {
      if (!server) {
        const config = vscode.workspace.getConfiguration('flux');
        const serverPath = config.get<string>('serverPath') || '';
        const serverMode = config.get<'stdio' | 'tcp'>('serverMode') || 'stdio';
        const serverHost = config.get<string>('serverHost') || '127.0.0.1';
        const serverPort = config.get<number>('serverPort') || 8080;
        server = new FluxServer({ mode: serverMode, serverPath, host: serverHost, port: serverPort });
        server.on('log', (line) => channel.appendLine(line));
        server.on('error', (err) => {
          channel.appendLine(`Error: ${err.message || err}`);
          this.post({ type: 'error', text: String(err.message || err) });
        });
        server.on('close', () => {
          channel.appendLine('Server connection closed');
          this.post({ type: 'error', text: 'Server disconnected. Click send to reconnect.' });
          server = undefined;
        });
        await server.start();

        // Provider/API configuration is handled by the server-side config file
        // (flux.toml). The extension only forwards optional MCP server configs.
        const mcpServers = config.get<import('./server').McpServerConfig[]>('mcpServers') || [];
        const initParams: import('./server').InitializeParams = {
          mcpServers: mcpServers.length > 0 ? mcpServers : undefined,
        };

        await server.initialize(initParams);
      }

      this.attachServerListeners();
      this.post({ type: 'status', text: 'Server ready' });
    } catch (err) {
      server = undefined;
      this.post({ type: 'error', text: `Failed to initialize Flux: ${err}` });
    }
  }

  private attachServerListeners(): void {
    if (!server) { return; }

    const onChunk = (params: { stream_id?: string; delta?: string }) => {
      this.post({ type: 'stream-chunk', id: params.stream_id || '', delta: params.delta || '' });
    };
    const onDone = (params: { stream_id?: string; content?: string }) => {
      const content = params.content || '';
      this.post({ type: 'stream-done', id: params.stream_id || '', content });
      void this.addHistory('assistant', content);
    };
    const onError = (params: { stream_id?: string; message?: string }) => {
      this.post({ type: 'stream-error', id: params.stream_id || '', text: params.message || 'Unknown streaming error' });
    };

    server.on('stream/chunk', onChunk);
    server.on('stream/done', onDone);
    server.on('stream/error', onError);

    this.disposables.push(
      new vscode.Disposable(() => {
        server?.off('stream/chunk', onChunk);
        server?.off('stream/done', onDone);
        server?.off('stream/error', onError);
      })
    );
  }

  private async sendMessage(text: string): Promise<void> {
    if (!server) {
      this.post({ type: 'error', text: 'Server not ready' });
      return;
    }

    this.post({ type: 'message', role: 'user', text });
    await this.addHistory('user', text);

    const streamId = Math.random().toString(36).slice(2);
    this.post({ type: 'stream-start', id: streamId });

    const history = this.loadHistory().slice(0, -1);

    try {
      await server.request('chat/stream', { message: text, stream_id: streamId, history });
    } catch (err) {
      this.post({ type: 'stream-error', id: streamId, text: String(err) });
    }
  }

  public dispose(): void {
    FluxChatPanel.currentPanel = undefined;
    this.panel.dispose();
    for (const d of this.disposables) {
      d.dispose();
    }
    this.disposables = [];
  }

  private getHtml(): string {
    const scriptUri = this.panel.webview.asWebviewUri(
      vscode.Uri.joinPath(this.extensionUri, 'media', 'chat.js')
    );
    const purifyUri = this.panel.webview.asWebviewUri(
      vscode.Uri.joinPath(this.extensionUri, 'media', 'purify.min.js')
    );
    const markedUri = this.panel.webview.asWebviewUri(
      vscode.Uri.joinPath(this.extensionUri, 'media', 'marked.umd.js')
    );
    return /* html */ `
      <!DOCTYPE html>
      <html lang="en">
      <head>
        <meta charset="UTF-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1.0" />
        <style>
          body { font-family: var(--vscode-font-family); color: var(--vscode-foreground); margin: 0; padding: 10px; display: flex; flex-direction: column; height: 100vh; box-sizing: border-box; }
          #messages { flex: 1; overflow-y: auto; display: flex; flex-direction: column; gap: 8px; margin-bottom: 10px; }
          .message { padding: 8px 10px; border-radius: 6px; max-width: 90%; word-break: break-word; }
          .user { align-self: flex-end; background: var(--vscode-button-background); color: var(--vscode-button-foreground); }
          .assistant { align-self: flex-start; background: var(--vscode-editor-inactiveSelectionBackground); }
          .error { align-self: flex-start; color: var(--vscode-errorForeground); }
          .status { align-self: center; font-size: 0.85em; opacity: 0.7; }
          .assistant strong { font-weight: bold; }
          .assistant em { font-style: italic; }
          .assistant code { font-family: var(--vscode-editor-font-family); background: var(--vscode-textCodeBlock-background); padding: 2px 4px; border-radius: 3px; }
          .assistant pre { background: var(--vscode-textCodeBlock-background); padding: 8px; border-radius: 4px; overflow-x: auto; }
          .assistant pre code { background: transparent; padding: 0; }
          .assistant h1, .assistant h2, .assistant h3, .assistant h4, .assistant h5, .assistant h6 { margin: 8px 0 4px; font-weight: bold; }
          .assistant h1 { font-size: 1.3em; }
          .assistant h2 { font-size: 1.2em; }
          .assistant h3 { font-size: 1.1em; }
          .assistant p { margin: 4px 0; }
          .assistant ul, .assistant ol { margin: 4px 0; padding-left: 20px; }
          .assistant li { margin: 2px 0; }
          .assistant blockquote { margin: 4px 0; padding-left: 10px; border-left: 3px solid var(--vscode-foreground); opacity: 0.8; }
          .assistant table { border-collapse: collapse; margin: 4px 0; }
          .assistant th, .assistant td { border: 1px solid var(--vscode-input-border); padding: 4px 8px; }
          .assistant th { background: var(--vscode-editor-inactiveSelectionBackground); }
          .assistant hr { border: none; border-top: 1px solid var(--vscode-input-border); margin: 8px 0; }
          #input-row { display: flex; gap: 8px; }
          #input { flex: 1; background: var(--vscode-input-background); color: var(--vscode-input-foreground); border: 1px solid var(--vscode-input-border); border-radius: 4px; padding: 6px; }
          #send { background: var(--vscode-button-background); color: var(--vscode-button-foreground); border: none; border-radius: 4px; padding: 6px 12px; cursor: pointer; }
        </style>
      </head>
      <body>
        <div id="messages"></div>
        <div id="input-row">
          <input id="input" type="text" placeholder="Ask Flux..." />
          <button id="send">Send</button>
        </div>
        <script src="${purifyUri}"></script>
        <script src="${markedUri}"></script>
        <script src="${scriptUri}"></script>
      </body>
      </html>
    `;
  }
}
