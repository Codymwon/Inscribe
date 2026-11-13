fn main() {
    println!("cargo:rustc-env=SLINT_STYLE=fluent-dark");
    println!("cargo:rerun-if-changed=ui/appwindow.slint");
}
