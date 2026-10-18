// Codegen test design: a parameterised DUT with an interface port. The model top is the
// generated wrapper, which has no parameters of its own, so the DUT's parameters must be
// read from the DUT's module. `u_core.acc` is a probe target below the DUT.
interface r_if #(
    parameter int W = 4
) (
    input logic clk
);
  logic [W-1:0] d;
  modport m(input d, input clk);
endinterface

module r_core #(
    parameter int K = 1
) (
    input  logic         clk,
    input  logic [3:0]   d,
    output logic [K-1:0] q
);
  logic [K-1:0] acc;
  always_ff @(posedge clk) acc <= acc + K'(d);
  assign q = acc;
endmodule

module r_dut #(
    parameter int K = 1
) (
    r_if.m bus,
    output logic [K-1:0] q
);
  r_core #(.K(K)) u_core (
      .clk(bus.clk),
      .d(bus.d),
      .q(q)
  );
endmodule
