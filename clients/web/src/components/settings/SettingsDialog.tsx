/**
 * SettingsDialog — the server-global settings surface: ONE dialog, three
 * sections (Providers / MCP servers / Skills) switched with the app's
 * segmented-control Tabs (ui/tabs — the sidebar's tab language). The
 * former three peer dialogs shared 90% of their chrome (title, hint,
 * scroll region, form) and forced three round-trips through the menu;
 * one surface with a tab strip makes the sections side-by-side
 * comparable and the strip a single always-visible anchor.
 *
 * Desktop sections are MASTER-DETAIL: a selection rail (persistent New
 * row + entries) on the left, a preview/detail pane on the right — the
 * page never scrolls; each pane scrolls internally only when its own
 * content outgrows the box. Mobile keeps the stacked layout. Size: fixed
 * height min(88dvh, 780px) and a wider 900px body for the two columns —
 * switching tabs never resizes the dialog. Radix unmounts inactive
 * content, so a section's fetch-on-show and (for providers) the catalog
 * probe fire only when the section is actually visited.
 *
 * The gear opens this dialog directly (TopBar, no menu); the
 * last-visited section is remembered across open/close within the page
 * session (module state — the dialog itself unmounts).
 *
 * Provides: SettingsDialog, SettingsTab
 * Depends: components/settings/{ProvidersPanel,McpPanel,SkillsPanel},
 *          components/ui/dialog, components/ui/tabs
 */
import { useState } from 'react';
import { Dialog, DialogContent, DialogTitle } from '../ui/dialog';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '../ui/tabs';
import { McpPanel } from './McpPanel';
import { ProvidersPanel } from './ProvidersPanel';
import { SkillsPanel } from './SkillsPanel';

export type SettingsTab = 'providers' | 'mcp' | 'skills';

const TAB_VALUES: SettingsTab[] = ['providers', 'mcp', 'skills'];

/** Last-visited section — survives close/reopen within a page session
 * (module-level on purpose: the dialog unmounts, the memory shouldn't). */
let lastVisitedTab: SettingsTab = 'providers';

export function SettingsDialog(props: {
  initialTab?: SettingsTab;
  onClose: () => void;
}): React.ReactElement {
  const [tab, setTab] = useState<SettingsTab>(() =>
    props.initialTab && TAB_VALUES.includes(props.initialTab) ? props.initialTab : lastVisitedTab,
  );
  return (
    <Dialog open onOpenChange={(open) => !open && props.onClose()}>
      <DialogContent className="flex h-[min(88dvh,780px)] w-[min(94vw,900px)] flex-col overflow-y-auto max-md:p-4">
        <DialogTitle className="shrink-0">Settings</DialogTitle>
        <Tabs
          value={tab}
          onValueChange={(v) => {
            setTab(v as SettingsTab);
            lastVisitedTab = v as SettingsTab;
          }}
          className="flex min-h-0 flex-1 flex-col gap-4"
        >
          <TabsList aria-label="Settings sections" className="shrink-0">
            <TabsTrigger value="providers">Providers</TabsTrigger>
            <TabsTrigger value="mcp">MCP servers</TabsTrigger>
            <TabsTrigger value="skills">Skills</TabsTrigger>
          </TabsList>
          <TabsContent value="providers" className="flex min-h-0 flex-1 flex-col">
            <ProvidersPanel />
          </TabsContent>
          <TabsContent value="mcp" className="flex min-h-0 flex-1 flex-col">
            <McpPanel />
          </TabsContent>
          <TabsContent value="skills" className="flex min-h-0 flex-1 flex-col">
            <SkillsPanel />
          </TabsContent>
        </Tabs>
      </DialogContent>
    </Dialog>
  );
}
