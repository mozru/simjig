// Combinational inverter: the smallest design with an input-to-output path.
module inv (
    input  logic a,
    output logic y
);
  assign y = ~a;
endmodule
