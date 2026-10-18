// A valid/ready bus. `src` drives the payload; `dst` consumes it.
interface bus_if;
  import iface_pkg::*;

  logic              valid;
  logic              ready;
  logic [DATA_W-1:0] data;
  kind_e             kind;

  modport src(output valid, output data, output kind, input ready);
  modport dst(input valid, input data, input kind, output ready);
endinterface
