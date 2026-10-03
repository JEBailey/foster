fn main() {
    println!("cargo:rustc-check-cfg=cfg(foster_bootstrap)");
    println!("cargo:rustc-cfg=foster_bootstrap");
}
