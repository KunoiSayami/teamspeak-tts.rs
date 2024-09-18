fn main() {
    #[cfg(not(debug_assertions))]
    println!("cargo::rerun-if-changed=src/html/index.html");
}
