// `DEPTH` (a define) sets a port width and a parameter, so a define that the JSON pass and
// the model build read differently shows up as a header mismatch or a wrong const.
module defines #(parameter int D = `DEPTH) (
  output logic [`DEPTH-1:0] q
);
  assign q = '0;
endmodule
