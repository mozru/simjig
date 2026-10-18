#[simjig::test(timeout = "2 days")]
async fn bad_unit() {}

#[simjig::test(timeout = 2)]
async fn not_a_string() {}

#[simjig::test(seed = 1)]
async fn unknown_argument() {}

fn main() {}
