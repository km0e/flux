/**
 * cn.ts — class merge helper (clsx + tailwind-merge).
 *
 * tailwind-merge resolves conflicts between the component's own utilities
 * and the caller's overrides (later wins), so variants compose cleanly.
 */
import { clsx, type ClassValue } from 'clsx';
import { twMerge } from 'tailwind-merge';

export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}
