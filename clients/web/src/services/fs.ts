/**
 * fs.ts — Filesystem browsing for the workdir picker + explorer.
 *
 * Direct gRPC-Web calls (the FsList/FsRead RPCs): replies pair natively
 * over HTTP. Failures (transport or inline) resolve with `error` set so
 * every surface renders
 * them inline on the unified toast stack instead of rejecting.
 *
 * Provides: listDir, readFile
 * Depends: core/grpc.ts
 */
import { DISCONNECTED_ERROR, grpcListDir, grpcReadFile } from '../core/grpc';
import type { FsContent, FsListing } from '../core/types';

export { DISCONNECTED_ERROR };

/** True when a listing/read error is the outage marker (not a real
 * filesystem failure) — surfaces suppress their toasts for it. */
export function isDisconnect(error: string | undefined): boolean {
  return error === DISCONNECTED_ERROR;
}

/**
 * List one directory (directories first). `path` omitted = the server's
 * default start dir ($HOME, falling back to its cwd). Never rejects —
 * browse failures resolve with `error` set so the picker renders them
 * inline.
 */
export function listDir(path?: string): Promise<FsListing> {
  return grpcListDir(path);
}

/**
 * Read a bounded head of one file for the preview pane. Never rejects —
 * failures resolve with `error` set.
 */
export function readFile(path: string): Promise<FsContent> {
  return grpcReadFile(path);
}
