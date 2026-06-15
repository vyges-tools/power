// Correlation DUT: a small synchronous counter, synthesized to sky130 and run
// through both OpenSTA report_power and vyges-power on the SAME mapped netlist.
module counter #(parameter WIDTH = 8) (
  input  wire             clk,
  input  wire             rst_n,
  input  wire             enable,
  output reg  [WIDTH-1:0] count
);
  always @(posedge clk or negedge rst_n)
    if (!rst_n)      count <= {WIDTH{1'b0}};
    else if (enable) count <= count + 1'b1;
endmodule
