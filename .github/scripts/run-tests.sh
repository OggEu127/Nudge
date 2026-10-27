#!/usr/bin/env bash
# Used by Windows CI and release builds through Git Bash.
set -euo pipefail

# Actions invokes Bash with -e -o pipefail. Capture both pipeline statuses before
# restoring fail-fast behavior, otherwise a failing test bypasses diagnostics.
set +e
cargo test --locked --workspace --all-features --no-fail-fast 2>&1 | tee test-output.log
statuses=("${PIPESTATUS[@]}")
set -e

if (( statuses[0] != 0 )); then
  if command -v node >/dev/null 2>&1; then
    if ! node - <<'JS'
const fs = require('fs');
const text = fs.readFileSync('test-output.log', 'utf8').replace(/\r\n/g, '\n');
// Keep panic details as well as the final failing-test names, across crates.
const sections = text.match(/(?:^|\n)failures:\n[\s\S]*?(?=\ntest result:|$)/g);
const details = (sections ? sections.join('\n') : text).slice(-16000);
const message = details.replace(/%/g, '%25').replace(/\r/g, '%0D').replace(/\n/g, '%0A');
console.log(`::error title=Test failure output::${message}`);
JS
    then
      echo '::warning::Cannot generate test summary; inspect the test log.'
    fi
  fi
  # Reporting must never hide or replace Cargo's failure status.
  exit "${statuses[0]}"
fi

if (( statuses[1] != 0 )); then
  echo '::error::Cannot save the complete test log.'
  exit "${statuses[1]}"
fi
