/**
 * ui/tabs.tsx — Radix Tabs wrapper, styled as a SEGMENTED CONTROL (the
 * app's one tab language: an inset track, the active segment raised on
 * elev). Used by the sidebar panels (Chats / Files).
 *
 * Provides: Tabs, TabsList, TabsTrigger, TabsContent
 */
import * as TabsPrimitive from '@radix-ui/react-tabs';
import { cn } from '../../lib/cn';

export const Tabs = TabsPrimitive.Root;

export function TabsList({
  className,
  ...rest
}: React.ComponentPropsWithoutRef<typeof TabsPrimitive.List>): React.ReactElement {
  return (
    <TabsPrimitive.List
      className={cn('flex shrink-0 gap-0.5 rounded-md bg-inset p-0.5', className)}
      {...rest}
    />
  );
}

export function TabsTrigger({
  className,
  ...rest
}: React.ComponentPropsWithoutRef<typeof TabsPrimitive.Trigger>): React.ReactElement {
  return (
    <TabsPrimitive.Trigger
      className={cn(
        'flex-1 cursor-pointer rounded-sm px-2.5 py-1 max-md:py-2.5 max-md:text-sm font-medium text-muted select-none',
        'transition-colors duration-100 hover:text-fg',
        'data-[state=active]:bg-elev data-[state=active]:text-fg data-[state=active]:shadow-sm',
        className,
      )}
      {...rest}
    />
  );
}

export function TabsContent({
  className,
  ...rest
}: React.ComponentPropsWithoutRef<typeof TabsPrimitive.Content>): React.ReactElement {
  return (
    <TabsPrimitive.Content
      className={cn('min-h-0 flex-1 overflow-hidden data-[state=inactive]:hidden', className)}
      {...rest}
    />
  );
}
