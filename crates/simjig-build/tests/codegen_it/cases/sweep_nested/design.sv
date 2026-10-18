// Sweep case: a three-level chain of module-local structs. `inner_t` is 16 bits in every
// configuration but splits them by `W`; `mid_t` and `outer_t` are then alike (same text,
// same width) in every configuration, yet name a type that is not, so neither may be
// shared. `tag_t`/`wrap_t` follow no parameter and are. The port `clone` and the fields
// of `odd_t` are named like methods the generated traits already have.
module nx_top (o, wo, clone, od);
  parameter int W = 8;
  typedef struct packed { logic [W-1:0] a; logic [15-W:0] b; } inner_t;
  typedef struct packed { inner_t i; logic v; } mid_t;
  typedef struct packed { mid_t m; logic u; } outer_t;
  typedef struct packed { logic [3:0] t; } tag_t;
  typedef struct packed { tag_t t; logic v; } wrap_t;
  typedef struct packed { logic [W-1:0] eq; logic [3:0] with_x; logic x; } odd_t;
  output outer_t o;
  output wrap_t wo;
  input logic [W-1:0] clone;
  output odd_t od;
  assign o = '0;
  assign wo = '0;
  assign od = '0;
endmodule
