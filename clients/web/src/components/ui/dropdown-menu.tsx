/**
 * ui/dropdown-menu.tsx — Radix DropdownMenu wrapper (shadcn conventions).
 *
 * Owns only styling; Radix owns the behavior (roving focus, typeahead,
 * Esc, outside-click, collision-avoiding positioning).
 *
 * Provides: DropdownMenu, DropdownMenuTrigger, DropdownMenuItem,
 *           DropdownMenuContent
 */
import * as MenuPrimitive from '@radix-ui/react-dropdown-menu';
import { cn } from '../../lib/cn';

export const DropdownMenu = MenuPrimitive.Root;
export const DropdownMenuTrigger = MenuPrimitive.Trigger;

export function DropdownMenuItem({
  danger,
  className,
  ...rest
}: React.ComponentPropsWithoutRef<typeof MenuPrimitive.Item> & {
  danger?: boolean;
}): React.ReactElement {
  return (
    <MenuPrimitive.Item
      className={cn(
        'flex cursor-pointer items-center gap-2 rounded-sm px-2.5 py-1.5 text-xs select-none outline-none',
        'text-fg data-[highlighted]:bg-hover',
        danger && 'text-danger data-[highlighted]:bg-danger/10',
        className,
      )}
      {...rest}
    />
  );
}

export function DropdownMenuContent({
  className,
  ...rest
}: React.ComponentPropsWithoutRef<typeof MenuPrimitive.Content>): React.ReactElement {
  return (
    <MenuPrimitive.Portal>
      <MenuPrimitive.Content
        sideOffset={4}
        align="start"
        className={cn(
          'z-[60] min-w-[132px] rounded-md border border-border bg-elev p-1',
          'shadow-[var(--fx-shadow-pop)] data-[state=open]:animate-scale-in',
          className,
        )}
        {...rest}
      />
    </MenuPrimitive.Portal>
  );
}
