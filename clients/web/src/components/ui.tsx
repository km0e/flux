/**
 * ui.tsx — control primitives: the single styling authority for controls.
 *
 * Variants map to Tailwind utilities consuming the --fx-* token theme
 * (styles/app.css @theme bridge); per-feature ad-hoc control styling is
 * retired — layout utilities may be passed via `className`, never colors.
 *
 * Provides: Button, IconButton, TextField, Badge, Spinner
 */
import { forwardRef } from 'react';
import { cn } from '../lib/cn';

export type ButtonVariant = 'primary' | 'secondary' | 'ghost' | 'danger';
export type ButtonSize = 'sm' | 'md';

const BUTTON_VARIANT: Record<ButtonVariant, string> = {
  primary:
    'bg-accent text-accent-fg shadow-sm hover:bg-accent-strong active:translate-y-px disabled:hover:bg-accent',
  secondary: 'bg-elev text-fg border border-border hover:bg-hover hover:border-border-strong',
  ghost: 'bg-transparent text-muted hover:text-fg hover:bg-hover',
  danger: 'bg-danger text-white hover:brightness-110',
};

export interface ButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = 'secondary', size = 'md', className, type = 'button', ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      type={type}
      className={cn(
        'inline-flex items-center justify-center gap-1.5 whitespace-nowrap rounded-sm font-medium',
        'transition-colors duration-100 cursor-pointer disabled:opacity-45 disabled:cursor-not-allowed disabled:pointer-events-none',
        // Touch floor: controls clear 40px on mobile (HIG/Material); the
        // desktop heights stay at the control-height tokens.
        size === 'md'
          ? 'h-[var(--fx-control-h)] max-md:h-10 px-3 text-sm'
          : 'h-[var(--fx-control-h-sm)] max-md:h-9 px-2.5 text-xs',
        BUTTON_VARIANT[variant],
        className,
      )}
      {...rest}
    />
  );
});

export interface IconButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  /** Required — icon-only controls must stay reachable by name. */
  label: string;
}

export const IconButton = forwardRef<HTMLButtonElement, IconButtonProps>(function IconButton(
  { label, className, type = 'button', ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      type={type}
      aria-label={label}
      title={rest.title ?? label}
      className={cn(
        'inline-flex items-center justify-center rounded-sm text-muted',
        'size-7 max-md:size-10 shrink-0 cursor-pointer transition-colors duration-100 hover:bg-hover hover:text-fg',
        className,
      )}
      {...rest}
    />
  );
});

export const TextField = forwardRef<HTMLInputElement, React.InputHTMLAttributes<HTMLInputElement>>(
  function TextField({ className, ...rest }, ref) {
    return (
      <input
        ref={ref}
        className={cn(
          'h-[var(--fx-control-h)] w-full rounded-sm border border-border bg-inset px-2.5 text-sm text-fg',
          'placeholder:text-faint transition-colors duration-100',
          'focus:border-accent focus:outline-none',
          className,
        )}
        {...rest}
      />
    );
  },
);

export type BadgeTone = 'default' | 'warn' | 'accent';

export function Badge({
  tone = 'default',
  className,
  ...rest
}: React.HTMLAttributes<HTMLSpanElement> & { tone?: BadgeTone }): React.ReactElement {
  return (
    <span
      className={cn(
        'inline-flex items-center rounded-full px-1.5 py-px text-2xs font-medium leading-4 whitespace-nowrap',
        tone === 'default' && 'bg-hover text-muted',
        tone === 'warn' && 'bg-warn/15 text-warn',
        tone === 'accent' && 'bg-accent-dim text-accent',
        className,
      )}
      {...rest}
    />
  );
}

/** Running spinner (10px, currentcolor ring). */
export function Spinner({ className }: { className?: string }): React.ReactElement {
  return (
    <span
      aria-hidden="true"
      className={cn(
        'inline-block size-3 rounded-full border-[1.5px] border-current border-t-transparent animate-spin',
        className,
      )}
    />
  );
}
