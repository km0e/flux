/**
 * grpc.test.ts — the Connect client's frontend contract proofs.
 *
 * Golden-bytes: the TS-generated messages must encode to exactly the bytes
 * prost encodes (proto3 canonical) — the cross-language schema agreement
 * the whole protocol reset rests on, pinned by one fixture per message.
 * (The Rust side pins the same bytes in flux-server's tests.)
 *
 * Call layer: the fs RPCs through a mocked fetch carrying real gRPC-Web
 * framing — inline errors, disconnect markers, and the proto→wire mappers.
 */
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { toBinary as toBinaryMsg, fromBinary, create } from '@bufbuild/protobuf';

import {
  FsListRequestSchema,
  FsListResponseSchema,
  FsEntrySchema,
  FsEntryKind,
  GitStatus,
} from '../../gen/flux/v1/fs_pb';
import { SubscribeResponseSchema } from '../../gen/flux/v1/events_pb';
import { ListProvidersResponseSchema } from '../../gen/flux/v1/providers_pb';
import { ListModelsResponseSchema } from '../../gen/flux/v1/models_pb';
import { ModelSummarySchema } from '../../gen/flux/v1/common_pb';
import {
  mapEntryKind,
  mapGitStatus,
  protoListingToListing,
  grpcListDir,
  grpcFetchProviders,
  grpcFetchModels,
  DISCONNECTED_ERROR,
} from '../../core/grpc';
import { useFlux, resetFluxForTest } from '../../core/state';

/** Assemble a gRPC-Web response body: data frame + trailer frame. */
function grpcWebBody(messageBytes: Uint8Array, status = 0): Uint8Array {
  const frame = (flag: number, payload: Uint8Array) => {
    const out = new Uint8Array(5 + payload.length);
    out[0] = flag;
    new DataView(out.buffer).setUint32(1, payload.length);
    out.set(payload, 5);
    return out;
  };
  const data = frame(0, messageBytes);
  const trailers = new TextEncoder().encode(`grpc-status:${status}\r\n`);
  const tail = frame(0x80, trailers);
  const body = new Uint8Array(data.length + tail.length);
  body.set(data, 0);
  body.set(tail, data.length);
  return body;
}

/** Mock globalThis.fetch to answer one gRPC-Web body; records the URLs.
 * Returns the restore closure. */
function mockFetch(body: Uint8Array, urls: string[]): () => void {
  const realFetch = globalThis.fetch;
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    urls.push(typeof input === 'string' ? input : input.toString());
    return new Response(body as unknown as BodyInit, {
      status: 200,
      headers: { 'content-type': 'application/grpc-web+proto' },
    });
  }) as typeof fetch;
  return () => {
    globalThis.fetch = realFetch;
  };
}

