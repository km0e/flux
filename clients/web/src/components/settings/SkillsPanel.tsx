/**
 * SkillsPanel — the installed skills' browse + install/remove section of
 * the Settings dialog.
 *
 * Skills are pure filesystem content: the list shows the global skills
 * dir (user-installed, removable) plus the active chat's workdir-local
 * (project) skills (read-only — they live in the user's repository).
 * Installing materializes a validated copy into the global dir and is
 * IMMEDIATELY effective — flux-tools scans fresh on every skill_list
 * call, so there is no restart semantics.
 *
 * Two install sources, one field: a local directory path or a git URL
 * (detected by the `https?://` / `git@` prefix), with an optional
 * subpath for multi-skill repositories. Name collisions are rejected
 * server-side — no silent overwrite.
 *
 * Desktop: master-detail — a selection rail (a persistent "+ Install
 * skill" row above the entries) and a detail pane that either previews
 * the selected skill (description, source, remove for global entries) or
 * carries the install form. Mobile keeps the stacked layout (hint,
 * notice, rows with inline actions, form). The install form stacks its
 * fields FULL-WIDTH (source, then subpath — the previous half-width
 * side-by-side pair crushed the subpath to ~90px on phones), and the
 * draft lives at panel level so switching the selection never loses a
 * half-typed source.
 *
 * Presentation shares the shared building blocks (settings/shared) with
 * ProvidersPanel / McpPanel — one typography scale, one spacing rhythm,
 * one two-step-remove control, one rail-selection repair.
 *
 * Provides: SkillsPanel
 * Depends: core/state.ts, services/skills.ts, hooks/useIsMobile.ts,
 *          components/ui/*, components/settings/shared.tsx
 */
import { useEffect, useState } from 'react';
import { useFlux } from '../../core/state';
import { addSkill, fetchSkills, removeSkill } from '../../services/skills';
import { useIsMobile } from '../../hooks/useIsMobile';
import type { SkillSummary } from '../../core/types';
import { Badge, Button, Spinner, TextField } from '../ui';
import {
  DetailPane,
  DialogHint,
  EmptyState,
  FormField,
  MasterDetail,
  NewRailButton,
  NoticeBar,
  Rail,
  RailButton,
  RailEmpty,
  RemoveControl,
  RowShell,
  RowSub,
  RowTitle,
  SectionLabel,
  useRailSelection,
} from './shared';

/** Selection key — names can collide across sources, so the key carries
 * the source too. */
const keyOf = (s: SkillSummary): string => `${s.source}:${s.name}`;

/** The install form — shared verbatim by the desktop detail pane and the
 * mobile stacked layout. */
function SkillForm(props: {
  source: string;
  subpath: string;
  adding: boolean;
  canAdd: boolean;
  addError: string | null;
  isUrl: boolean;
  setSource: (v: string) => void;
  setSubpath: (v: string) => void;
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
      <SectionLabel>Install a skill</SectionLabel>
      <FormField label="Source" hint="a local directory with a SKILL.md, or a git URL">
        <TextField
          value={props.source}
          onChange={(e) => props.setSource(e.target.value)}
          placeholder="/path/to/skill  ·  https://github.com/user/skills"
          className="font-mono text-sm"
        />
      </FormField>
      <FormField
        label="Subpath"
        hint={props.isUrl ? 'optional — a directory inside a multi-skill repo' : 'git URLs only — disabled for local paths'}
      >
        <TextField
          value={props.subpath}
          onChange={(e) => props.setSubpath(e.target.value)}
          placeholder={props.isUrl ? 'skills/pdf-tools' : '—'}
          disabled={!props.isUrl}
          className="font-mono text-sm"
        />
      </FormField>
      {props.addError && <span className="text-2xs break-all text-danger">{props.addError}</span>}
      <div className="flex items-center justify-between gap-3 max-md:flex-col-reverse max-md:items-stretch">
        <span className="text-xs leading-relaxed text-muted">
          {props.isUrl
            ? 'Git source — shallow-cloned (https/SSH remotes), then copied without .git'
            : 'Local source — copied (never moved); the directory must contain a SKILL.md'}
        </span>
        <Button variant="primary" type="submit" disabled={!props.canAdd} className="shrink-0">
          {props.adding ? <Spinner /> : 'Install'}
        </Button>
      </div>
    </form>
  );
}

/** Mobile row: compact, with the inline actions the stacked layout needs. */
function SkillRow(props: {
  skill: SkillSummary;
  onRemove: () => Promise<string | undefined>;
}): React.ReactElement {
  return (
    <RowShell>
      <div className="flex items-center gap-2">
        <RowTitle>{props.skill.name}</RowTitle>
        <Badge
          title={
            props.skill.source === 'global'
              ? 'installed in ~/.flux/skills'
              : 'lives in this chat workdir (.flux/skills) — read-only here'
          }
        >
          {props.skill.source}
        </Badge>
        <span className="flex-1" />
        {props.skill.removable && (
          <RemoveControl label={`Remove ${props.skill.name}`} onRemove={props.onRemove} />
        )}
      </div>
      <RowSub title={props.skill.description}>{props.skill.description}</RowSub>
    </RowShell>
  );
}

/** Desktop detail: the selected skill's preview — description, source,
 * remove for global entries. Keyed so confirm/error state resets per
 * selection. */
