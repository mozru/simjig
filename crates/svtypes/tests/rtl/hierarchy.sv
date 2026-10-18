// The instance tree and body variables the IR records for build-time probe checks.
module hierarchy #(parameter int P = 2) (input logic clk, output logic [7:0] y);
  localparam logic [7:0] LP = 8'h5a;
  logic [7:0] r;
  hier_leaf u_leaf (.clk(clk), .q(y));
  always_ff @(posedge clk) r <= LP + 8'(P);
endmodule

module hier_leaf (input logic clk, output logic [7:0] q);
  logic [7:0] s;
  always_ff @(posedge clk) begin
    s <= s + 8'd1;
    q <= s;
  end
endmodule
