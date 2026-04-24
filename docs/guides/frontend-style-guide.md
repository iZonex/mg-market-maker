# Frontend Style Guide

How to build new UI without adding to the mess. Two layers, two rules.

## The two layers

```
  tokens.css                    ← colours, spacing, radii, typography
       │                          (one source of truth)
       ▼
  primitives/                   ← Button, Modal, Chip, StatusPill,
  (src/lib/primitives/)           FormField, DataGrid
       │
       ▼
  components / pages            ← compose primitives + app logic
```

- **`tokens.css`** owns every colour, spacing unit, radius, shadow, z-index, typography scale. 108 variables, well-sectioned. Editing one token re-skins the whole app.
- **`primitives/`** owns the base interactive surfaces. Every button in the app is a `<Button>`. Every modal is a `<Modal>`. No one rolls their own.
- **Everything else** (components, pages) composes primitives. They're allowed to have layout CSS — table grids, page sections, bespoke one-off widgets — as long as colours come from tokens.

## The two rules

### Rule 1: Primitives consume tokens only

Inside `src/lib/primitives/`, `#hex` and `rgba(...)` literals are banned. Only `var(--name)`. CI will grep for the pattern; a PR introducing either fails review.

**Why:** primitives are the broadest-reach code. One hex literal in `Button.svelte` defeats the whole token layer — a fork that re-skins by editing tokens would still see the old button colour bleed through.

**Exception:** none. If you need a colour that isn't in tokens, add the token first, then reference it.

### Rule 2: Components never duplicate primitives

If you catch yourself writing `.btn { padding: ...; background: var(--accent); }` in a component `<style>` block, stop. That's `<Button>` territory. Add a variant to `<Button>` if none of the existing ones fit, then use it.

**Why:** 14 different `.btn {}` CSS blocks existed before Phase 1. Each had slightly different sizing / hover state / focus ring. Inconsistent UX + 14× the surface to update when styling changes.

## Available primitives

### `<Button>`
```svelte
<Button
  variant="primary|ghost|danger|warn|ok"
  size="xs|sm|md|lg"
  iconOnly={false}
  loading={false}
  onclick={handler}
>
  Click me
</Button>
```
Forwards all native `<button>` props (`type`, `disabled`, `aria-*`, `title`, `onclick`, `onkeydown`). `loading` implies `disabled` + spinner overlay.

### `<Modal>`
```svelte
<Modal
  open={someCondition}
  ariaLabel="Confirm delete"
  maxWidth="640px"
  onClose={close}
>
  {#snippet children()}
    <h3>Are you sure?</h3>
    <p>This can't be undone.</p>
  {/snippet}
  {#snippet actions()}
    <Button variant="ghost" onclick={close}>Cancel</Button>
    <Button variant="danger" onclick={confirm}>Delete</Button>
  {/snippet}
</Modal>
```
Handles backdrop click + Escape dismiss. Don't roll your own `.modal-backdrop` / `.modal-card` / `.modal-actions` — `<Modal>` owns those.

### `<Chip>`
Compact status / category marker. Use for roles, product tags, regimes, severities.
```svelte
<Chip tone="spot">SPOT</Chip>
<Chip tone="perp">PERP</Chip>
<Chip tone="admin">admin</Chip>
<Chip tone="danger" size="xs">rejected</Chip>
```
Tones: `neutral, accent, positive, warn, danger, info, spot, perp, invperp, admin, operator, viewer, client`.

### `<StatusPill>`
Top-of-page indicators with a leading dot. Slightly larger than `<Chip>`; use when the status IS the message.
```svelte
<StatusPill severity="ok">L0 Normal</StatusPill>
<StatusPill severity="warn" pulse>L2 StopNewOrders</StatusPill>
<StatusPill severity="danger" label="L3 CancelAll" />
```

### `<FormField>`
Label + input wrapper with error + hint.
```svelte
<FormField label="Client ID" hint="Unique identifier" error={clientIdErr}>
  {#snippet input()}
    <input type="text" bind:value={clientId} />
  {/snippet}
</FormField>
```
You provide the actual `<input>` / `<select>` / `<textarea>` via the `input` snippet. `FormField` handles the label row + error styling.

