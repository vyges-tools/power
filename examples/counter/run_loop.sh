#!/usr/bin/env bash
# Demonstrates the closed power-integrity loop on the counter block:
#
#   vyges-extract (SPEF)  ->  vyges-power (activity map)  ->  vyges-em-ir (droop)
#
# It runs vyges-power three ways and feeds each per-instance map into vyges-em-ir,
# then compares the predicted IR droop:
#   - WORST-CASE-simultaneous (vectorless activity 2.0, the conservative current
#     em-ir assumes by default),
#   - MEASURED from a VCD (counter.vcd),
#   - MEASURED from a Verilator --trace-saif SAIF (counter.saif — regenerated from
#     counter_tb.v + cells_sim.v when `verilator` is on PATH, else the committed file).
# Measured activity gives the realistic (lower) droop. (counter_pdn.lef uses a
# deliberately coarse grid so a 5-cell block shows a readable number; the RATIO is
# the point, not the absolute value.)
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

# 0) Real activity from a gate-level simulation: Verilator --trace-saif emits a SAIF
#    over the netlist's nets. Regenerate it when a simulator is available; otherwise
#    keep the committed counter.saif so the loop still runs offline.
if command -v verilator >/dev/null 2>&1; then
  echo "0) verilator --trace-saif: counter_tb.v -> counter.saif"
  rm -rf obj_dir
  verilator --binary --trace-saif -Wno-fatal -Wno-DECLFILENAME -Wno-WIDTH \
    --top-module counter_tb counter_tb.v counter.v cells_sim.v >/dev/null 2>&1
  ./obj_dir/Vcounter_tb >/dev/null 2>&1
else
  echo "0) verilator not found — using the committed counter.saif"
fi

echo "1) vyges-power — MEASURED activity (counter.vcd)  -> counter.activity"
"$P" run counter.pwr -q
echo "2) vyges-power — MEASURED activity (counter.saif) -> counter_saif.activity"
"$P" run counter_saif.pwr -q
echo "3) vyges-power — WORST-CASE map (vectorless 2.0)  -> counter_worst.activity"
"$P" run counter_worst.pwr -q

droop() { "$E" run "$1.emir" | grep -oE '\([0-9.]+%\)' | tr -d '()'; }
curr()  { awk 'NR>3{s+=$2} END{printf "%.3f uA", s*1e6}' "$1.activity"; }

echo
printf "%-24s %12s %12s\n" "em-ir current source" "total I" "worst droop"
printf "%-24s %12s %12s\n" "worst-case (default)" "$(curr counter_worst)" "$(droop counter_worst)"
printf "%-24s %12s %12s\n" "vyges-power (VCD)"     "$(curr counter)"       "$(droop counter)"
printf "%-24s %12s %12s\n" "vyges-power (SAIF)"    "$(curr counter_saif)"  "$(droop counter_saif)"
echo
echo "=> the worst-case-simultaneous assumption overpredicts the droop; vyges-power's"
echo "   measured per-instance activity — from a VCD or a Verilator --trace-saif SAIF —"
echo "   gives the realistic, lower droop."
