fn main() {
    println!("cargo:rerun-if-changed=assets/favicon.ico");
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/favicon.ico");
        if let Err(err) = res.compile() {
            println!("cargo:warning=无法嵌入 exe 图标（rc.exe 不可用？）: {err}");
        }
    }
}
