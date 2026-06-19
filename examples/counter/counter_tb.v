// Testbench that drives the gate-level counter and dumps a SAIF over its nets
// via the simulator's --trace-saif — the real-activity input for vyges-power's
// `saif:` job key. Build + run (Verilator >= 5.0):
//
//   $ verilator --binary --trace-saif -Wno-fatal -Wno-DECLFILENAME \
//               counter_tb.v counter.v cells_sim.v
//   $ ./obj_dir/Vcounter_tb        # writes counter.saif
//
// run_loop.sh regenerates counter.saif this way when the simulator is on PATH,
// and otherwise uses the committed file.
`timescale 1ns / 1ps
module counter_tb;
  reg clk_in = 1'b0;
  reg d      = 1'b0;
  wire q0, q1, y;

  counter dut (.clk_in(clk_in), .d(d), .q0(q0), .q1(q1), .y(y));

  always #5 clk_in = ~clk_in;            // 100 MHz clock (toggles every 5 ns)

  integer i;
  initial begin
    $dumpfile("counter.saif");
    $dumpvars(0, counter_tb);
    for (i = 0; i < 40; i = i + 1) begin // 40 cycles = 400 ns window
      @(posedge clk_in);
      if (i % 3 == 0) d <= ~d;           // data switches every third cycle
    end
    $finish;
  end
endmodule
