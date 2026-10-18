// Package localparams of every supported kind, and a typedef sized by one of them.
package cfg;
  localparam int          WIDTH  = 12;
  localparam int          NEG    = -5;
  localparam logic [7:0]  MAGIC  = 8'hA5;
  localparam logic [99:0] BIG    = 100'h8_0000_0000_0000_0000_0000_0001;
  localparam bit          FLAG   = 1'b1;
  localparam string       NAME   = "simjig";
  localparam real         RATIO  = 0.25;
  parameter  int          DEPTH  = WIDTH * 2;
  typedef logic [WIDTH-1:0] word_t;
endpackage

module top (
  input  cfg::word_t           data,
  output logic [cfg::DEPTH-1:0] wide
);
  assign wide = {data, data};
endmodule
