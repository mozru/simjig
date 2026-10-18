// Every DPI-C argument and result type simjig implements, each result driven onto a port
// so the test sees what crossed the C ABI in both directions.
//   ty_ints:    the eight integer types in, a longint out, an int result (9 arguments);
//   ty_bits:    bit/logic scalars in, out and inout, a bit result;
//   ty_vec:     bit/logic vectors of 8, 41 and 64 bits in, out and inout, a bit [15:0] result;
//   ty_byte, ty_short, ty_ulong, ty_logic: the other result types;
//   ty_void:    a void function with an output; ty_aliased: a C alias;
//   ty_handle, ty_task: a chandle round trip and a context task, once per posedge clk.
module dpi_types (
    input  logic        clk,
    input  logic [31:0] a,
    input  logic [63:0] b,
    output logic [31:0] ints_ret,
    output logic [63:0] ints_sum,
    output logic [3:0]  bits_out,
    output logic [15:0] vec_ret,
    output logic [40:0] vec_o41,
    output logic [63:0] vec_io64,
    output logic [7:0]  vec_ol8,
    output logic [7:0]  r_byte,
    output logic [15:0] r_short,
    output logic [63:0] r_ulong,
    output logic        r_logic,
    output logic [31:0] void_out,
    output logic [31:0] aliased,
    output logic        handle_ok
);
  import "DPI-C" function int ty_ints(
      input byte b, input shortint s, input int i, input longint l, input byte unsigned ub,
      input shortint unsigned us, input int unsigned ui, input longint unsigned ul,
      output longint sum);
  import "DPI-C" function bit ty_bits(input bit b, input logic l, output bit ob,
                                      output logic ol, inout bit iob);
  import "DPI-C" function bit [15:0] ty_vec(
      input bit [7:0] v8, input logic [40:0] l41, input bit signed [63:0] s64,
      output bit [40:0] o41, inout logic [63:0] io64, output logic [7:0] ol8);
  import "DPI-C" function byte ty_byte(input byte x);
  import "DPI-C" function shortint ty_short(input shortint x);
  import "DPI-C" function longint unsigned ty_ulong(input longint unsigned x);
  import "DPI-C" function logic ty_logic(input logic x);
  import "DPI-C" function void ty_void(output int o);
  import "DPI-C" ty_c_alias = function int ty_aliased(input int x);
  import "DPI-C" function chandle ty_handle(input chandle c);
  import "DPI-C" context task ty_task(input int x);

  always_comb begin
    longint sum;
    bit ob, iob;
    logic ol;
    bit [40:0] o41;
    logic [63:0] io64;
    logic [7:0] ol8;
    int vo;
    ints_ret = ty_ints(a[7:0], a[15:0], a, b, a[7:0], a[15:0], a, b, sum);
    ints_sum = sum;
    iob = a[2];
    bits_out[3] = ty_bits(a[0], a[1], ob, ol, iob);
    bits_out[2:0] = {ob, ol, iob};
    io64 = b;
    vec_ret = ty_vec(a[7:0], {9'b0, a}, b, o41, io64, ol8);
    vec_o41 = o41;
    vec_io64 = io64;
    vec_ol8 = ol8;
    r_byte = ty_byte(a[7:0]);
    r_short = ty_short(a[15:0]);
    r_ulong = ty_ulong(b);
    r_logic = ty_logic(a[0]);
    ty_void(vo);
    void_out = vo;
    aliased = ty_aliased(a);
  end

  chandle h;  // null until the first posedge (IEEE 1800-2023 §6.14)
  always_ff @(posedge clk) begin
    chandle next;
    next = ty_handle(h);
    handle_ok <= next != null;
    h <= next;
    ty_task(a);
  end
endmodule
