// A tagged beat: `data` with a small `tag` beside it, moved in a cycle where `valid` is
// high. `beat_t`'s field widths follow the interface's parameters, so two configurations
// of a module carrying it have two `beat_t`s.
interface tagged_if #(
    parameter int WIDTH = 8,
    parameter int TAG_W = 2
);
  timeunit 1ns; timeprecision 1ps;

  typedef struct packed {
    logic [TAG_W-1:0] tag;
    logic [WIDTH-1:0] data;
  } beat_t;

  beat_t beat;
  logic  valid;

  modport source(output beat, output valid);
  modport sink(input beat, input valid);
endinterface
