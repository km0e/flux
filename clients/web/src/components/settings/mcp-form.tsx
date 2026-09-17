/**
 * mcp-form.tsx — the MCP panel's form section: the kind picker + the
 * creation form. Split out of McpPanel.tsx (列表节 ↔ 表单节 seam); the
 * form is shared verbatim by the desktop detail pane and the mobile
 * stacked layout — one set of field states, one submit, owned by the
 * panel (the form is a controlled, stateless display).
 *
 * Provides: McpForm
 * Depends: settings/shared.tsx, ui/, lib/cn.ts, core/types.ts
 */

import type React from 'react';
import { cn } from '../../lib/cn';
import type { McpKind } from '../../core/types';
import { FormField, SectionLabel } from './shared';
import { Button, Spinner, TextArea, TextField } from '../ui';

/** The kind picker — a two-segment control in the app's segmented-tab
 * language (inset track, the active segment raised). */
function KindPicker(props: { kind: McpKind; onChange: (k: McpKind) => void }): React.ReactElement {
  const seg = (k: McpKind, label: string, title: string) => (
    <button
      type="button"
      aria-pressed={props.kind === k}
      title={title}
      onClick={() => props.onChange(k)}
      className={cn(
        'flex-1 cursor-pointer rounded-sm px-2.5 py-1 font-medium text-muted select-none',
        'transition-colors duration-fast hover:text-fg',
        props.kind === k && 'bg-elev text-fg shadow-sm',
      )}
    >
      {label}
    </button>
  );
  return (
    <div role="group" aria-label="Transport" className="flex gap-0.5 rounded-md bg-inset p-0.5">
      {seg('stdio', 'Command', 'Spawn a local child process over stdio')}
      {seg('http', 'URL', 'Connect to a remote Streamable HTTP endpoint')}
    </div>
  );
}

/** The creation form — shared verbatim by the desktop detail pane and the
 * mobile stacked layout. The stdio fields (command/args/env) and the http
 * fields (url/headers) are kind-conditional; the shared parts (id, kind,
 * submit) stay put so switching kind keeps the typed id. */
export function McpForm(props: {
  id: string;
  kind: McpKind;
  command: string;
  argsText: string;
  envText: string;
  urlText: string;
  headersText: string;
  adding: boolean;
  canAdd: boolean;
  addError: string | null;
  setId: (v: string) => void;
  setKind: (k: McpKind) => void;
  setCommand: (v: string) => void;
  setArgsText: (v: string) => void;
  setEnvText: (v: string) => void;
  setUrlText: (v: string) => void;
  setHeadersText: (v: string) => void;
  onSubmit: () => void;
}): React.ReactElement {
  return (
    <form
      className="flex flex-col gap-3 rounded-lg border border-border bg-panel p-4"
      onSubmit={(e) => {
        e.preventDefault();
        if (props.canAdd) props.onSubmit();
      }}
    >
      <SectionLabel>Add MCP server</SectionLabel>
      <div className="flex gap-2 max-md:flex-col">
        <FormField label="Id" hint="e.g. filesystem" className="flex-1">
          <TextField value={props.id} onChange={(e) => props.setId(e.target.value)} placeholder="filesystem" />
        </FormField>
        <FormField label="Transport" hint="local child process, or remote Streamable HTTP" className="flex-1">
          <KindPicker kind={props.kind} onChange={props.setKind} />
        </FormField>
      </div>
      {props.kind === 'stdio' ? (
        <>
          <FormField label="Command" hint="the executable to spawn">
            <TextField
              value={props.command}
              onChange={(e) => props.setCommand(e.target.value)}
              placeholder="npx"
              className="font-mono text-sm"
            />
          </FormField>
          <FormField label="Arguments" hint="one per line">
            <TextArea
              aria-label="Arguments"
              rows={4}
              placeholder={'-y\n@modelcontextprotocol/server-filesystem\n/path/to/workspace'}
              value={props.argsText}
              onChange={(e) => props.setArgsText(e.target.value)}
            />
          </FormField>
          <FormField
            label="Environment"
            hint="KEY=VALUE per line — the child gets ONLY these (nothing inherited); values are stored server-side and never echoed back"
          >
            <TextArea
              aria-label="Environment"
              rows={3}
              placeholder={'SOME_VAR=value\nAPI_TOKEN=… (the value never leaves the server)'}
              value={props.envText}
              onChange={(e) => props.setEnvText(e.target.value)}
            />
          </FormField>
        </>
      ) : (
        <>
          <FormField label="URL" hint="the Streamable HTTP endpoint (http/https)">
            <TextField
              value={props.urlText}
              onChange={(e) => props.setUrlText(e.target.value)}
              placeholder="https://example.com/mcp"
              className="font-mono text-sm"
            />
          </FormField>
          <FormField
            label="Headers"
            hint="KEY=VALUE per line — sent with every request; auth rides here (Authorization=Bearer …); values are stored server-side and never echoed back"
          >
            <TextArea
              aria-label="Headers"
              rows={3}
              placeholder={'Authorization=Bearer … (the value never leaves the server)'}
              value={props.headersText}
              onChange={(e) => props.setHeadersText(e.target.value)}
            />
          </FormField>
        </>
      )}
      {props.addError && <span className="text-2xs break-all text-danger">{props.addError}</span>}
      <div className="flex justify-end">
        <Button variant="primary" type="submit" disabled={!props.canAdd}>
          {props.adding ? <Spinner /> : null}
          Add server
        </Button>
      </div>
    </form>
  );
}
