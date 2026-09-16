/**
 * fields.tsx — text-entry controls: TextField, SelectField, TextArea.
 * One focus language (accent border on focus, no double ring), one
 * control radius and height — the field family reads as a single system
 * across every panel and dialog.
 *
 * Provides: TextField, SelectField, TextArea
 * Depends: lib/cn.ts
 */
import { forwardRef } from 'react';
import { cn } from '../../lib/cn';

export const TextField = forwardRef<HTMLInputElement, React.InputHTMLAttributes<HTMLInputElement>>(
  function TextField({ className, ...rest }, ref) {
    return (
      <input
        ref={ref}
        className={cn(
          'h-[var(--fx-control-h)] w-full rounded-sm border border-border bg-inset px-2.5 text-sm text-fg',
          'placeholder:text-faint transition-colors duration-fast',
          'focus:border-accent focus:outline-none',
          className,
        )}
        {...rest}
      />
    );
  },
);

/** The native select, styled to match TextField (one focus language, one
 * control radius). The chevron rides the UA's default — a custom arrow
 * would need an appearance reset + background SVG for no visual gain. */
export const SelectField = forwardRef<
  HTMLSelectElement,
  React.SelectHTMLAttributes<HTMLSelectElement>
>(function SelectField({ className, ...rest }, ref) {
  return (
    <select
      ref={ref}
      className={cn(
        'h-[var(--fx-control-h)] w-full cursor-pointer rounded-sm border border-border bg-inset px-2.5 text-sm text-fg',
        'transition-colors duration-fast focus:border-accent focus:outline-none',
        className,
      )}
      {...rest}
    />
  );
});

/** Multiline input, styled to match TextField (the same focus/border
 * rules; mono by default — the content is machine voice: commands, args,
 * launch lines). */
export const TextArea = forwardRef<
  HTMLTextAreaElement,
  React.TextareaHTMLAttributes<HTMLTextAreaElement>
>(function TextArea({ className, rows = 4, ...rest }, ref) {
  return (
    <textarea
      ref={ref}
      rows={rows}
      className={cn(
        'min-h-24 w-full resize-y rounded-sm border border-border bg-inset px-3 py-2 font-mono text-sm leading-relaxed text-fg',
        'placeholder:text-faint transition-colors duration-fast',
        'focus:border-accent focus:outline-none',
        className,
      )}
      {...rest}
    />
  );
});
