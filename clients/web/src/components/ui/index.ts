/**
 * index.ts — the component library's public surface: control primitives
 * (this directory) + the Radix behavior wrappers re-exported so ONE import
 * point covers both voices. Deep imports (`./ui/dialog`, `./ui/tabs`, …)
 * stay available and are the convention for the composite Radix chunks
 * (they keep dialog/menu code out of bundles that never open one).
 *
 * Primitives: Button, IconButton, TextField, SelectField, TextArea, Badge,
 *             Spinner
 * Radix wrappers: Dialog*, DropdownMenu*, Tabs*, Tooltip
 */
export { Button, IconButton } from './button';
export type { ButtonVariant, ButtonSize, ButtonProps, IconButtonProps } from './button';
export { TextField, SelectField, TextArea } from './fields';
export { Badge } from './badge';
export type { BadgeTone } from './badge';
export { Spinner } from './spinner';
