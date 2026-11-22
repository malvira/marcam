#!/bin/sh
# Everything the project promises, in one go: format, lints, Rust tests, the WebAssembly
# package (tested under Node), the Python wheel (tested in a fresh venv) and the web app
# (headless Chrome/Edge; needs network for three.js).
set -e
cd "$(dirname "$0")/.."
# --fast: the suite merges are gated on (about 10 minutes): format, lints, the engine's unit
# tests and every golden G-code, the engine in WebAssembly, the web app's core sections, two
# monkey seeds and the frame budget. The full suite (this script without --fast, CI on every
# push) runs behind it.
if [ "$1" = "--fast" ]; then
  t0=$(date +%s)
  cargo fmt --all -- --check
  cargo clippy --workspace --exclude marcam-step --all-targets -- -D warnings
  # the engine's slowest test files are the full suite's (each above 20 s)
  SLOW=" plan_cache project_toml tapered smoothing "
  tests=""
  for f in crates/marcam-core/tests/*.rs; do
    n=$(basename "$f" .rs)
    case "$SLOW" in *" $n "*) ;; *) tests="$tests --test $n" ;; esac
  done
  # (golden.rs and the CLI's knob run: every golden G-code, byte for byte)
  cargo test --release -p marcam-core --lib $tests
  cargo test --release -p marcam-cli --test cli
  sh scripts/build-wasm.sh
  node crates/marcam-wasm/js/test.mjs
  node web/tests/meshes.test.mjs
  node web/tests/project.test.mjs
  MARCAM_E2E_FAST=1 node web/tests/e2e.mjs
  # (Mar) the app as a person sees it: real clicks and typing, only what is on the screen
  node web/tests/usereye.mjs
  node web/tests/stress.mjs --only monkey --seeds 1,2 --steps 30
  # the viewer, tree and forms: what is drawn is what the toggles say, nothing changes by itself
  node web/tests/viewmonkey.mjs --seeds 1,2 --actions 30
  node web/tests/perf.mjs --gate --seconds 2
  echo "FAST CHECKS PASSED in $(( $(date +%s) - t0 )) s"
  exit 0
fi
cargo fmt --all -- --check
# STEP import builds SolidCraft's kernel: MARCAM_SKIP_STEP leaves it to scripts/check-step.sh
# (CI runs that as a job of its own)
if [ -n "$MARCAM_SKIP_STEP" ]; then
  cargo clippy --workspace --exclude marcam-step --all-targets -- -D warnings
else
  cargo clippy --workspace --all-targets -- -D warnings
fi
cargo clippy -p marcam-cli --features gpu --all-targets -- -D warnings
cargo test
# the GPU prototype against the CPU (skips, saying so, without a hardware GPU)
# one test at a time: several GPU devices opened at once have hung (a test burned 15 min of
# CPU in parallel, 5 s alone)
cargo test -p marcam-gpu -- --nocapture --test-threads=1
# the SolidCraft plugin: tests, the guest linted and packed, and the real host when
# SOLIDCRAFT_CLI names a solidcraft-cli (skipped otherwise, saying so)
cargo build -p marcam-cli
cargo test -p marcam-solidcraft
cargo clippy -p marcam-solidcraft --lib --target wasm32-unknown-unknown -- -D warnings
sh scripts/build-solidcraft-plugin.sh
sh scripts/e2e-solidcraft.sh
# the C ABI: its library, the C example and (with a .NET SDK) the C# example
sh scripts/test-capi.sh
# the embedding examples in Rust, Python and Node (docs/embedding.md)
sh scripts/test-embed.sh
# the FreeCAD adapter, with the wheel, against a mocked FreeCAD API
sh scripts/test-freecad.sh
sh scripts/build-wasm.sh
node crates/marcam-wasm/js/test.mjs
sh scripts/test-python.sh
node web/tests/meshes.test.mjs
node web/tests/project.test.mjs
node web/tests/e2e.mjs
node web/tests/usereye.mjs
# stateful UI tests: scripted scenarios and the seeded monkey (fixed seeds)
node web/tests/stress.mjs
node web/tests/viewmonkey.mjs --seeds 1,2,3,4 --actions 50
# the simulation fuzz in the browser (fixed seeds; the engine-level one runs with cargo test)
node web/tests/simfuzz.mjs
# playback smoothness and a responsive page during background work (MARCAM_PERF_SLACK
# loosens the frame budget on slow machines)
node web/tests/perf.mjs --gate
if [ -z "$MARCAM_SKIP_STEP" ]; then sh scripts/check-step.sh; else echo "STEP checks skipped (MARCAM_SKIP_STEP)"; fi
echo "ALL CHECKS PASSED"
