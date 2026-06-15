# OpenSTA report_power on the sky130-mapped counter. $SKY130_LIB -> the hd .lib.
read_liberty $env(SKY130_LIB)
read_verilog counter_syn.v
link_design counter
create_clock -name clk -period 10 [get_ports clk]
set_input_transition 0.15 [all_inputs]
set_load 0.05 [all_outputs]
# global switching activity 0.2 — match vyges-power's vectorless `activity: 0.2`
set_power_activity -global -activity 0.2 -duty 0.5
report_power
exit
