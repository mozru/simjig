// Types carried over bus_if.
package iface_pkg;
  localparam int DATA_W = 16;

  typedef enum logic [1:0] {
    KIND_IDLE = 2'd0,
    KIND_DATA = 2'd1,
    KIND_LAST = 2'd2
  } kind_e;
endpackage
