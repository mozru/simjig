// Purely combinational path from `a` to `y` through four continuous assigns, so a write to
// `a` must settle through several dependent nets within one eval.
//   s1 = a + 1; s2 = s1 ^ 8'h5a; s3 = {s2[6:0], s2[7]} (rotate left); y = s3 + b
// `y_eq` is a 1-bit output that also depends on the whole chain.
module comb_chain (
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic [7:0] y,
    output logic       y_eq
);
  logic [7:0] s1, s2, s3;
  assign s1   = a + 8'd1;
  assign s2   = s1 ^ 8'h5a;
  assign s3   = {s2[6:0], s2[7]};
  assign y    = s3 + b;
  assign y_eq = (y == b);
endmodule
