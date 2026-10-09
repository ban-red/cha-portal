fn main() {
    // `sqlx::migrate!` embeds the migrations at compile time but cargo only
    // tracks the files it saw, so a new one needs this to trigger a rebuild.
    println!("cargo:rerun-if-changed=migrations");
}
