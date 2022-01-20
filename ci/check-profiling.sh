#!/bin/bash
#
# This script only tests some of the profilers at the moment. More coverage
# would be nice.

set -eE -x;

bash -c "while true; do sleep 30; echo \$(date) - running ...; done" &
PING_LOOP_PID=$!
trap 'kill $PING_LOOP_PID' ERR 1 2 3 6

# Install a toolchain.
RUST_BACKTRACE=1 RUST_LOG=raw_cargo_messages=trace,collector=debug,rust_sysroot=debug \
    bindir=`cargo run -p collector --bin collector install_next`

cargo build -p collector --bin rustc-fake

#----------------------------------------------------------------------------
# Test the profilers
#----------------------------------------------------------------------------

# time-passes.
RUST_BACKTRACE=1 RUST_LOG=raw_cargo_messages=trace,collector=debug,rust_sysroot=debug \
    cargo run -p collector --bin collector -- \
    profile_local time-passes $bindir/rustc \
        --id Test \
        --builds Check \
        --cargo $bindir/cargo \
        --include helloworld \
        --runs Full
test -f results/Ztp-Test-helloworld-Check-Full
grep -q "time:.*total" results/Ztp-Test-helloworld-Check-Full

# perf-record: untested because we get "The instructions:u event is not
# supported" on GitHub Actions when the code below is run. Maybe because the
# hardware is virtualized and performance counters aren't available?
#RUST_BACKTRACE=1 RUST_LOG=raw_cargo_messages=trace,collector=debug,rust_sysroot=debug \
#    cargo run -p collector --bin collector -- \
#    profile_local perf-record $bindir/rustc \
#        --id Test \
#        --builds Check \
#        --cargo $bindir/cargo \
#        --include helloworld \
#        --runs Full
#test -f results/perf-Test-helloworld-Check-Full
#grep -q "PERFILE" results/perf-Test-helloworld-Check-Full

# oprofile: untested... it's not used much, and might have the same problems
# that `perf` has due to virtualized hardware.

# Cachegrind.
RUST_BACKTRACE=1 RUST_LOG=raw_cargo_messages=trace,collector=debug,rust_sysroot=debug \
    cargo run -p collector --bin collector -- \
    profile_local cachegrind $bindir/rustc \
        --id Test \
        --builds Check \
        --cargo $bindir/cargo \
        --include helloworld \
        --runs Full
test -f results/cgout-Test-helloworld-Check-Full
grep -q "events: Ir" results/cgout-Test-helloworld-Check-Full
test -f results/cgann-Test-helloworld-Check-Full
grep -q "PROGRAM TOTALS" results/cgann-Test-helloworld-Check-Full

# Callgrind.
RUST_BACKTRACE=1 RUST_LOG=raw_cargo_messages=trace,collector=debug,rust_sysroot=debug \
    cargo run -p collector --bin collector -- \
    profile_local callgrind $bindir/rustc \
        --id Test \
        --builds Check \
        --cargo $bindir/cargo \
        --include helloworld \
        --runs Full
test -f results/clgout-Test-helloworld-Check-Full
grep -q "creator: callgrind" results/clgout-Test-helloworld-Check-Full
test -f results/clgann-Test-helloworld-Check-Full
grep -q "Profile data file" results/clgann-Test-helloworld-Check-Full

# DHAT.
RUST_BACKTRACE=1 RUST_LOG=raw_cargo_messages=trace,collector=debug,rust_sysroot=debug \
    cargo run -p collector --bin collector -- \
    profile_local dhat $bindir/rustc \
        --id Test \
        --builds Check \
        --cargo $bindir/cargo \
        --include helloworld \
        --runs Full
test -f results/dhout-Test-helloworld-Check-Full
grep -q "dhatFileVersion" results/dhout-Test-helloworld-Check-Full

# Massif.
RUST_BACKTRACE=1 RUST_LOG=raw_cargo_messages=trace,collector=debug,rust_sysroot=debug \
    cargo run -p collector --bin collector -- \
    profile_local massif $bindir/rustc \
        --id Test \
        --builds Check \
        --cargo $bindir/cargo \
        --include helloworld \
        --runs Full
test -f results/msout-Test-helloworld-Check-Full
grep -q "snapshot=0" results/msout-Test-helloworld-Check-Full

# eprintln. The output file is empty because a vanilla rustc doesn't print
# anything to stderr.
RUST_BACKTRACE=1 RUST_LOG=raw_cargo_messages=trace,collector=debug,rust_sysroot=debug \
    cargo run -p collector --bin collector -- \
    profile_local eprintln $bindir/rustc \
        --id Test \
        --builds Check \
        --cargo $bindir/cargo \
        --include helloworld \
        --runs Full
test   -f results/eprintln-Test-helloworld-Check-Full
test ! -s results/eprintln-Test-helloworld-Check-Full

# llvm-lines. `Debug` not `Check` because it doesn't support `Check` builds.
# Including both `helloworld` and `futures` benchmarks, as they exercise the
# zero dependency and the greater than zero dependency cases, respectively, the
# latter of which has broken before.
RUST_BACKTRACE=1 RUST_LOG=raw_cargo_messages=trace,collector=debug,rust_sysroot=debug \
    cargo run -p collector --bin collector -- \
    profile_local llvm-lines $bindir/rustc \
        --id Test \
        --builds Debug \
        --cargo $bindir/cargo \
        --include helloworld,futures \
        --runs Full
