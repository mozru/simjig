// Probe targets that are not packed vectors (tests/it/probes.rs): a `real` and a `string`,
// which Verilator stores as `double` and `std::string`.
module unpacked_vars (
    input  logic       clk,
    output logic [7:0] y
);
  real        r;
  string      s;
  logic [7:0] n;
  always_ff @(posedge clk) begin
    r <= r + 0.5;
    s <= "x";
    n <= n + 8'd1;
  end
  assign y = n;
endmodule
