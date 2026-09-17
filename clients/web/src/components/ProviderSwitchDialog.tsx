/**
 * ProviderSwitchDialog — the conversation's provider/model switcher.
 *
 * Shows the chat's current pin, lets the operator pick another registry
 * provider and/or model; Apply sends `chat_provider` (the swap applies at
 * the round boundary and `provider_switched` updates the chip). Lives
 * next to the composer it acts on (the composer's bottom-left chip) — the
 * top bar stays an identity/status row, not a control surface.
 *
 * Provides: ProviderSwitchDialog
 * Depends: services/providers.ts, components/ProviderPicker.tsx,
 *          components/ui/*
 */
import { useState } from 'react';
import { switchProvider } from '../services/providers';
import { Button } from './ui';
import { Dialog, DialogContent, DialogTitle } from './ui/dialog';
import { ProviderPicker } from './ProviderPicker';

export function ProviderSwitchDialog(props: {
  chatId: string;
  provider: string;
  model: string;
  onClose: () => void;
}): React.ReactElement {
  const [providerId, setProviderId] = useState(props.provider);
  const [model, setModel] = useState(props.model);
  return (
    <Dialog open onOpenChange={(open) => !open && props.onClose()}>
      <DialogContent className="w-[min(94vw,440px)]">
        <DialogTitle>Switch provider</DialogTitle>
        <div className="flex flex-col gap-3">
          <ProviderPicker
            providerId={providerId}
            model={model}
            onChange={(next) => {
              setProviderId(next.provider);
              setModel(next.model);
            }}
          />
          <p className="text-xs leading-relaxed text-muted">
            The switch applies when the current round ends — a running round is never interrupted.
          </p>
          <div className="flex justify-end gap-2">
            <Button variant="secondary" onClick={props.onClose}>
              Cancel
            </Button>
            <Button
              variant="primary"
              disabled={!providerId || !model.trim()}
              onClick={() => {
                switchProvider(props.chatId, providerId, model.trim());
                props.onClose();
              }}
            >
              Switch
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
