#!/usr/bin/env bash
# Full regression sweep across all three backends (interpreter, C/
# freestanding, LLVM native) -- formalizes the verification this project
# has been doing by hand, one bash loop at a time, in every session so
# far. Run this before ANY commit that touches checker.rs, interpreter.rs,
# codegen_c.rs, codegen_llvm.rs, modules.rs, monomorphize.rs, or
# lambda_lift.rs -- i.e. anything that could change what a Kernel (.kn)
# program actually means, as opposed to pure tooling (lsp.rs, manifest.rs).
#
# What "pass" means per example, honestly stated (not every example is
# expected to succeed on every backend -- see RFC-002/RFC-003's named
# scope gaps):
#   - interpreter: must run without a Kernel-level error. A Rust panic
#     (e.g. the OOB-bounds-check demo) is a DELIBERATE pass for that one
#     file -- handled via the $EXPECT_PANIC list below, not a failure.
#   - C backend: must compile cleanly (no `error`, no
#     `warning: passing`/`warning: implicit` -- those have caught real
#     type-inference bugs in this project before, see RFC-002/engineering
#     history). QEMU boot + screendump is NOT automated here (comparing
#     a screenshot needs a human/vision check, see
#     qemu-headless-verification) -- this script only proves the C
#     compiles; boot it manually when touching codegen_c.rs specifically.
#   - LLVM native: EITHER produces a real executable whose stdout matches
#     the interpreter's EXACTLY, OR is rejected with a message containing
#     "Kernel LLVM codegen:" (a real, intentional, named v1.x gap --
#     raw_write/unsafe, read_file/write_file, cross-compilation). Any
#     OTHER panic (a Rust index-out-of-bounds inside the compiler itself,
#     an LLVM verifier failure, etc.) is a REAL BUG, not a named gap, and
#     fails the sweep.
#
# Usage: ./scripts/regression.sh   (run from anywhere; cd's to the repo root itself)

set -uo pipefail
cd "$(dirname "$0")/.."

PASS=0
FAIL=0
REJECTED=0
FAILURES=()

# Files whose CORRECT behavior is to panic/halt loudly, not produce
# matching stdout -- named here once, not re-discovered as a false
# "mismatch" every time this script runs.
EXPECT_PANIC=("oob_panic_demo.kn")

# Files that are CORRECTLY rejected by the C backend itself (not just
# the LLVM native one) with a real, named "Kernel codegen:" panic --
# e.g. vec_test.kn, since Vec<T> needs a heap allocator the freestanding
# C target doesn't have (see codegen_c.rs's vec_new/vec_push/etc. match
# arm). Treated as a pass, same idea as LLVM's named-rejection handling
# below, just one backend earlier.
C_EXPECT_REJECT=("vec_test.kn")

is_c_expect_reject() {
    local base
    base=$(basename "$1")
    local pattern
    for pattern in "${C_EXPECT_REJECT[@]}"; do
        [ "$base" == "$pattern" ] && return 0
    done
    return 1
}

echo "=== building kernel (debug) ==="
cargo build -q 2>&1 | grep -E "^error" && { echo "BUILD FAILED -- fix before running the sweep"; exit 1; }
echo "build ok"
echo

is_expect_panic() {
    local base
    base=$(basename "$1")
    local pattern
    for pattern in "${EXPECT_PANIC[@]}"; do
        [ "$base" == "$pattern" ] && return 0
    done
    return 1
}

