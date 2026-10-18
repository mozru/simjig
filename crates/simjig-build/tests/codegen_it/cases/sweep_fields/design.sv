// Sweep case for fields traits. `fs_top`: a port and a struct field of one type in every
// configuration (`dout`, `beat.data`), through interfaces `a_if` and `b_if`, each with its
// own `beat_t`. `fn_top`: a struct port nesting the type of another (`o.i`, `io`), a
// field named `raw`, like `Packed::Raw`, and a field and a port named like blanket-impl
// methods (`into`, `from`).
interface a_if #(parameter int W = 8);
  typedef struct packed { logic [W-1:0] data; logic [W:0] tag; } beat_t;
  beat_t beat;
  modport sink(input beat);
endinterface
interface b_if #(parameter int W = 8);
  typedef struct packed { logic [W-1:0] x; logic y; } beat_t;
  beat_t beat;
  modport sink(input beat);
endinterface
module fs_top #(parameter int W = 8) (
    a_if.sink pa,
    b_if.sink pb,
    input  logic [W-1:0] din,
    output logic [W-1:0] dout
);
  assign dout = din;
endmodule
module fn_top (o, io, rw, from);
  parameter int W = 8;
  typedef struct packed { logic [W-1:0] a; } inner_t;
  typedef struct packed { inner_t i; logic v; } outer_t;
  typedef struct packed { logic [W-1:0] raw; logic [W:0] other; logic into; } rw_t;
  output outer_t o;
  output inner_t io;
  output rw_t rw;
  output logic from;
  assign o = '0;
  assign io = '0;
  assign rw = '0;
  assign from = 1'b0;
endmodule
