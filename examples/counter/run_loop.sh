#!/usr/bin/env bash
# Demonstrates the closed power-integrity loop on the counter block:
#
#   vyges-extract (SPEF)  ->  vyges-power (activity map)  ->  vyges-em-ir (droop)
#
# It runs vyges-power two ways — MEASURED activity (from counter.vcd) and a
# WORST-CASE-simultaneous map (vectorless activity 2.0, the conservative current
# em-ir assumes by default) — feeds each per-instance map into vyges-em-ir, and
# compares the predicted IR droop. Measured activity gives the realistic (lower)
# droop. (counter_pdn.lef uses a deliberately coarse grid so a 5-cell block shows
# a readable number; the RATIO is the point, not the absolute value.)
set -euo pipefail
cd "$(dirname "$0")"

ROOT=../../..
PWR="$ROOT/vyges-tools-power"
EMIR="$ROOT/vyges-tools-em-ir"

echo "building vyges-power + vyges-em-ir…"
( cd "$PWR"  && cargo build --quiet )
( cd "$EMIR" && cargo build --quiet )
P="$PWR/target/debug/vyges-power"
E="$EMIR/target/debug/vyges-em-ir"

echo "1) vyges-power — MEASURED activity (counter.vcd) -> counter.activity"
"$P" run counter.pwr -q
echo "2) vyges-power — WORST-CASE map (vectorless 2.0) -> counter_worst.activity"
"$P" run counter_worst.pwr -q

echo
printf "%-22s %12s %12s\n" "em-ir current source" "total I" "worst droop"
w=$("$E" run counter_worst.emir | grep -oE '\([0-9.]+%\)' | tr -d '()')
m=$("$E" run counter.emir       | grep -oE '\([0-9.]+%\)' | tr -d '()')
wi=$(awk 'NR>3{s+=$2} END{printf "%.3f uA", s*1e6}' counter_worst.activity)
mi=$(awk 'NR>3{s+=$2} END{printf "%.3f uA", s*1e6}' counter.activity)
printf "%-22s %12s %12s\n" "worst-case (default)" "$wi" "$w"
printf "%-22s %12s %12s\n" "vyges-power measured" "$mi" "$m"
echo
echo "=> the worst-case-simultaneous assumption overpredicts the droop; vyges-power's"
echo "   measured per-instance activity gives the realistic, lower droop."
