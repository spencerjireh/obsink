fn main() {
    // The default server is baked in; rebuild when it changes.
    println!("cargo:rerun-if-env-changed=OBSINK_SERVER_URL");
    tauri_build::build()
}
