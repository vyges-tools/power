// Gate-level counter matching examples/counter/counter.def — the same instances
// (clkbuf, ff0, ff1, u0, u1) and the nets clk + n0 that vyges-extract extracts,
// so clkbuf's output net (clk) and u0's output net (n0) get REAL extracted wire
// caps from counter.spef; the other output nets fall back to the stand-in.
module counter (clk_in, d, q0, q1, y);
  input  clk_in, d;
  output q0, q1, y;
  wire clk, n0;
  CLKBUF clkbuf (.A(clk_in), .X(clk));     // drives net clk  (SPEF: 0.4565 fF)
  INV    u0     (.A(d),      .Y(n0));       // drives net n0   (SPEF: 0.0869 fF)
  BUF    u1     (.A(n0),     .Y(y));
  DFF    ff0    (.CK(clk),   .D(d),  .Q(q0));
  DFF    ff1    (.CK(clk),   .D(n0), .Q(q1));
endmodule
