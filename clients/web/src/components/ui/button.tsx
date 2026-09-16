/**
 * button.tsx — Button + IconButton: the click-target controls. This
 * module (with its ui/ siblings) is the single styling authority for
 * controls — variants map to Tailwind utilities consuming the --fx-*
 * token theme (styles/app.css @theme bridge); per-feature ad-hoc control
 * styling is retired — layout utilities may be passed via `className`,
 * never colors.
 *
 * Provides: Button, IconButton, ButtonVariant, ButtonSize, ButtonProps,
 *           IconButtonProps
 * Depends: lib/cn.ts
 */
import { forwardRef } from 'react';
import { cn } from '../../lib/cn';

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
        'transition-colors duration-fast cursor-pointer disabled:opacity-45 disabled:cursor-not-allowed disabled:pointer-events-none',
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
        'size-7 max-md:size-10 shrink-0 cursor-pointer transition-colors duration-fast hover:bg-hover hover:text-fg',
        className,
      )}
      {...rest}
    />
  );
});
