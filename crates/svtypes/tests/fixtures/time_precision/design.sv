// A design whose finest time precision is 10 fs.
`timescale 1ns / 1ps
module leaf (input logic a, output logic y);
  timeunit 1ps;
  timeprecision 10fs;
  assign y = a;
endmodule

module top (input logic a, output logic y);
  leaf u_leaf (.a(a), .y(y));
endmodule
