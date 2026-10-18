// A `sync_fifo` of `fifo_pkg::item_t`: the same FIFO with typed ports, so the testbench
// drives and checks `Item` structs instead of raw bits. See sync_fifo.sv for the
// handshake; DEPTH and FWFT pass through.
module item_fifo
  import fifo_pkg::*;
#(
    parameter int DEPTH = 8,
    parameter bit FWFT  = 1'b1
) (
    input  logic                   clk,
    input  logic                   rst,
    input  logic                   push,
    input  item_t                  wr_item,
    input  logic                   pop,
    output item_t                  rd_item,
    output logic                   full,
    output logic                   empty,
    output logic [$clog2(DEPTH):0] count
);
  sync_fifo #(
      .WIDTH($bits(item_t)),
      .DEPTH(DEPTH),
      .FWFT (FWFT)
  ) u_fifo (
      .clk,
      .rst,
      .push,
      .wr_data(wr_item),
      .pop,
      .rd_data(rd_item),
      .full,
      .empty,
      .count
  );
endmodule
