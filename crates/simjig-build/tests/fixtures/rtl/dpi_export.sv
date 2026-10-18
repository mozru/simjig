// Exports a DPI-C function. Two models of it in one binary would define the same global
// C symbol, so the build script must refuse to build it twice under different names.
module dpi_export (
  input  logic clk,
  output int   o
);
  export "DPI-C" function dpi_export_get;
  function int dpi_export_get();
    return o;
  endfunction
  always_ff @(posedge clk) o <= o + 1;
endmodule