### `<DataGrid>`
Dense tabular data.
```svelte
<DataGrid
  columns={[
    { key: 'symbol', label: 'Symbol', mono: true },
    { key: 'pnl', label: 'PnL', align: 'right', mono: true },
  ]}
  rows={list}
  onRowClick={(r) => open(r.id)}
  emptyText="no deployments"
/>
```
Use the `cell` snippet for per-column custom rendering (embed a `<Chip>`, format a number, etc.).

### `<StatTile>`
One KPI — label, big value, optional delta/meta. Tone colours the value.
```svelte
<StatTile label="Mid" value="50,123.45" mono />
<StatTile label="PnL (24h)" value="+$1,234" tone="positive" delta="+2.1%" />
<StatTile label="Spread" value="12.4" meta="bps" tone="warn" />
```
Pre-format the string; missing values render as `—` automatically.

### `<SectionHeader>`
Consistent header for a grouped block INSIDE a Card or page section.
```svelte
<SectionHeader title="Recent fills" subtitle="last 20">
  {#snippet actions()}
    <Button variant="ghost" size="sm" onclick={refresh}>Refresh</Button>
  {/snippet}
</SectionHeader>
```
Use `<Card>` for the outer chrome (title + subtitle baked in), `<SectionHeader>` for sub-groups inside.

### `<EmptyState>`
"Nothing here yet" placeholder with an icon + title + hint. Variants: `waiting`, `done`, `muted`.
```svelte
<EmptyState
  icon="history"
  variant="waiting"
  title="No fills yet"
  hint="Engine starts emitting once the first quote lands."
/>
```

### `<Card>`
Page-panel chrome — title + actions + body + empty/loading states built in. Re-exported from primitives for discovery; lives in `components/` for historical import-path stability.
```svelte
<Card title="Fleet" subtitle="live sessions" span={2} empty={rows.length === 0}>
  {#snippet actions()}<Button ...>...</Button>{/snippet}
  {#snippet children()}<DataGrid {columns} {rows} />{/snippet}
</Card>
```

## When to roll your own vs extend a primitive

- **New variant of an existing primitive** → extend the primitive (add a `variant` / `tone` / `severity` value). Don't fork.
- **Genuinely new shape** (e.g. a specialised graph-node panel, a highly custom sparkline) → a regular component in `components/`. Rule 1 still applies: tokens, not hex.
- **Bespoke one-off in a specific page** → fine, but keep the CSS scoped (`<style>` block in the same file) + use tokens.

## Anti-patterns (CI will flag these)

```svelte
<!-- ❌ Don't reinvent Button -->
<button class="btn ghost sm" onclick={handle}>...</button>

<!-- ❌ Don't hand-roll modal chrome -->
<div class="modal-backdrop" onclick={close}>
  <div class="modal-card">...</div>
</div>

<!-- ❌ Don't hex-code inside a component -->
<style>
  .my-thing { color: #fafafa; border: 1px solid rgba(255,255,255,0.1); }
</style>
```

```svelte
<!-- ✅ Do this instead -->
<Button variant="ghost" size="sm" onclick={handle}>...</Button>

<Modal open={isOpen} ariaLabel="..." onClose={close}>
  {#snippet children()}...{/snippet}
</Modal>

<style>
  .my-thing { color: var(--fg-primary); border: 1px solid var(--border-subtle); }
</style>
```

## Migration status

Phase 1 (primitives) landed; Phase 2 (migration) is incremental:

| Component | Status |
|-----------|--------|
| `ReplayModal.svelte` | ✓ migrated to `<Modal>` + `<Button>` |
| `StrategyPage.svelte` | pending |
| `FleetPage.svelte` | pending |
| `DeploymentDrilldown.svelte` | pending |
| `DeployDialog.svelte` | pending |
| … (60 others) | migrate on touch |

Old code stays; new code uses primitives. Don't open a pre-emptive megarefactor PR — bring migrations along with feature work on the same page.

## See also

- **[Rebranding guide](rebranding.md)** — how to fork-and-rebrand for a white-label deployment
- **`frontend/src/lib/tokens.css`** — authoritative token list
- **`frontend/src/lib/primitives/`** — the primitives themselves
