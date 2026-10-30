fn main() {
    println!("cargo:rerun-if-changed=logo/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("logo/icon.ico");
        if let Err(e) = res.compile() {
            eprintln!("Failed to compile Windows resource: {e}");
        }
    }
}
