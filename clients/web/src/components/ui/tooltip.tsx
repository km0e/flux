/**
 * ui/tooltip.tsx — Radix Tooltip wrapper (shadcn conventions) + Provider.
 *
 * Mount the Provider once at the app root; Tooltip triggers anywhere.
 *
 * Provides: TooltipProvider, Tooltip
 */
import * as TooltipPrimitive from '@radix-ui/react-tooltip';
import { cn } from '../../lib/cn';

export const TooltipProvider = TooltipPrimitive.Provider;

export function Tooltip(props: {
  content: React.ReactNode;
  side?: 'top' | 'bottom' | 'left' | 'right';
  openDelay?: number;
  children: React.ReactNode;
}): React.ReactElement {
  // A self-contained Provider: the tooltip is usable standalone (tests,
  // isolated mounts); the App-level Provider's delayDuration default is
  // overridden per-instance here anyway. Radix providers nest fine.
  return (
    <TooltipPrimitive.Provider delayDuration={props.openDelay ?? 250}>
      <TooltipPrimitive.Root>
        <TooltipPrimitive.Trigger asChild>{props.children}</TooltipPrimitive.Trigger>
        <TooltipPrimitive.Portal>
          <TooltipPrimitive.Content
            side={props.side ?? 'bottom'}
            sideOffset={6}
            className={cn(
              'z-[60] max-w-[340px] rounded-md border border-border bg-elev px-2.5 py-1.5',
              'text-2xs leading-relaxed whitespace-pre-wrap text-fg shadow-[var(--fx-shadow-pop)]',
              'data-[state=delayed-open]:animate-[fadeIn_120ms_ease-out]',
            )}
          >
            {props.content}
          </TooltipPrimitive.Content>
        </TooltipPrimitive.Portal>
      </TooltipPrimitive.Root>
    </TooltipPrimitive.Provider>
  );
}