test -f results/ll-Test-helloworld-Debug-Full
grep -q "Lines.*Copies.*Function name" results/ll-Test-helloworld-Debug-Full
test -f results/ll-Test-futures-Debug-Full
grep -q "Lines.*Copies.*Function name" results/ll-Test-futures-Debug-Full


#----------------------------------------------------------------------------
# Test option handling
#----------------------------------------------------------------------------

# With `--builds` unspecified, `Check`/`Debug`/`Opt` files must be present, and
# `Doc` files must not be present.
RUST_BACKTRACE=1 RUST_LOG=raw_cargo_messages=trace,collector=debug,rust_sysroot=debug \
    cargo run -p collector --bin collector -- \
    profile_local eprintln $bindir/rustc \
        --id Builds1 \
        --cargo $bindir/cargo \
        --include helloworld
test   -f results/eprintln-Builds1-helloworld-Check-Full
test   -f results/eprintln-Builds1-helloworld-Check-IncrFull
test   -f results/eprintln-Builds1-helloworld-Check-IncrPatched0
test   -f results/eprintln-Builds1-helloworld-Check-IncrUnchanged
test   -f results/eprintln-Builds1-helloworld-Debug-Full
test   -f results/eprintln-Builds1-helloworld-Debug-IncrFull
test   -f results/eprintln-Builds1-helloworld-Debug-IncrPatched0
test   -f results/eprintln-Builds1-helloworld-Debug-IncrUnchanged
test   -f results/eprintln-Builds1-helloworld-Opt-Full
test   -f results/eprintln-Builds1-helloworld-Opt-IncrFull
test   -f results/eprintln-Builds1-helloworld-Opt-IncrPatched0
test   -f results/eprintln-Builds1-helloworld-Opt-IncrUnchanged
test ! -e results/eprintln-Builds1-helloworld-Doc-Full
test ! -e results/eprintln-Builds1-helloworld-Doc-IncrFull
test ! -e results/eprintln-Builds1-helloworld-Doc-IncrPatched0
test ! -e results/eprintln-Builds1-helloworld-Doc-IncrUnchanged

# With `--builds Doc` specified, `Check`/`Debug`/`Opt` files must not be
# present, and `Doc` files must be present (but not for incremental runs).
RUST_BACKTRACE=1 RUST_LOG=raw_cargo_messages=trace,collector=debug,rust_sysroot=debug \
    cargo run -p collector --bin collector -- \
    profile_local eprintln $bindir/rustc \
        --id Builds2 \
        --builds Doc \
        --cargo $bindir/cargo \
        --include helloworld
test ! -e results/eprintln-Builds2-helloworld-Check-Full
test ! -e results/eprintln-Builds2-helloworld-Check-IncrFull
test ! -e results/eprintln-Builds2-helloworld-Check-IncrUnchanged
test ! -e results/eprintln-Builds2-helloworld-Check-IncrPatched0
test ! -e results/eprintln-Builds2-helloworld-Debug-Full
test ! -e results/eprintln-Builds2-helloworld-Debug-IncrFull
test ! -e results/eprintln-Builds2-helloworld-Debug-IncrUnchanged
test ! -e results/eprintln-Builds2-helloworld-Debug-IncrPatched0
test ! -e results/eprintln-Builds2-helloworld-Opt-Full
test ! -e results/eprintln-Builds2-helloworld-Opt-IncrFull
test ! -e results/eprintln-Builds2-helloworld-Opt-IncrUnchanged
test ! -e results/eprintln-Builds2-helloworld-Opt-IncrPatched0
test   -f results/eprintln-Builds2-helloworld-Doc-Full
test ! -f results/eprintln-Builds2-helloworld-Doc-IncrFull
test ! -f results/eprintln-Builds2-helloworld-Doc-IncrPatched0
test ! -f results/eprintln-Builds2-helloworld-Doc-IncrUnchanged

# With `--runs IncrUnchanged` specified, `IncrFull` and `IncrUnchanged` files
# must be present.
RUST_BACKTRACE=1 RUST_LOG=raw_cargo_messages=trace,collector=debug,rust_sysroot=debug \
    cargo run -p collector --bin collector -- \
    profile_local eprintln $bindir/rustc \
        --id Runs1 \
        --builds Check \
        --cargo $bindir/cargo \
        --include helloworld \
        --runs IncrUnchanged
test ! -e results/eprintln-Runs1-helloworld-Check-Full
test   -f results/eprintln-Runs1-helloworld-Check-IncrFull
test   -f results/eprintln-Runs1-helloworld-Check-IncrUnchanged
test ! -e results/eprintln-Runs1-helloworld-Check-IncrPatched0

# With `--runs IncrPatched` specified, `IncrFull` and `IncrPatched0` files must
# be present.
RUST_BACKTRACE=1 RUST_LOG=raw_cargo_messages=trace,collector=debug,rust_sysroot=debug \
    cargo run -p collector --bin collector -- \
    profile_local eprintln $bindir/rustc \
        --id Runs2 \
        --builds Check \
        --cargo $bindir/cargo \
        --include helloworld \
        --runs IncrPatched
test ! -e results/eprintln-Runs2-helloworld-Check-Full
test   -f results/eprintln-Runs2-helloworld-Check-IncrFull
test ! -e results/eprintln-Runs2-helloworld-Check-IncrUnchanged
test   -f results/eprintln-Runs2-helloworld-Check-IncrPatched0

kill $PING_LOOP_PID
exit 0
