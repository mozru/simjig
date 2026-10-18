// Types carried by `item_fifo`: a packed-struct payload, which simjig generates as the
// Rust struct `fifo_pkg::Item` (here `item_fifo::Item`) with typed fields.
package fifo_pkg;
  timeunit 1ns; timeprecision 1ps;

  localparam int TAG_W  = 6;
  localparam int DATA_W = 16;

  // What a beat carries; any 2-bit value is legal on the wire.
  typedef enum logic [1:0] {
    KIND_DATA = 2'd0,
    KIND_CTRL = 2'd1,
    KIND_LAST = 2'd2,
    KIND_ERR  = 2'd3
  } kind_e;

  // Packed, MSB first: {kind[1:0], tag[5:0], data[15:0]} = 24 bits (IEEE 1800-2023 §7.2.1).
  typedef struct packed {
    kind_e             kind;
    logic [TAG_W-1:0]  tag;
    logic [DATA_W-1:0] data;
  } item_t;
endpackage
