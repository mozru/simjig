// DPI-C imports of every type class the IR distinguishes, plus an alias, a package import,
// a typedef'd argument, unsupported types and an export.
package dpi_pkg;
  import "DPI-C" function int pkg_fn(input int x);
endpackage

module top (
    input  logic        clk,
    input  logic [31:0] a,
    output logic [31:0] y
);
  typedef bit [7:0] byte_t;
  typedef struct packed { logic [3:0] hi; logic [3:0] lo; } pair_t;

  import "DPI-C" function int f_int(input int x, output int o, inout int io);
  import "DPI-C" function byte f_ints(input byte b, input shortint s, input longint l,
                                      input byte unsigned ub, input int unsigned ui,
                                      input longint unsigned ul, input shortint unsigned us);
  import "DPI-C" function bit f_scalars(input bit b, input logic l, output bit ob,
                                        output logic ol);
  import "DPI-C" function logic f_vectors(input byte_t v8, input logic [7:0] l8,
                                          input bit [40:0] v41, input logic signed [63:0] l64,
                                          input bit [0:0] v1, input bit [64:0] v65);
  import "DPI-C" function bit [15:0] f_ret16(output bit [7:0] o8, inout logic [40:0] io41);
  import "DPI-C" function chandle f_chandle(input chandle c);
  import "DPI-C" function void f_void();
  import "DPI-C" context task t_task(input int x);
  import "DPI-C" c_alias = function int f_aliased(input int x);
  import "DPI-C" function void f_unsupported(input string s, input real r, input pair_t p,
                                             input int arr[2]);
  import dpi_pkg::*;

  export "DPI-C" function e_fn;
  function int e_fn(int x);
    return x + 1;
  endfunction

  always_ff @(posedge clk) begin
    int o, io;
    bit ob;
    logic ol;
    bit [7:0] o8;
    logic [40:0] io41;
    chandle c;
    int arr[2];
    io = 0;
    io41 = '0;
    y <= f_int(a, o, io) + 32'(f_ints(8'(a), 16'(a), 64'(a), 8'(a), a, 64'(a), 16'(a)))
       + 32'(f_scalars(a[0], a[1], ob, ol))
       + 32'(f_vectors(8'(a), 8'(a), 41'(a), 64'(a), a[0], 65'(a)))
       + 32'(f_ret16(o8, io41)) + f_aliased(a) + pkg_fn(a);
    c = f_chandle(c);
    f_void();
    t_task(a);
    f_unsupported("s", 1.0, '0, arr);
  end
endmodule
