/**
 * models.ts — LOCAL saved-model registry client.
 *
 * Direct gRPC-Web calls (the ModelService RPCs): replies pair natively
 * over HTTP. The broadcast/reply routing that lands the authoritative
 * list in the
 * store's `savedModels` stays (the `models` stream element routes through
 * handlers.ts into handleModelsFrame).
 *
 * Provides: fetchModels, saveModel, removeModel, syncModels,
 *           handleModelsFrame, findSavedModel, savedModelParams,
 *           effectiveContextLength
 * Depends: core/grpc.ts, core/state.ts
 */
import { grpcFetchModels, grpcRemoveModel, grpcSaveModel, grpcSyncModels } from '../core/grpc';
import { log } from '../logger';
import { useFlux } from '../core/state';
import type { ModelParams, SavedModelInfo } from '../core/types';

/** Fetch the LOCAL saved-model list. Preloaded once per session attach
 * (handlers.session_resumed — every reconnect re-pulls); dialogs and
 * pickers only read the store. */
export function fetchModels(): void {
  // Fire-and-forget (see providers.ts) — logged, never unhandled.
  grpcFetchModels().catch((e) => log.warn('model fetch failed', e));
}

/** Create or edit one saved model (upsert). Resolves with the inline
 * error (undefined = success; the fresh list arrives via the `models`
 * broadcast). A CREATE is auto-enriched server-side from models.dev. */
export function saveModel(
  provider: string,
  model: string,
  params: ModelParams,
): Promise<{ enriched?: boolean; error?: string }> {
  return grpcSaveModel(provider, model, params);
}

/** Remove one saved model. Resolves with the inline error, or undefined. */
export function removeModel(provider: string, model: string): Promise<string | undefined> {
  return grpcRemoveModel(provider, model);
}

/** Re-sync saved models against models.dev (metadata only — user params
 * are preserved). Scope: no args = ALL rows; provider alone = its rows;
 * both = one row. */
export function syncModels(
  provider?: string,
  model?: string,
): Promise<{ updated?: number; error?: string }> {
  return grpcSyncModels(provider, model);
}

/** Reply routing (called by handlers.ts): the authoritative list replaces
 * the store's `savedModels`. */
export function handleModelsFrame(models: SavedModelInfo[]): void {
  useFlux.setState({ savedModels: models });
}

/** The saved row for a (provider, model) pin, if any. */
export function findSavedModel(provider: string, model: string): SavedModelInfo | undefined {
  return useFlux
    .getState()
    .savedModels.find((m) => m.provider === provider && m.model === model);
}

/** The editable params of a saved model ({} when unsaved). */
export function savedModelParams(provider: string, model: string): ModelParams {
  return findSavedModel(provider, model)?.params ?? {};
}

/**
 * Effective context length for a (provider, model) pin — the ContextMeter's
 * single lookup. Saved-model params win (the user's deployment truth), the
 * probed catalog is the fallback; undefined = unknown (meter hidden).
 */
export function effectiveContextLength(
  provider: string,
  model: string,
): number | undefined {
  const saved = findSavedModel(provider, model)?.params.context_length;
  if (saved) return saved;
  return useFlux
    .getState()
    .providerModels[provider]?.find((m) => m.id === model)?.context_length;
}
