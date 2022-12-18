use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=logo.svg");
    println!("cargo:rerun-if-changed=favicon.ico");

    let doc_dir = "target/doc";

    if Path::new(doc_dir).is_dir() {
        fs::copy("logo.svg", format!("{}/logo.svg", doc_dir)).expect("Could not copy logo to target/doc");
        fs::copy("favicon.ico", format!("{}/favicon.ico", doc_dir)).expect("Could not copy favicon to target/doc");
    }
}
