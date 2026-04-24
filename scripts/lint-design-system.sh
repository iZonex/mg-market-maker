#!/usr/bin/env bash
# Design-system linter. Enforces the invariants the design-system
# refactor (waves 1-4) established. Run from the repo root:
#
#     ./scripts/lint-design-system.sh
#
# CI should call this after `npm run build`. Fail-loud on any
# violation — the whole point of the design system is that these
# invariants don't drift silently.
#
# What it checks:
#   1. No hex / rgba literals inside src/lib/primitives/ (tokens only)
#   2. No `.btn { ... }` CSS outside primitives/Button.svelte
#   3. No `.modal-backdrop` CSS outside primitives/Modal.svelte
#   4. No `class="btn ..."` inline usage outside primitives/
#   5. No hex literal outside tokens.css, utilities.css, primitives/,
#      and chart-theme.js (with a few justified QR-contrast exceptions)
#   6. No hardcoded "Market Maker" / "MG Market Maker" outside
#      branding.js (re-branding single-file surface)
#   7. No duplicate `.chip { ... }` / `.tone-X { ... }` / `.pill { ... }`
#      CSS rules outside utilities.css
#
# Exit 0 = clean; non-zero = the first failing check's count.

set -u
cd "$(dirname "$0")/.." || exit 1

RED=$'\033[1;31m'
GRN=$'\033[0;32m'
YLW=$'\033[0;33m'
NC=$'\033[0m'

fails=0

check() {
    local label="$1"; shift
    local expected="$1"; shift
    local count
    count=$("$@" | wc -l | tr -d ' ')
    if [[ "$count" -gt "$expected" ]]; then
        echo "${RED}✗${NC} $label (found $count, expected ≤ $expected)"
        "$@" | head -20 | sed 's/^/    /'
        fails=$((fails + count - expected))
    else
        echo "${GRN}✓${NC} $label ($count)"
    fi
}

echo "── Design-system linter ──────────────────────────"

# 1. Primitives are token-only.
check "hex in primitives/" 0 \
    grep -rnE '#[0-9a-fA-F]{3}\b|#[0-9a-fA-F]{6}\b' frontend/src/lib/primitives \
        --include='*.svelte'

check "rgba in primitives/" 0 \
    grep -rnE 'rgba?\([0-9]' frontend/src/lib/primitives \
        --include='*.svelte'

# 2. Button CSS is singleton.
check ".btn {} outside primitives/" 0 \
    bash -c "grep -rnE '^\s*\.btn\s*\{' frontend/src/lib --include='*.svelte' | grep -v 'primitives/'"

# 3. Modal chrome is singleton.
check ".modal-backdrop CSS outside primitives/" 0 \
    bash -c "grep -rnE '^\s*\.modal-backdrop\s*\{' frontend/src/lib --include='*.svelte' | grep -v 'primitives/'"

# 4. No inline class="btn ..." outside primitives/ (2 known edge
#    cases: <label> + <summary> — those use distinct class names).
check 'inline class="btn" leaks' 0 \
    bash -c "grep -rn 'class=\"btn' frontend/src/lib --include='*.svelte' | grep -v 'primitives/'"

# 5. Hex literals allowed only in tokens.css, utilities.css, primitives,
#    chart-theme.js, and the QR-contrast block in ProfilePage. Two hex
#    literals are known exceptions for QR contrast — we allow ≤ 2.
check "hex outside design-system files" 2 \
    bash -c "grep -rnE '#[0-9a-fA-F]{3}\b|#[0-9a-fA-F]{6}\b' frontend/src/lib \
        --include='*.svelte' --include='*.css' --include='*.js' \
      | grep -v 'tokens.css' | grep -v 'utilities.css' \
      | grep -v 'primitives/' | grep -v 'chart-theme.js'"

# 6. Brand strings belong to branding.js only.
check "product-name leaks" 0 \
    bash -c "grep -rn 'Market Maker\|MG Market' frontend/src \
        --include='*.svelte' --include='*.js' --include='*.html' \
      | grep -v 'branding.js'"

# 7. Chip/pill/tone utility CSS is singleton.
check "utility-class CSS outside utilities.css" 0 \
    bash -c "grep -rnE '^\s*\.(chip|pill|tone-[a-z0-9_-]+|mono|muted|faint|pos|neg)\s*\{' frontend/src/lib \
        --include='*.svelte' \
      | grep -v 'utilities.css' | grep -v 'primitives/'"

echo

if [[ "$fails" -eq 0 ]]; then
    echo "${GRN}All design-system invariants hold.${NC}"
    exit 0
else
    echo "${RED}Design-system lint failed with $fails offending locations.${NC}"
    echo "${YLW}Fix the items above or add a justified exception in scripts/lint-design-system.sh.${NC}"
    exit 1
fi
