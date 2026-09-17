/**
 * provider-forms.tsx — the Providers panel's form section: the creation
 * form and the edit form. Split out of ProvidersPanel.tsx (列表节 ↔ 表单节
 * seam); both are shared verbatim by the desktop detail pane and the
 * mobile stacked layout — one set of field states, one submit, owned by
 * the panel (the forms are controlled, stateless display).
 *
 * Provides: ProviderForm, ProviderEditForm
 * Depends: settings/shared.tsx, ui/
 */

import type React from 'react';
import { FormField, SectionLabel } from './shared';
import { Button, Spinner, TextField } from '../ui';

/** The creation form — shared verbatim by the desktop detail pane and the
 * mobile stacked layout (one set of field states, one submit). */
export function ProviderForm(props: {
  id: string;
  url: string;
  apiKey: string;
  adding: boolean;
  canAdd: boolean;
  addError: string | null;
  setId: (v: string) => void;
  setUrl: (v: string) => void;
  setApiKey: (v: string) => void;
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
      <SectionLabel>Add provider</SectionLabel>
      <div className="flex gap-2 max-md:flex-col">
        <FormField label="Id" hint="e.g. main" className="flex-1">
          <TextField value={props.id} onChange={(e) => props.setId(e.target.value)} placeholder="main" />
        </FormField>
        <FormField label="Base url" hint="blank = OpenAI default" className="flex-[1.8]">
          <TextField
            value={props.url}
            onChange={(e) => props.setUrl(e.target.value)}
            placeholder="https://api.openai.com/v1"
            className="font-mono text-sm"
          />
        </FormField>
      </div>
      <FormField label="API key" hint="stored in the server database, never shown back">
        <TextField
          type="password"
          value={props.apiKey}
          onChange={(e) => props.setApiKey(e.target.value)}
          placeholder="sk-…"
          autoComplete="off"
          className="font-mono text-sm"
        />
      </FormField>
      {props.addError && <span className="text-2xs break-all text-danger">{props.addError}</span>}
      <div className="flex justify-end">
        <Button variant="primary" type="submit" disabled={!props.canAdd}>
          {props.adding ? <Spinner /> : null}
          Add provider
        </Button>
      </div>
    </form>
  );
}

/** The edit form — an entry's endpoint is editable BEHIND its fixed id
 * (the id is what chat pins and saved models reference; renaming is
 * remove + re-add). Shared verbatim by the desktop detail pane and the
 * mobile rows. The url prefills with the EFFECTIVE one (an unset url
 * arrives resolved to the OpenAI default); the api_key field starts
 * EMPTY and means "keep" — the stored key never comes down the wire. */
export function ProviderEditForm(props: {
  id: string;
  url: string;
  apiKey: string;
  saving: boolean;
  saveError: string | null;
  setUrl: (v: string) => void;
  setApiKey: (v: string) => void;
  onSave: () => void;
  onCancel: () => void;
}): React.ReactElement {
  return (
    <form
      className="flex flex-col gap-3 rounded-lg border border-border bg-panel p-4"
      onSubmit={(e) => {
        e.preventDefault();
        props.onSave();
      }}
    >
      <SectionLabel>Edit provider</SectionLabel>
      <FormField label="Id" hint="the reference chats pin — never changes">
        <TextField
          value={props.id}
          readOnly
          aria-label={`Id of ${props.id}`}
          className="font-mono text-sm opacity-70"
        />
      </FormField>
      <FormField label="Base url" hint="blank = OpenAI default">
        <TextField
          value={props.url}
          onChange={(e) => props.setUrl(e.target.value)}
          placeholder="https://api.openai.com/v1"
          aria-label={`Base url of ${props.id}`}
          className="font-mono text-sm"
        />
      </FormField>
      <FormField label="API key" hint="leave blank to keep the stored key">
        <TextField
          type="password"
          value={props.apiKey}
          onChange={(e) => props.setApiKey(e.target.value)}
          placeholder="stored — leave blank to keep"
          autoComplete="off"
          aria-label={`API key of ${props.id}`}
          className="font-mono text-sm"
        />
      </FormField>
      {props.saveError && <span className="text-2xs break-all text-danger">{props.saveError}</span>}
      <div className="flex justify-end gap-2">
        <Button variant="ghost" type="button" onClick={props.onCancel}>
          Cancel
        </Button>
        <Button variant="primary" type="submit" disabled={props.saving}>
          {props.saving ? <Spinner /> : null}
          Save changes
        </Button>
      </div>
    </form>
  );
}
