// Behavioral models for the counter's gate cells, so the gate-level netlist
// (counter.v) is simulatable by Verilator to emit a SAIF over its REAL nets
// (clk, n0, y, q0, q1) via --trace-saif. For activity generation only — these
// are simulation stand-ins, not the characterized cells in counter.lib.
module CLKBUF (input A, output X); assign X = A; endmodule
module BUF    (input A, output Y); assign Y = A; endmodule
module INV    (input A, output Y); assign Y = ~A; endmodule
module DFF    (input CK, input D, output reg Q);
  initial Q = 1'b0;
  always @(posedge CK) Q <= D;
endmodule
