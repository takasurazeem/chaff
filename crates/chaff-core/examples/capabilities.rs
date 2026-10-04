//! Print the Capability Report for this machine.
//!
//!     cargo run --release -p chaff-core --example capabilities [-- <endpoint-url> ...]
//!
//! The same report the application shows. Exists so the tier selection can be inspected on
//! a machine without opening the window — and so it can be run over SSH on a headless box,
//! which is where the GPU usually is.

use chaff_core::hardware;

fn main() {
    let urls: Vec<String> = std::env::args().skip(1).collect();

    let mut probe = hardware::probe();
    for url in &urls {
        probe.endpoints.push(hardware::probe_endpoint(url, 1500));
    }

    print!("{}", hardware::render(&probe));
}
