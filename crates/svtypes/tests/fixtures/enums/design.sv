// Enums with implicit values, explicit values, an explicit base type, the default `int`
// base, a 2-state base and an anonymous enum port.
package pkg;
  typedef enum logic [2:0] { A, B, C } implicit_t;
  typedef enum logic [7:0] { OFF = 8'h00, ON = 8'h5a, ERR = 8'hff } explicit_t;
  typedef enum logic [3:0] { X0 = 2, X1, X2 = 9, X3 } mixed_t;
  typedef enum { RED, GREEN = 10, BLUE } int_t;
  typedef enum bit [1:0] { LO, HI } twostate_t;
endpackage

module top (
  input  pkg::implicit_t i_impl,
  input  pkg::explicit_t i_expl,
  input  pkg::mixed_t    i_mixed,
  input  pkg::int_t      i_int,
  input  pkg::twostate_t i_two,
  input  enum logic { NO, YES } i_anon,
  output pkg::explicit_t o_expl
);
  assign o_expl = i_expl;
endmodule
