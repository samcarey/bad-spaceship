#[derive(Clone, Copy)]
pub struct Args {
    pub is_server: bool,
}

pub fn parse_args() -> Args {
    cfg_if::cfg_if! {
        if #[cfg(target_arch = "wasm32")] {
            let is_server = false;
        } else {
            let is_server = std::env::args().any(|arg| ["--server", "-s"].contains(&&*arg));
        }
    }

    Args { is_server }
}
