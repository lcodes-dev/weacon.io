fn main() {
    weacon_version::emit();
    // Release manifests name each build by its target.
    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo::rustc-env=WEACON_TARGET={target}");
}
