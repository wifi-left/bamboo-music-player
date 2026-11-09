fn main() {
    println!("cargo:rerun-if-changed=assets/favicon.ico");
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/favicon.ico");
        res.set("FileDescription", "Bamboo Music Player");
        res.set("ProductName", "Bamboo Music Player");
        res.set("OriginalFilename", "bamboo-player.exe");
        res.set(
            "LegalCopyright",
            "Copyright (c) 2026 wifi-left. Bamboo Music Player Non-Commercial License 1.0",
        );
        // Windows version resources require four numeric components.
        let version = env!("CARGO_PKG_VERSION");
        let mut parts: Vec<&str> = version.split('.').collect();
        while parts.len() < 4 {
            parts.push("0");
        }
        let numeric = parts.into_iter().take(4).collect::<Vec<_>>().join(".");
        res.set("FileVersion", &numeric);
        res.set("ProductVersion", &numeric);
        if let Err(err) = res.compile() {
            println!("cargo:warning=无法嵌入 exe 图标（rc.exe 不可用？）: {err}");
        }
    }
}
