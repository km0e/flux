/**
 * ui/dropdown-menu.tsx — Radix DropdownMenu wrapper (shadcn conventions).
 *
 * Owns only styling; Radix owns the behavior (roving focus, typeahead,
 * Esc, outside-click, collision-avoiding positioning).
 *
 * Provides: DropdownMenu, DropdownMenuTrigger, DropdownMenuItem,
 *           DropdownMenuContent, ChatRowMenu
 */
import * as MenuPrimitive from '@radix-ui/react-dropdown-menu';
import { MoreHorizontal, Pencil, Trash2 } from 'lucide-react';
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
          'shadow-[var(--fx-shadow-pop)] data-[state=open]:animate-[scaleIn_120ms_var(--fx-ease-out)_both]',
          className,
        )}
        {...rest}
      />
    </MenuPrimitive.Portal>
  );
}

/** The chat row's ⋯ menu (Rename/Delete). */
export function ChatRowMenu(props: {
  label: string;
  onRename: () => void;
  onDelete: () => void;
}): React.ReactElement {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          aria-label={props.label}
          className={cn(
            'inline-flex size-6 cursor-pointer items-center justify-center rounded-md text-muted transition-all duration-100',
            // Hover-capable devices reveal on row hover; touch devices keep
            // the menu reachable (a hover reveal is unreachable there).
            'opacity-0 touch:opacity-100 group-hover/row:opacity-100 hover:bg-hover hover:text-fg focus-visible:opacity-100 data-[state=open]:bg-hover data-[state=open]:text-fg data-[state=open]:opacity-100',
            'max-md:size-9',
          )}
        >
          <MoreHorizontal size={14} />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent>
        <DropdownMenuItem onSelect={props.onRename}>
          <Pencil size={12} aria-hidden="true" className="text-muted" />
          Rename
        </DropdownMenuItem>
        <DropdownMenuItem danger onSelect={props.onDelete}>
          <Trash2 size={12} aria-hidden="true" />
          Delete
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
