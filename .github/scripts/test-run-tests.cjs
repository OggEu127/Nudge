// Regression tests for the CI shell wrapper. Uses only local fake commands.
// Run on Linux; the production wrapper is also used with Git Bash on Windows.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const runner = path.resolve('.github/scripts/run-tests.sh');
const fixture = fs.mkdtempSync(path.join(os.tmpdir(), 'nudge-ci-tests-'));
try {
  const tests = [
    { name: 'success', cargo: 0, expected: 0 },
    { name: 'test failure retains status and panic details', cargo: 101, expected: 101, annotation: true },
    { name: 'compiler failure retains diagnostics', cargo: 101, expected: 101, compile: true, annotation: true },
    { name: 'summary failure cannot replace test failure', cargo: 101, expected: 101, nodeFailure: true },
    { name: 'log writer failure is not ignored', cargo: 0, expected: 73, teeFailure: true },
    { name: 'test failure takes precedence over log failure', cargo: 101, expected: 101, teeFailure: true },
  ];
  for (const [index, test] of tests.entries()) {
    const directory = path.join(fixture, String(index));
    const bin = path.join(directory, 'bin');
    fs.mkdirSync(bin, { recursive: true });
    const log = test.compile ? 'error[E0000]: synthetic compiler failure\n' :
      'running 1 test\r\ntest fixture ... FAILED\r\n\r\nfailures:\r\n\r\n---- fixture stdout ----\r\nsynthetic panic with 100% detail\r\n\r\nfailures:\r\n    fixture\r\n\r\ntest result: FAILED\r\n';
    fs.writeFileSync(path.join(directory, 'fixture.log'), log);
    fs.writeFileSync(path.join(bin, 'cargo'), '#!/bin/bash\n' +
      '[[ "$*" == "test --locked --workspace --all-features --no-fail-fast" ]] || exit 92\n' +
      `if (( ${test.cargo} != 0 )); then /bin/cat fixture.log; else printf 'test result: ok\\n'; fi\n` +
      `exit ${test.cargo}\n`, { mode: 0o700 });
    if (test.nodeFailure) fs.writeFileSync(path.join(bin, 'node'), '#!/bin/bash\nexit 64\n', { mode: 0o700 });
    if (test.teeFailure) fs.writeFileSync(path.join(bin, 'tee'), '#!/bin/bash\n/bin/cat > /dev/null\nexit 73\n', { mode: 0o700 });
    const result = spawnSync('/bin/bash', ['--noprofile', '--norc', '-e', '-o', 'pipefail', runner], {
      cwd: directory,
      env: { ...process.env, PATH: `${bin}:${process.env.PATH}` },
      encoding: 'utf8',
    });
    assert.equal(result.status, test.expected, `${test.name}: ${result.stderr}`);
    if (!test.teeFailure) assert.ok(fs.existsSync(path.join(directory, 'test-output.log')));
    if (test.annotation) {
      assert.ok(result.stdout.includes('::error title=Test failure output::'), test.name);
      assert.ok(result.stdout.includes(test.compile ? 'synthetic compiler failure' : 'synthetic panic with 100%25 detail'), test.name);
    }
    if (test.nodeFailure) assert.ok(result.stdout.includes('::warning::Cannot generate test summary'));
    console.log(`PASS: ${test.name}`);
  }
} finally {
  fs.rmSync(fixture, { recursive: true });
}
