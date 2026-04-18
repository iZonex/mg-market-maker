<script>
  /*
   * Icon — crafted SVG icon set, lucide-aligned.
   *
   * All icons live on a 24×24 grid, stroke 1.75 px,
   * round caps + joins. `currentColor` so they inherit text
   * colour from parent. Size is controlled by the `size`
   * prop (CSS px) — default 16.
   */
  let { name, size = 16, label = null } = $props()

  // Path dictionary — keys are identifiers we use throughout
  // the UI. Easy to add new icons, one place.
  const paths = {
    // Nav
    overview:    'M3 3h7v9H3zM14 3h7v5h-7zM14 12h7v9h-7zM3 16h7v5H3z',
    orderbook:   'M4 6h16 M4 10h12 M4 14h14 M4 18h8',
    // Horizontal sliders — classic "tuning" metaphor (three rails
    // with perpendicular thumb marks at different positions).
    calibration: 'M4 6H12 M16 6H20 M14 4V8|M4 12H8 M12 12H20 M10 10V14|M4 18H14 M18 18H20 M16 16V20',
    compliance:  'M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z|M9 12l2 2 4-4',
    admin:       'M20 7h-9 M14 17H5',
    // Two heads for the Users page.
    users:       'M16 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2 M8.5 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8 M22 21v-2a4 4 0 0 0-3-3.87 M16.5 3.13a4 4 0 0 1 0 7.75',

    // UI actions
    chevronDown: 'M6 9l6 6 6-6',
    chevronUp:   'M6 15l6-6 6 6',
    chevronR:    'M9 6l6 6-6 6',
    check:       'M20 6L9 17l-5-5',
    close:       'M18 6L6 18 M6 6l12 12',
    search:      'M11 19a8 8 0 1 1 0-16 8 8 0 0 1 0 16z M21 21l-4.35-4.35',
    logout:      'M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4 M16 17l5-5-5-5 M21 12H9',
    refresh:     'M23 4v6h-6|M1 20v-6h6|M3.51 9a9 9 0 0 1 14.85-3.36L23 10|M20.49 15a9 9 0 0 1-14.85 3.36L1 14',
    clock:       'M12 7v5l3 3',
    // History — classic lucide: counter-clockwise arrow around a clock
    history:     'M3 3v5h5|M3.05 13A9 9 0 1 0 6 5.3L3 8|M12 7v5l4 2',
    link:        'M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71 M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71',
    external:    'M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6 M15 3h6v6 M10 14L21 3',
    settings:    'M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8z',
    // Graph / node editor — three connected circles (DAG hint).
    graph:       'M5 6a2 2 0 1 0 0 4 2 2 0 0 0 0-4 M19 6a2 2 0 1 0 0 4 2 2 0 0 0 0-4 M12 14a2 2 0 1 0 0 4 2 2 0 0 0 0-4|M7 8l10 0|M7 10l5 4|M17 10l-5 4',
    doc:         'M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z M14 2v6h6 M9 13h6 M9 17h4',

    // Status
    alert:       'M12 2a10 10 0 1 0 10 10 M12 8v4 M12 16h.01',
    info:        'M12 2a10 10 0 1 0 10 10 M12 16v-4 M12 8h.01',
    pulse:       'M22 12h-4l-3 9-6-18-3 9H2',
    shield:      'M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z',
    bolt:        'M13 2L3 14h7l-1 8 10-12h-7l1-8z',

    // File operations
    download:    'M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4 M7 10l5 5 5-5 M12 15V3',
    upload:      'M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4 M17 8l-5-5-5 5 M12 3v12',
    save:        'M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2z M17 21v-8H7v8 M7 3v5h8',

    // Empty-state style
    emptyList:   'M4 6h10 M4 12h6 M4 18h8',
    emptyBook:   'M2 3h6a4 4 0 0 1 4 4v14a3 3 0 0 0-3-3H2z M22 3h-6a4 4 0 0 0-4 4v14a3 3 0 0 1 3-3h7z',
  }
  const d = $derived(paths[name] || '')
  const segments = $derived(d ? d.split('|') : [])
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.75"
  stroke-linecap="round"
  stroke-linejoin="round"
  class="icon"
  aria-hidden={!label}
  aria-label={label}
  role={label ? 'img' : 'presentation'}
>
  {#each segments as s}
    <path d={s} />
  {/each}
</svg>

<style>
  .icon { display: inline-block; flex-shrink: 0; vertical-align: middle; }
</style>
