#[derive(Clone, Copy)]
pub struct Args {
    pub is_server: bool,
}

#[cfg(target_arch = "wasm32")]
#[macro_use]
mod platform {
    use super::Args;

    pub fn parse_args() -> Args {
        Args { is_server: false }
    }

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

    pub mod html_body {
        use web_sys::HtmlElement;

        pub fn get() -> HtmlElement {
            let window = web_sys::window().expect("no global `window` exists");
            let document = window.document().expect("should have a document on window");
            let body = document.body().expect("document should have a body");
            body
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[macro_use]
mod platform {
    use super::Args;

    pub fn parse_args() -> Args {
        Args {
            is_server: std::env::args().any(|arg| ["--server", "-s"].contains(&&*arg)),
        }
    }

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
}

pub use platform::*;

use bevy::prelude::*;
use rapier3d::math::{Translation, Vector};

pub trait Vec3Ext {
    fn to_vector(&self) -> Vector<f32>;
    fn to_translation3(&self) -> Translation<f32>;
}

impl Vec3Ext for Vec3 {
    fn to_vector(&self) -> Vector<f32> {
        Vector::new(self.x, self.y, self.z)
    }

    fn to_translation3(&self) -> Translation<f32> {
        Translation::new(self.x, self.y, self.z)
    }
}

// TODO: submit this to bevy project
pub trait TransformExt {
    fn right(&self) -> Vec3;
    fn up(&self) -> Vec3;
}

impl TransformExt for Transform {
    fn right(&self) -> Vec3 {
        self.rotation * Vec3::unit_x()
    }

    fn up(&self) -> Vec3 {
        self.rotation * Vec3::unit_y()
    }
}

use nalgebra::Quaternion;

pub trait QuatExt {
    fn to(&self, other: Quat) -> Quat;
    fn to_rotation_vector(&self) -> Vector<f32>;
    fn to_quaternion(&self) -> Quaternion<f32>;
}

impl QuatExt for Quat {
    fn to(&self, other: Quat) -> Quat {
        (self.conjugate() * other).normalize()
    }

    fn to_rotation_vector(&self) -> Vector<f32> {
        let (axis, angle) = self.to_axis_angle();
        axis.to_vector() * angle
    }

    fn to_quaternion(&self) -> Quaternion<f32> {
        Quaternion::new(self.w, self.x, self.y, self.z)
    }
}
