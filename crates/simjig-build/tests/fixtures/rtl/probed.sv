// Probe targets for the build-time probe checks (tests/it/probes.rs): a localparam, a
// parameter and an internal signal of a submodule.
module lp #(parameter int P = 3) (
    input  logic       clk,
    output logic [7:0] y
);
  localparam logic [7:0] LP = 8'h5a;
  logic [7:0] acc;
  lp_core u_core (.clk(clk), .y(y));
  always_ff @(posedge clk) acc <= acc + LP + 8'(P);
endmodule

module lp_core (
    input  logic       clk,
    output logic [7:0] y
);
  logic [7:0] result;
  always_ff @(posedge clk) result <= result + 8'd1;
  assign y = result;
endmodule
