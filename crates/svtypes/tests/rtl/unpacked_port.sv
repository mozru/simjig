// A port of an unpacked struct type: it has no C++ storage simjig supports.
module unpacked_port (
    input logic clk,
    input struct { logic [3:0] a; logic b; } s
);
endmodule