echo "=== interpreter + C + LLVM native sweep ==="
for f in examples/*.kn examples/modules/main.kn; do
    base=$(basename "$f")
    [[ "$base" == bad_*.kn ]] && continue

    interp_out=$(cargo run -q -- run "$f" 2>/dev/null)
    interp_exit=$?

    if is_expect_panic "$f"; then
        if [ $interp_exit -ne 0 ]; then
            echo "✓ $f (interpreter correctly panics on expected OOB)"
        else
            echo "✗ $f: expected the interpreter to panic (OOB) but it exited 0"
            FAIL=$((FAIL + 1)); FAILURES+=("$f: interpreter should have panicked")
            continue
        fi
    elif [ $interp_exit -ne 0 ]; then
        echo "✗ $f: interpreter failed unexpectedly (exit $interp_exit)"
        FAIL=$((FAIL + 1)); FAILURES+=("$f: interpreter failed")
        continue
    fi

    c_out=$(cargo run -q -- build "$f" /tmp/kn_regress_c.bin 2>&1)

    if is_c_expect_reject "$f"; then
        if echo "$c_out" | grep -q "Kernel codegen:"; then
            reason=$(echo "$c_out" | grep -o "Kernel codegen:.*" | head -1)
            native_out2=$(cargo run -q -- build-native "$f" /tmp/kn_regress_native 2>&1)
            if echo "$native_out2" | grep -q "Kernel LLVM codegen:"; then
                echo "○ $f (C correctly rejected: ${reason:0:60}...; LLVM native also correctly rejected)"
                REJECTED=$((REJECTED + 1)); PASS=$((PASS + 1))
            else
                echo "✗ $f: C correctly rejected, but LLVM native did NOT (expected a named rejection too)"
                FAIL=$((FAIL + 1)); FAILURES+=("$f: LLVM native should have rejected like C did")
            fi
        else
            echo "✗ $f: expected the C backend to reject this with a named 'Kernel codegen:' panic, but it didn't"
            FAIL=$((FAIL + 1)); FAILURES+=("$f: C backend should have rejected this")
        fi
        continue
    fi

    c_bad=$(echo "$c_out" | grep -iE "^error|warning: passing|warning: implicit")
    if [ -n "$c_bad" ]; then
        echo "✗ $f: C backend issue: $(echo "$c_bad" | head -1)"
        FAIL=$((FAIL + 1)); FAILURES+=("$f: C backend: $(echo "$c_bad" | head -1)")
        continue
    fi

    native_out=$(cargo run -q -- build-native "$f" /tmp/kn_regress_native 2>&1)
    if echo "$native_out" | grep -q "panicked"; then
        reason=$(echo "$native_out" | grep -o "Kernel LLVM codegen:.*" | head -1)
        if [ -n "$reason" ]; then
            echo "○ $f: C ok, LLVM native correctly rejected (${reason:0:70}...)"
            REJECTED=$((REJECTED + 1)); PASS=$((PASS + 1))
            continue
        else
            echo "✗ $f: LLVM native backend crashed with a REAL bug (not a named rejection): $(echo "$native_out" | grep panicked | head -1)"
            FAIL=$((FAIL + 1)); FAILURES+=("$f: unnamed LLVM native crash")
            continue
        fi
    fi

    if is_expect_panic "$f"; then
        /tmp/kn_regress_native >/dev/null 2>&1
        native_exit=$?
        if [ $native_exit -ne 0 ]; then
            echo "✓ $f (C ok, LLVM native correctly halts on expected OOB too)"
            PASS=$((PASS + 1))
        else
            echo "✗ $f: expected the LLVM native binary to halt (OOB) but it exited 0"
            FAIL=$((FAIL + 1)); FAILURES+=("$f: native binary should have halted")
        fi
        continue
    fi

    native_run_out=$(/tmp/kn_regress_native 2>&1)
    if [ "$native_run_out" == "$interp_out" ]; then
        echo "✓ $f (C ok, LLVM native matches interpreter exactly)"
        PASS=$((PASS + 1))
    else
        echo "✗ $f: LLVM native output MISMATCH"
        echo "    interpreter: $(echo "$interp_out" | tr '\n' '|')"
        echo "    native:      $(echo "$native_run_out" | tr '\n' '|')"
        FAIL=$((FAIL + 1)); FAILURES+=("$f: native/interpreter output mismatch")
    fi
done

echo
echo "=== negative tests (bad_*.kn) -- must still be rejected by the checker ==="
for f in examples/bad_*.kn; do
    [ -f "$f" ] || continue
    out=$(cargo run -q -- run "$f" 2>&1)
    if echo "$out" | grep -q "error: Kernel safety check failed"; then
        echo "✓ $f (correctly rejected)"
        PASS=$((PASS + 1))
    else
        echo "✗ $f: NOT rejected -- this is a REGRESSION in the checker"
        FAIL=$((FAIL + 1)); FAILURES+=("$f: checker failed to reject")
    fi
done

echo
echo "=== summary ==="
echo "pass: $PASS (of which $REJECTED are correctly-named LLVM native rejections)"
echo "fail: $FAIL"
if [ $FAIL -gt 0 ]; then
    echo
    echo "FAILURES:"
    for msg in "${FAILURES[@]}"; do
        echo "  - $msg"
    done
    exit 1
fi
echo "all clean."
