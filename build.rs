fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/hdjlabs.ico")
            .set("ProductName", "HDJLabs")
            .set("FileDescription", "HDJLabs — secure offline exams")
            .set("CompanyName", "OPH")
            .set("LegalCopyright", "© OPH — MIT License");
        if let Err(e) = res.compile() {
            println!("cargo:warning=icon resource not embedded: {e}");
        }
    }
    println!("cargo:rerun-if-changed=assets/hdjlabs.ico");
}
