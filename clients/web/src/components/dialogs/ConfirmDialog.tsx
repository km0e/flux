/**
 * ConfirmDialog.tsx — destructive-action confirmation (chat delete).
 *
 * Provides: ConfirmDialog
 */
import { Button } from '../ui';
import { Dialog, DialogContent, DialogTitle } from '../ui/dialog';

export function ConfirmDialog(props: {
  title: string;
  message: string;
  confirmLabel: string;
  onConfirm: () => void;
  onCancel: () => void;
}): React.ReactElement {
  return (
    <Dialog open onOpenChange={(open) => !open && props.onCancel()}>
      <DialogContent className="w-[min(94vw,480px)]">
        <DialogTitle>{props.title}</DialogTitle>
        <p className="text-sm leading-relaxed text-muted">{props.message}</p>
        <div className="mt-4 flex justify-end gap-2">
          <Button variant="secondary" onClick={props.onCancel}>
            Cancel
          </Button>
          <Button variant="danger" onClick={props.onConfirm}>
            {props.confirmLabel}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
