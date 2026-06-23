import { ChildProcessWithoutNullStreams, spawn } from 'child_process';
import * as net from 'net';
import * as path from 'path';
import { EventEmitter } from 'events';
import * as vscode from 'vscode';

export interface JsonRpcRequest<T = unknown> {
  jsonrpc: '2.0';
  id: number;
  method: string;
  params?: T;
}

export interface JsonRpcResponse<T = unknown> {
  jsonrpc: '2.0';
  id?: number;
  result?: T;
  error?: { code: number; message: string; data?: unknown };
}

export interface ProviderParams {
  name: 'openai' | 'anthropic';
  model?: string;
  baseUrl?: string;
  apiKey?: string;
}

export interface McpServerConfig {
  command: string;
  args?: string[];
  env?: Record<string, string>;
}

export interface InitializeParams {
  provider?: ProviderParams;
  workdir?: string;
  mcpServers?: McpServerConfig[];
}

export interface ServerConfig {
  mode: 'stdio' | 'tcp';
  serverPath?: string;
  host?: string;
  port?: number;
}

export class FluxServer extends EventEmitter {
  private process?: ChildProcessWithoutNullStreams;
  private socket?: net.Socket;
  private requestId = 0;
  private pending = new Map<number, { resolve: (value: any) => void; reject: (reason?: any) => void }>();
  private buffer = '';

  constructor(private readonly config: ServerConfig) {
    super();
  }

  async start(): Promise<void> {
    if (this.process || this.socket) {
      return;
    }

    if (this.config.mode === 'tcp') {
      const host = this.config.host || '127.0.0.1';
      const port = this.config.port || 8080;
      this.socket = net.createConnection(port, host);

      this.socket.on('data', (chunk: Buffer) => this.handleData(chunk.toString()));
      this.socket.on('error', (err) => this.emit('error', err));
      this.socket.on('close', () => this.emit('close'));

      return new Promise<void>((resolve, reject) => {
        this.socket!.once('connect', () => {
          this.emit('log', `[tcp] connected to ${host}:${port}`);
          resolve();
        });
        this.socket!.once('error', reject);
      });
    }

    const binary = this.resolveBinary();
    const cwd = this.resolveCwd();
    this.process = spawn(binary, ['stdio'], {
      cwd,
      stdio: ['pipe', 'pipe', 'pipe'],
    });

    this.process.stdout.on('data', (chunk: Buffer) => this.handleData(chunk.toString()));
    this.process.stderr.on('data', (chunk: Buffer) => {
      const line = chunk.toString().trim();
      if (line) {
        this.emit('log', `[server stderr] ${line}`);
      }
    });

    this.process.on('error', (err) => this.emit('error', err));
    this.process.on('close', (code) => this.emit('close', code));
  }

  async initialize(params?: InitializeParams): Promise<void> {
    if (!this.process && !this.socket) {
      throw new Error('Server not started');
    }
    await this.request('initialize', params ?? {});
    this.emit('ready');
  }

  stop(): void {
    this.process?.kill();
    this.process = undefined;
    this.socket?.destroy();
    this.socket = undefined;
    this.pending.forEach((p) => p.reject(new Error('Server stopped')));
    this.pending.clear();
  }

  async request<T = unknown>(method: string, params: unknown): Promise<T> {
    if (!this.process && !this.socket) {
      throw new Error('Server not started');
    }

    const id = ++this.requestId;
    const req: JsonRpcRequest = { jsonrpc: '2.0', id, method, params };
    const line = JSON.stringify(req) + '\n';

    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      const writeCb = (err: Error | null | undefined) => {
        if (err) {
          this.pending.delete(id);
          reject(err);
        }
      };

      if (this.socket) {
        this.socket.write(line, writeCb);
      } else {
        this.process!.stdin.write(line, writeCb);
      }
    });
  }

  private resolveBinary(): string {
    if (this.config.serverPath) {
      return this.config.serverPath;
    }

    const root = this.resolveWorkspaceRoot();
    const binary = path.join(root, 'target', 'release', 'flux-server');
    return process.platform === 'win32' ? `${binary}.exe` : binary;
  }

  private resolveCwd(): string | undefined {
    if (this.config.serverPath) {
      return undefined;
    }
    return this.resolveWorkspaceRoot();
  }

  private resolveWorkspaceRoot(): string {
    const workspaceFolders = vscode.workspace.workspaceFolders;
    if (!workspaceFolders || workspaceFolders.length === 0) {
      throw new Error('No workspace folder open and flux.serverPath is not set');
    }
    return workspaceFolders[0].uri.fsPath;
  }

  private handleData(data: string): void {
    this.buffer += data;
    let newlineIndex: number;
    while ((newlineIndex = this.buffer.indexOf('\n')) >= 0) {
      const line = this.buffer.slice(0, newlineIndex).trim();
      this.buffer = this.buffer.slice(newlineIndex + 1);
      if (line) {
        this.handleLine(line);
      }
    }
  }

  private handleLine(line: string): void {
    try {
      const resp = JSON.parse(line);
      if (resp.id !== undefined && this.pending.has(resp.id)) {
        const { resolve, reject } = this.pending.get(resp.id)!;
        this.pending.delete(resp.id);
        if (resp.error) {
          reject(new Error(resp.error.message));
        } else {
          resolve(resp.result);
        }
      } else if (resp.method) {
        this.emit(resp.method, resp.params);
      } else {
        this.emit('notification', resp);
      }
    } catch (err) {
      this.emit('log', `[parse error] ${line}`);
    }
  }
}
