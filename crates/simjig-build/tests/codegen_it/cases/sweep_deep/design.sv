// A sweep case: three-level nesting, one struct at two depths and as a port, a field
// type shared with a flat port, a field concrete in every member, two interface ports.
interface dp_if #(parameter int W = 8);
  typedef struct packed { logic [W-1:0] a; logic [3:0] k; } leaf_t;
  typedef struct packed { leaf_t x; leaf_t y; logic [W:0] b; } mid_t;
  mid_t beat;
  logic [W-1:0] side;
  modport sink(input beat, side);
endinterface
module dp_top #(parameter int W = 8) (
    dp_if.sink p,
    dp_if.sink q,
    input  logic [W-1:0] din,
    output logic [W:0] bout
);
  assign bout = {1'b0, din};
endmodule
module dq_top (top, lf, din, bout);
  parameter int W = 8;
  typedef struct packed { logic [W-1:0] a; logic [3:0] k; } leaf_t;
  typedef struct packed { leaf_t x; leaf_t y; logic [W:0] b; } mid_t;
  typedef struct packed { mid_t m; leaf_t l; logic [W-1:0] a2; } top_t;
  input top_t top;
  output leaf_t lf;
  input logic [W-1:0] din;
  output logic [W:0] bout;
  assign lf = top.l;
  assign bout = {1'b0, din};
endmodule
