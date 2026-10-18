// Module parameters overridden with -G, and a parameterised submodule specialised twice.
module sub #(
  parameter int N = 4,
  parameter type T = logic [N-1:0]
) (
  input  T x,
  output T y
);
  assign y = x;
endmodule

module top #(
  parameter int         W     = 8,
  parameter logic [3:0] MODE  = 4'd1,
  parameter string      LABEL = "default",
  parameter bit [69:0]  WIDE  = 70'h1
) (
  input  logic [W-1:0] a,
  output logic [W-1:0] y,
  output logic [2:0]   narrow
);
  sub #(.N(W)) s_wide  (.x(a), .y(y));
  sub #(.N(3)) s_narrow (.x(a[2:0]), .y(narrow));
endmodule
