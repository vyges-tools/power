#!/usr/bin/env bash
# Correlate vyges-power against OpenSTA `report_power` on a real sky130 block.
#
# Runs on a host with: yosys + sta (OpenSTA) + a sky130 hd .lib + a built
# vyges-power. Synthesizes counter.v to sky130, then runs BOTH tools on the same
# mapped netlist + the same global activity, and prints the comparison.
#
#   SKY130_LIB=/path/to/sky130_fd_sc_hd__tt_025C_1v80.lib \
#   VYGES_POWER=/path/to/vyges-power  ./run.sh
set -euo pipefail
cd "$(dirname "$0")"
: "${SKY130_LIB:?set SKY130_LIB to the sky130_fd_sc_hd hd .lib}"
PWR="${VYGES_POWER:-vyges-power}"

echo "synthesizing counter.v -> sky130 (yosys)…"
yosys -q -p "
  read_verilog counter.v
  synth -top counter -flatten
  dfflibmap -liberty $SKY130_LIB
  abc -liberty $SKY130_LIB
  opt_clean -purge
  write_verilog -noattr counter_syn.v
"
cells=$(grep -cE 'sky130_fd_sc_hd__' counter_syn.v)

cat > counter.pwr <<J
design:   counter
netlist:  counter_syn.v
lib:      $SKY130_LIB
clock:    clk 10.0
vdd:      1.8
activity: 0.2
J

echo
echo "OpenSTA report_power vs vyges-power  (sky130 hd, $cells cells, activity 0.2):"
echo
sta -no_init -exit report_power.tcl 2>&1 | awk '/^Total/{
  printf "  OpenSTA      leakage %s  internal %s  switching %s  total %s\n",$4,$2,$3,$5}'
"$PWR" run counter.pwr --json 2>&1 \
  | awk -F'[:,]' '
      /"leakage_w"/{l=$2} /"internal_w"/{i=$2} /"switch_w"/{s=$2} /"total_w"/{t=$2}
      END{printf "  vyges-power  leakage%s  internal%s  switching%s  total%s\n",l,i,s,t}'
echo
echo "Expected (this DUT): leakage matches ~0.2%; internal within ~2x; switching"
echo "low pre-layout (no extracted wire caps — feed a routed-block SPEF to close it)."
