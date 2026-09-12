/**
 * ui/dialog.tsx — Radix Dialog wrapper (shadcn conventions).
 *
 * Radix owns focus trap, Esc, outside-click and aria wiring; this file owns
 * the styling. Used by the confirm + new-chat dialogs.
 *
 * Provides: Dialog, DialogTrigger, DialogContent, DialogTitle, DialogClose
 */
import * as DialogPrimitive from '@radix-ui/react-dialog';
import { X } from 'lucide-react';
import { cn } from '../../lib/cn';
import { IconButton } from '../ui';

export const Dialog = DialogPrimitive.Root;
export const DialogTrigger = DialogPrimitive.Trigger;
export const DialogClose = DialogPrimitive.Close;

export function DialogContent({
  className,
  children,
  ...rest
}: React.ComponentPropsWithoutRef<typeof DialogPrimitive.Content>): React.ReactElement {
  return (
    <DialogPrimitive.Portal>
      <DialogPrimitive.Overlay className="fixed inset-0 z-50 bg-black/55 backdrop-blur-[2px] data-[state=open]:animate-[fadeIn_150ms_ease-out]" />
      <DialogPrimitive.Content
        className={cn(
          'fixed top-1/2 left-1/2 z-50 max-h-[88dvh] w-[min(94vw,640px)] -translate-x-1/2 -translate-y-1/2 overflow-y-auto',
          'rounded-lg border border-border bg-elev p-7 text-fg shadow-[var(--fx-shadow-modal)]',
          'data-[state=open]:animate-[scaleIn_160ms_var(--fx-ease-out)_both]',
          'focus:outline-none',
          className,
        )}
        {...rest}
      >
        {children}
        <DialogPrimitive.Close asChild>
          <IconButton label="Close" className="absolute top-3.5 right-3.5">
            <X size={14} />
          </IconButton>
        </DialogPrimitive.Close>
      </DialogPrimitive.Content>
    </DialogPrimitive.Portal>
  );
}

export function DialogTitle({
  className,
  ...rest
}: React.ComponentPropsWithoutRef<typeof DialogPrimitive.Title>): React.ReactElement {
  return (
    <DialogPrimitive.Title
      className={cn('mb-3 text-lg font-semibold tracking-[-0.01em]', className)}
      {...rest}
    />
  );
}

export function DialogDescription({
  className,
  ...rest
}: React.ComponentPropsWithoutRef<typeof DialogPrimitive.Description>): React.ReactElement {
  return <DialogPrimitive.Description className={cn('text-sm text-muted', className)} {...rest} />;
}
