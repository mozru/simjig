// A packed struct, a struct nesting it and an enum, a packed union, a signed struct and an
// anonymous struct port.
package pkg;
  typedef enum logic [1:0] { IDLE, BUSY, DONE } state_t;
  typedef struct packed {
    logic [3:0]         tag;
    logic signed [11:0] data;
  } inner_t;
  typedef struct packed {
    inner_t     in;
    state_t     st;
    logic       valid;
    logic [2:0] pad;
  } outer_t;
  typedef union packed {
    logic [15:0]     word;
    logic [1:0][7:0] bytes;
    inner_t          as_inner;
  } word_u;
  typedef struct packed signed {
    logic [3:0] hi;
    logic [3:0] lo;
  } sdata_t;
endpackage

module top (
  input  pkg::inner_t in_i,
  input  pkg::outer_t in_o,
  input  pkg::word_u  in_u,
  input  pkg::sdata_t in_s,
  input  struct packed { logic a; logic [2:0] b; } in_anon,
  output pkg::outer_t out_o
);
  assign out_o = in_o;
endmodule