function SkillPreview(props: {
  skill: SkillSummary;
  onRemove: () => Promise<string | undefined>;
}): React.ReactElement {
  return (
    <div className="flex flex-col gap-3 rounded-lg border border-border bg-panel p-4">
      <div className="flex items-center gap-2">
        <RowTitle>{props.skill.name}</RowTitle>
        <Badge
          title={
            props.skill.source === 'global'
              ? 'installed in ~/.flux/skills'
              : 'lives in this chat workdir (.flux/skills) — read-only here'
          }
        >
          {props.skill.source}
        </Badge>
        <span className="flex-1" />
        {props.skill.removable && (
          <RemoveControl label={`Remove ${props.skill.name}`} onRemove={props.onRemove} />
        )}
      </div>
      <RowSub title={props.skill.description}>{props.skill.description}</RowSub>
      {!props.skill.removable && (
        <span className="text-2xs text-muted">
          Read-only — a project skill living in this chat's workdir (.flux/skills).
        </span>
      )}
    </div>
  );
}

const HINT =
  'Self-contained capability packages (a directory with a SKILL.md) the model discovers ' +
  'via skill_list and loads on demand via skill_read — nothing is injected into prompts. ' +
  'Installing a skill materializes a copy in ~/.flux/skills; the source is never modified.';

export function SkillsPanel(): React.ReactElement {
  const skills = useFlux((s) => s.skills);
  const activeChatId = useFlux((s) => s.activeChatId);
  const isMobile = useIsMobile();
  const { selected, choose, noteRemoved } = useRailSelection(skills, keyOf);

  // The install-form draft lives at panel level: switching the selection
  // never loses a half-typed source.
  const [source, setSource] = useState('');
  const [subpath, setSubpath] = useState('');
  const [adding, setAdding] = useState(false);
  const [addError, setAddError] = useState<string | null>(null);

  // The list may be cold — pull on section show (incl. the active chat's
  // project skills, shown read-only).
  useEffect(() => {
    fetchSkills(activeChatId || undefined);
  }, [activeChatId]);

  const isUrl = /^(https?:\/\/|git@)/.test(source.trim());
  const canAdd = source.trim() !== '' && !adding;

  const runAdd = () => {
    setAdding(true);
    setAddError(null);
    const trimmed = source.trim();
    const input = isUrl ? { url: trimmed, subpath } : { path: trimmed, subpath: undefined };
    void addSkill(input).then((addErr) => {
      setAdding(false);
      if (addErr) {
        setAddError(addErr);
        return;
      }
      // Success: the broadcast refreshes the list — reset the form. The
      // ack carries no name, so the selection stays on the install row.
      setSource('');
      setSubpath('');
      useFlux.getState().pushToast('info', 'Skill installed — available to every chat now');
    });
  };

  /** Shared by the preview pane and the mobile rows. `key` is the rail
   *  selection key (`source:name`) — noteRemoved matches against keyOf,
   *  so the bare name never matches and the neighbor fallback dies. */
  const remove = (key: string, name: string): Promise<string | undefined> => {
    noteRemoved(key);
    return removeSkill(name).then((error) => {
      if (!error) useFlux.getState().pushToast('info', `Skill "${name}" removed`);
      return error;
    });
  };

  const selectedSkill = selected === 'new' ? undefined : skills.find((s) => keyOf(s) === selected);

  // ── Mobile: the stacked layout (hint, notice, rows, form) ──
  if (isMobile) {
    return (
      <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto">
        <DialogHint>{HINT}</DialogHint>
        <NoticeBar>
          Installs and removals are immediate — every chat's next skill_list call sees the new
          set (no restart). Project skills (this chat's workdir) are read-only here.
        </NoticeBar>
        <div className="flex flex-col gap-2">
          <SectionLabel>Installed</SectionLabel>
          {skills.length === 0 ? (
            <EmptyState>No skills installed — add one below (local path or git URL).</EmptyState>
          ) : (
            <ul aria-label="Installed skills" className="m-0 flex list-none flex-col gap-2 p-0">
              {skills.map((s) => (
                <SkillRow key={keyOf(s)} skill={s} onRemove={() => remove(keyOf(s), s.name)} />
              ))}
            </ul>
          )}
        </div>
        <SkillForm
          source={source}
          subpath={subpath}
          adding={adding}
          canAdd={canAdd}
          addError={addError}
          isUrl={isUrl}
          setSource={setSource}
          setSubpath={setSubpath}
          onSubmit={runAdd}
        />
      </div>
    );
  }

  // ── Desktop: master-detail ──
  return (
    <MasterDetail
      list={
        <Rail label="Installed skills">
          <NewRailButton label="Install skill" selected={selected === 'new'} onClick={() => choose('new')} />
          {skills.map((s) => (
            <RailButton
              key={keyOf(s)}
              selected={selected === keyOf(s)}
              onClick={() => choose(keyOf(s))}
              ariaLabel={`Select skill ${s.name}`}
              title={s.name}
              sub={s.description}
              badge={
                <Badge
                  className="shrink-0"
                  title={
                    s.source === 'global'
                      ? 'installed in ~/.flux/skills'
                      : 'lives in this chat workdir (.flux/skills) — read-only here'
                  }
                >
                  {s.source}
                </Badge>
              }
            />
          ))}
          {skills.length === 0 && (
            <RailEmpty>No skills installed.</RailEmpty>
          )}
        </Rail>
      }
      detail={
        <DetailPane>
          {selectedSkill ? (
            <SkillPreview key={keyOf(selectedSkill)} skill={selectedSkill} onRemove={() => remove(keyOf(selectedSkill), selectedSkill.name)} />
          ) : (
            <>
              <DialogHint>{HINT}</DialogHint>
              <NoticeBar>
                Installs and removals are immediate — every chat's next skill_list call sees the
                new set (no restart). Project skills (this chat's workdir) are read-only here.
              </NoticeBar>
              <SkillForm
                source={source}
                subpath={subpath}
                adding={adding}
                canAdd={canAdd}
                addError={addError}
                isUrl={isUrl}
                setSource={setSource}
                setSubpath={setSubpath}
                onSubmit={runAdd}
              />
            </>
          )}
        </DetailPane>
      }
    />
  );
}
