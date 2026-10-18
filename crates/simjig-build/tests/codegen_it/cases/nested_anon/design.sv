// Codegen test design: anonymous packed aggregates nested inside other types. Verilator
// 5.052 crashes (V3LinkParse "Implicit enum/struct type created under unexpected node
// type") when such a type is spelled inline in `$bits(..)`, so the layout check must name
// every aggregate with a local typedef.
package np;
  typedef enum logic [1:0] {
    GAP_NONE  = 2'd0,
    GAP_SHORT = 2'd1,
    GAP_LONG  = 2'd3
  } gap_e;

  // 2 + 4 + 4 = 10 bits; `un` is an anonymous union inside a named struct.
  typedef struct packed {
    gap_e       e;
    logic [0:3] asc;
    union packed {
      logic [3:0]      u;
      logic [1:0][1:0] v;
    } un;
  } s_t;
endpackage

module nested_top (
    input  np::s_t in_s,
    // An anonymous struct port, itself holding an anonymous struct and union.
    input  struct packed {
      logic a;
      struct packed {
        logic [2:0] x;
        logic       y;
      } inner;
      union packed {
        logic [1:0] p;
        logic [1:0] q;
      } b;
    } in_anon,
    output np::s_t out_s
);
  assign out_s = in_s;
endmodule
