// Typedef chains across $unit, a package and a module.
typedef logic [5:0] unit_word_t;
typedef unit_word_t unit_alias_t;

package pkg;
  typedef struct packed { logic [3:0] hi; logic [3:0] lo; } base_t;
  typedef base_t       alias1_t;
  typedef alias1_t     alias2_t;
  typedef logic [9:0]  vec_t;
  typedef vec_t        vec_alias_t;
endpackage

module top (
  input  pkg::alias2_t    a2,
  input  pkg::alias1_t    a1,
  input  pkg::base_t      b,
  input  pkg::vec_alias_t v,
  input  unit_alias_t     u,
  output logic [7:0]      y
);
  typedef pkg::alias2_t local_t;
  local_t l;
  assign l = a2;
  assign y = l ^ a1 ^ b ^ v[7:0] ^ {2'b0, u};
endmodule
