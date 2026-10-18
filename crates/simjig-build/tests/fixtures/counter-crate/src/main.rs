//! Prints the counter value after the number of cycles given as the first argument.

unsafe extern "C" {
    safe fn counter_run(cycles: u32) -> u32;
}

fn main() {
    let cycles = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(0);
    println!("{}", counter_run(cycles));
}
