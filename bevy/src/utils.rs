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

cfg_if::cfg_if! {
    if #[cfg(target_arch = "wasm32")] {
        #[macro_export]
        macro_rules! config_from_file {
            ($filepath: literal) => {
                ron::from_str(
                    &crate::CONFIG_DIR
                        .get_file($filepath)
                        .unwrap()
                        .contents_utf8()
                        .unwrap()[..],
                )
                .unwrap()
            };
        }
    } else {
        #[macro_export]
        macro_rules! config_from_file {
            ($filepath: literal) => {
                ron::from_str(
                    &std::fs::read_to_string(
                        std::path::Path::new("assets/config").join($filepath)
                    ).unwrap()[..]
                ).unwrap()
            };
        }
    }
}
