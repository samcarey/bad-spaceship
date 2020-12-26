#[derive(Clone, Copy)]
pub struct Args {
    pub is_server: bool,
}

#[cfg(target_arch = "wasm32")]
pub fn parse_args() -> Args {
    Args { is_server: false }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn parse_args() -> Args {
    Args {
        is_server: std::env::args().any(|arg| ["--server", "-s"].contains(&&*arg)),
    }
}

#[cfg(target_arch = "wasm32")]
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

#[cfg(not(target_arch = "wasm32"))]
#[macro_export]
macro_rules! config_from_file {
    ($filepath: literal) => {
        ron::from_str(
            &std::fs::read_to_string(std::path::Path::new("assets/config").join($filepath))
                .unwrap()[..],
        )
        .unwrap()
    };
}

#[cfg(target_arch = "wasm32")]
pub mod html_body {
    use web_sys::HtmlElement;

    pub fn get() -> HtmlElement {
        let window = web_sys::window().expect("no global `window` exists");
        let document = window.document().expect("should have a document on window");
        let body = document.body().expect("document should have a body");
        body
    }
}
