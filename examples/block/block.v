// Tiny gate-level block: NAND -> INV -> DFF. Matches tiny.lib + block.vcd.
module block (clk, a, b, q);
  input clk, a, b;
  output q;
  wire n1, n2;
  NAND2 u_nand (.A(a), .B(b), .Y(n1));
  INV   u_inv  (.A(n1), .Y(n2));
  DFF   u_ff   (.CK(clk), .D(n2), .Q(q));
endmodule
