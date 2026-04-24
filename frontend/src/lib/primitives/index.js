/*
 * Primitives — the one place design-system primitives live.
 *
 * Rule #1: primitives consume CSS tokens only. No hex or rgb
 * values inside `primitives/`. A CI check against this path enforces
 * it so a theme swap (edit `tokens.css`) actually re-skins the app.
 *
 * Rule #2: primitives never import from other app modules. They
 * are leaf components — consumers compose, primitives don't.
 *
 * Import from this barrel:
 *   import { Button, Modal, Chip, StatusPill, FormField, DataGrid } from '$lib/primitives'
 */

export { default as Button }     from './Button.svelte'
export { default as Modal }      from './Modal.svelte'
export { default as Chip }       from './Chip.svelte'
export { default as StatusPill } from './StatusPill.svelte'
export { default as FormField }  from './FormField.svelte'
export { default as DataGrid }   from './DataGrid.svelte'