describe('grpc golden bytes (proto3 canonical — matches prost)', () => {
  it('encodes FsListRequest {path: "x"} as 0a 01 78', () => {
    const bytes = toBinaryMsg(FsListRequestSchema, create(FsListRequestSchema, { path: 'x' }));
    expect(Array.from(bytes)).toEqual([0x0a, 0x01, 0x78]);
  });

  it('encodes FsListRequest {} (default start dir) as empty', () => {
    const bytes = toBinaryMsg(FsListRequestSchema, create(FsListRequestSchema, {}));
    expect(bytes.byteLength).toBe(0);
  });

  it('round-trips an FsListResponse with entries', () => {
    // prost's Rust-side encoding of the same fixture (field numbers + enum
    // values are the contract):
    //   requested(1)="p"; entries(5){name(1)="a.txt", kind(2)=FILE=2,
    //   size(3)=5}; path(3) omitted, error(2) omitted.
    const rustBytes = Uint8Array.from([
      0x0a, 0x01, 0x70, // requested = "p"
      0x2a, 0x0b, // entries: len 11
      0x0a, 0x05, 0x61, 0x2e, 0x74, 0x78, 0x74, // name = "a.txt"
      0x10, 0x02, // kind = FILE
      0x18, 0x05, // size = 5
    ]);
    const resp = fromBinary(FsListResponseSchema, rustBytes);
    expect(resp.requested).toBe('p');
    expect(resp.entries).toHaveLength(1);
    expect(resp.entries[0].name).toBe('a.txt');
    expect(resp.entries[0].kind).toBe(FsEntryKind.FILE);
    expect(resp.entries[0].size).toBe(5n);
  });

  it('round-trips a text_delta SubscribeResponse', () => {
    // chat_seq(1)=0 implicit; chat_id(2)="c"; text_delta(4){delta(1)="hi"}.
    const rustBytes = Uint8Array.from([
      0x12, 0x01, 0x63, // chat_id = "c"
      0x22, 0x04, // text_delta: len 4
      0x0a, 0x02, 0x68, 0x69, // delta = "hi"
    ]);
    const response = fromBinary(SubscribeResponseSchema, rustBytes);
    expect(response.chatId).toBe('c');
    expect(response.kind.case).toBe('textDelta');
    if (response.kind.case === 'textDelta' && response.kind.value) {
      expect(response.kind.value.delta).toBe('hi');
    }
  });
});

describe('grpc mappers (proto → wire shape)', () => {
  it('maps entry kinds', () => {
    expect(mapEntryKind(FsEntryKind.DIR)).toBe('dir');
    expect(mapEntryKind(FsEntryKind.FILE)).toBe('file');
    expect(mapEntryKind(FsEntryKind.UNSPECIFIED)).toBe('file');
  });

  it('maps git statuses with an undefined passthrough for UNSPECIFIED', () => {
    expect(mapGitStatus(GitStatus.MODIFIED)).toBe('modified');
    expect(mapGitStatus(GitStatus.ADDED)).toBe('added');
    expect(mapGitStatus(GitStatus.UNTRACKED)).toBe('untracked');
    expect(mapGitStatus(GitStatus.CONFLICTED)).toBe('conflicted');
    expect(mapGitStatus(GitStatus.UNSPECIFIED)).toBeUndefined();
  });

  it('maps a proto listing onto the UI-shaped FsListing', () => {
    const listing = protoListingToListing({
      requested: '',
      error: undefined,
      path: '/tmp/proj',
      parent: '/tmp',
      entries: [
        { name: 'src', kind: FsEntryKind.DIR, size: undefined, git: GitStatus.MODIFIED },
        { name: 'main.rs', kind: FsEntryKind.FILE, size: 100n, git: GitStatus.UNSPECIFIED },
      ],
    });
    // The exact shape services/fs.listDir's callers consume.
    expect(listing).toEqual({
      type: 'fs_listing',
      requested: '',
      path: '/tmp/proj',
      parent: '/tmp',
      entries: [
        { name: 'src', kind: 'dir', git: 'modified' },
        { name: 'main.rs', kind: 'file', size: 100 },
      ],
    });
  });

  it('keeps inline errors as picker data (never a thrown transport fault)', () => {
    const listing = protoListingToListing({
      requested: '/gone',
      error: 'No such file or directory',
      path: undefined,
      parent: undefined,
      entries: [],
    });
    expect(listing.error).toBe('No such file or directory');
    expect(listing.entries).toEqual([]);
  });
});

