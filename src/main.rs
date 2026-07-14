mod app;
mod claude;
mod ssh;
mod tmux;
mod ui;

#[cfg(target_arch = "wasm32")]
fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(app::App);
}

// Native build exists only so `cargo test` can run the pure parsers.
#[cfg(not(target_arch = "wasm32"))]
fn main() {}
