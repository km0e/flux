/**
 * providers.ts — provider discovery, management + hot-swap client.
 *
 * Direct gRPC-Web calls (the ProviderService RPCs): replies pair natively
 * over HTTP — the old value-matched AckWait correlation is gone. The
 * probe publishes into the store through the SAME handler the broadcast
 * frames use, so the pickers keep reading the cache and never probe ad
 * hoc. Inline errors ride the promise; the fresh-registry broadcast
 * arrives on the stream (the `providers` frame handler stays
 * authoritative for the store).
 *
 * Provides: fetchProviders, probeProvider, switchProvider,
 *           addProvider, removeProvider, handleProviderModelsReply
 * Depends: core/grpc.ts, core/state.ts, logger.ts
 */
import { grpcAddProvider, grpcFetchProviders, grpcProbeProvider, grpcRemoveProvider } from '../core/grpc';
import { bridge } from '../core/bridge';
import { useFlux } from '../core/state';
import { log } from '../logger';
import type { ProviderModelInfo } from '../core/types';

/** Fetch the registry summary (provider ids + effective urls — entries
 * are pure endpoints; the api_key never leaves the server). */
export function fetchProviders(): void {
  // Fire-and-forget: a failed refresh leaves the store cache as-is (the
  // next mount refetches) — the rejection is logged, never unhandled.
  grpcFetchProviders().catch((e) => log.warn('provider fetch failed', e));
}

/** Probe one provider's upstream model catalog, keeping the in-band
 * error visible (the Providers dialog's Test button). Saving a provider
 * never validates connectivity — this probe is the explicit check. */
export async function probeProvider(
  providerId: string,
): Promise<{ models: ProviderModelInfo[]; error?: string }> {
  const r = await grpcProbeProvider(providerId);
  handleProviderModelsReply(providerId, r.models, r.error);
  return r;
}

/** The catalog lands in the store cache (the ONLY source the pickers
 * read — they never probe ad hoc); the probe error rides
 * `providerProbeErrors` so the Providers dialog can render it with a
 * retry. (Also the reply routing for the probe path above.) */
export function handleProviderModelsReply(
  provider: string,
  models: ProviderModelInfo[],
  error?: string,
): void {
  useFlux.setState((s) => {
    const probeErrors = { ...s.providerProbeErrors };
    if (error) probeErrors[provider] = error;
    else delete probeErrors[provider];
    return {
      providerModels: { ...s.providerModels, [provider]: models },
      providerProbeErrors: probeErrors,
    };
  });
  if (error) log.warn(`provider_models [${provider}] probe failed: ${error}`);
}

/** Register a provider (the server persists it to the database FIRST —
 * the ack implies durability). Resolves with the inline error, or
 * undefined on success (the fresh list arrives via the `providers`
 * broadcast). Saving never validates connectivity — test explicitly with
 * probeProvider. */
export function addProvider(input: {
  id: string;
  url?: string;
  api_key?: string;
}): Promise<string | undefined> {
  return grpcAddProvider(input);
}

/** Remove a registered provider. Resolves with the inline error, or
 * undefined on success. */
export function removeProvider(id: string): Promise<string | undefined> {
  return grpcRemoveProvider(id);
}

/** Hot-swap the conversation's provider (lease holder only). The swap
 * applies at the round boundary; `provider_switched` announces the apply.
 * `model` is required — the wire carries no default to imply. */
export function switchProvider(chatId: string, providerId: string, model: string): void {
  bridge.send({
    type: 'chat_provider',
    chat_id: chatId,
    provider: providerId,
    model,
  });
}
