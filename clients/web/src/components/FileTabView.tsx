/**
 * FileTabView.tsx — the CONTENT of one open file tab in the right dock,
 * chrome-free: the tab strip owns the name, the close button, and the
 * floating actions (Copy / Raw / truncated badge).
 *
 * Markdown files (.md/.markdown) render through the shared markdown
 * pipeline (marked + DOMPurify + highlight.js — the same `renderMarkdown`
 * the streaming pipeline uses, including the delegated code-copy handler);
 * the Raw view is the tab's `rawView` flag (strip toggle). Everything else
 * stays plain text, ALWAYS horizontally scrolling (wrap support was
 * removed deliberately: `whitespace-pre`, no toggle).
 *
 * Provides: FileTabView
 * Depends: core/state.ts, lib/markdown.ts
 */
import { useMemo } from 'react';
import { useFlux } from '../core/state';
import { renderMarkdown } from '../lib/markdown';

/** Extensions rendered as markdown instead of plain text. */
const MARKDOWN_EXT = /\.(md|markdown)$/i;

export function FileTabView(props: { tabId: string }): React.ReactElement {
  const tab = useFlux((s) => s.openFiles.find((t) => t.id === props.tabId));
  // Markdown parses once per content, not per render — the dock re-renders
  // on width changes and a 256KB file re-parse per pointer event is exactly
  // the jank the drag must not have.
  const renderedHtml = useMemo(
    () => (tab && MARKDOWN_EXT.test(tab.name) ? renderMarkdown(tab.content) : ''),
    [tab],
  );
  if (!tab) return <div className="flex-1" />;

  const isMarkdown = MARKDOWN_EXT.test(tab.name);
  const showRendered = isMarkdown && !tab.rawView && !tab.error && !tab.loading;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {showRendered ? (
        /* Sanitized by renderMarkdown (DOMPurify); `.prose` brings the same
           typography and code-copy chrome as the chat stream. */
        <div
          className="md-preview prose min-h-0 flex-1 overflow-auto px-4 py-3"
          dangerouslySetInnerHTML={{ __html: renderedHtml }}
        />
      ) : (
        <pre className="min-h-0 flex-1 overflow-auto whitespace-pre p-2.5 font-mono text-xs leading-[1.5]">
          {/* Read failures report on the unified toast stack — the pane itself
              stays a neutral placeholder, never a raw error dump. */}
          {tab.loading
            ? 'Loading…'
            : tab.error
              ? '(file could not be read)'
              : tab.content || '(empty file)'}
        </pre>
      )}
    </div>
  );
}
