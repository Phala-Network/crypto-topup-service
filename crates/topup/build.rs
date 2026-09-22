//! Build-time migration change tracking.

fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