describe('grpc fs calls over the real wire framing', () => {
  beforeEach(() => {
    resetFluxForTest();
    useFlux.setState({ connectionStatus: 'connected' });
  });

  it('grpcListDir rides /flux.v1.FileSystemService/FsList and maps the reply', async () => {
    const respBytes = toBinaryMsg(
      FsListResponseSchema,
      create(FsListResponseSchema, {
        requested: '/proj',
        path: '/proj',
        entries: [create(FsEntrySchema, { name: 'src', kind: FsEntryKind.DIR })],
      }),
    );
    const urls: string[] = [];
    const restore = mockFetch(grpcWebBody(respBytes), urls);
    try {
      const r = await grpcListDir('/proj');
      expect(urls[0]).toContain('/flux.v1.FileSystemService/FsList');
      expect(r.error).toBeUndefined();
      expect(r.entries).toEqual([{ name: 'src', kind: 'dir' }]);
    } finally {
      restore();
    }
  });

  it('a transport failure while DISCONNECTED resolves with the outage marker', async () => {
    useFlux.setState({ connectionStatus: 'disconnected' });
    const restore = mockFetch(new Uint8Array(0), []); // wrong body → decode/transport failure
    try {
      const r = await grpcListDir('/proj');
      expect(r.error).toBe(DISCONNECTED_ERROR);
    } finally {
      restore();
    }
  });

  it('a transport failure while connected resolves with a per-call message', async () => {
    const restore = mockFetch(new Uint8Array(0), []);
    try {
      const r = await grpcListDir('/proj');
      expect(r.error).toBe('listing failed');
      expect(r.error).not.toBe(DISCONNECTED_ERROR);
    } finally {
      restore();
    }
  });
});

describe('grpc management fetches publish into the store', () => {
  beforeEach(() => {
    resetFluxForTest();
    useFlux.setState({ connectionStatus: 'connected' });
  });

  it('grpcFetchProviders publishes the registry', async () => {
    const respBytes = toBinaryMsg(
      ListProvidersResponseSchema,
      create(ListProvidersResponseSchema, {
        providers: [
          { id: 'p1', url: 'http://a/v1' },
          { id: 'p2', url: 'http://b/v1' },
        ],
      }),
    );
    const urls: string[] = [];
    const restore = mockFetch(grpcWebBody(respBytes), urls);
    try {
      await grpcFetchProviders();
      const providers = useFlux.getState().providers;
      expect(providers.map((p) => p.id)).toEqual(['p1', 'p2']);
      expect(providers[0]).toEqual({ id: 'p1', url: 'http://a/v1' });
      expect(urls[0]).toContain('/flux.v1.ProviderService/ListProviders');
    } finally {
      restore();
    }
  });

  it('grpcFetchModels publishes the saved rows', async () => {
    const respBytes = toBinaryMsg(
      ListModelsResponseSchema,
      create(ListModelsResponseSchema, {
        models: [
          create(ModelSummarySchema, {
            provider: 'p1',
            model: 'big',
            paramsJson: '{"context_length":128000}',
            metaJson: '{"name":"Big"}',
          }),
        ],
      }),
    );
    const urls: string[] = [];
    const restore = mockFetch(grpcWebBody(respBytes), urls);
    try {
      await grpcFetchModels();
      const saved = useFlux.getState().savedModels;
      expect(saved).toHaveLength(1);
      expect(saved[0]).toEqual({
        provider: 'p1',
        model: 'big',
        params: { context_length: 128000 },
        meta: { name: 'Big' },
      });
      expect(urls[0]).toContain('/flux.v1.ModelService/ListModels');
    } finally {
      restore();
    }
  });

  it('unary calls carry the session token header when attached', async () => {
    sessionStorage.setItem('flux.session.id', 'tok-1');
    const respBytes = toBinaryMsg(ListProvidersResponseSchema, create(ListProvidersResponseSchema, {}));
    let seen: string | null = null;
    const realFetch = globalThis.fetch;
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      seen = new Headers(init?.headers).get('x-flux-session');
      void input;
      return new Response(grpcWebBody(respBytes) as unknown as BodyInit, {
        status: 200,
        headers: { 'content-type': 'application/grpc-web+proto' },
      });
    }) as typeof fetch;
    try {
      await grpcFetchProviders();
      expect(seen).toBe('tok-1');
    } finally {
      globalThis.fetch = realFetch;
      sessionStorage.removeItem('flux.session.id');
    }
  });

  it('vi stub sanity (no-op without network)', () => {
    expect(vi).toBeDefined();
  });
});
