// A valid/ready stream. A beat moves at a posedge where `valid && ready`. The source
// holds `valid` and `data` steady until the beat moves, and never waits for `ready` to
// raise `valid`; the sink may raise or drop `ready` at any cycle.
interface stream_if #(
    parameter int WIDTH = 8
);
  timeunit 1ns; timeprecision 1ps;

  logic             valid;
  logic             ready;
  logic [WIDTH-1:0] data;

  modport source(output valid, output data, input ready);
  modport sink(input valid, input data, output ready);
endinterface
