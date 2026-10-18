// Probe paths through generate blocks (tests/it/probes.rs): a named and an unnamed
// generate-if, a generate-for, and an instance array.
module gen_pipe #(parameter bit EN = 1) (
    input  logic       clk,
    output logic [7:0] y
);
  if (EN) begin : g_x
    gen_leaf u_x (.clk(clk), .acc(y));
  end
  // Unnamed: IEEE 1800-2023 §27.6 names it `genblk2` (the second generate construct).
  if (EN) begin
    gen_leaf u_anon (.clk(clk), .acc());
  end
  for (genvar i = 0; i < 2; i++) begin : g
    gen_leaf u (.clk(clk), .acc());
    logic [3:0] t;
    always_ff @(posedge clk) t <= t + 4'(i);
  end
  gen_leaf arr[1:0] (.clk(clk), .acc());
endmodule

module gen_leaf (
    input  logic       clk,
    output logic [7:0] acc
);
  logic [7:0] s;
  always_ff @(posedge clk) begin
    acc <= acc + 8'd1;
    s   <= acc;
  end
endmodule
