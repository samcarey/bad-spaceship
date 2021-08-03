use bevy::prelude::*;
use bevy_rapier3d::na::{Translation3, Vector3};
pub use platform::*;
use std::f32::consts::PI;

pub const DEG_TO_RADIANS: f32 = PI / 180.;

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

    pub mod html {
        use web_sys::{Document, HtmlElement};

        pub fn get_document() -> Document {
            let window = web_sys::window().expect("no global `window` exists");
            let document = window.document().expect("should have a document on window");
            document
        }

        pub fn get_body() -> HtmlElement {
            get_document().body().expect("document should have a body")
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

pub trait Vec3Ext {
    fn to_vector(&self) -> Vector3<f32>;
    fn to_translation3(&self) -> Translation3<f32>;
}

// impl Vec3Ext for Vec3 {
//     fn to_vector(&self) -> Vector3<f32> {
//         Vector3::from(self.x, self.y, self.z)
//     }

//     fn to_translation3(&self) -> Translation3<f32> {
//         Translation3::new(self.x, self.y, self.z)
//     }
// }

// TODO: submit this to bevy project
pub trait TransformExt {
    fn forward(&self) -> Vec3;
    fn right(&self) -> Vec3;
    fn up(&self) -> Vec3;
}

impl TransformExt for Transform {
    fn forward(&self) -> Vec3 {
        self.local_z()
    }

    fn right(&self) -> Vec3 {
        -self.local_x()
    }

    fn up(&self) -> Vec3 {
        self.local_y()
    }
}

impl TransformExt for GlobalTransform {
    fn forward(&self) -> Vec3 {
        self.local_z()
    }

    fn right(&self) -> Vec3 {
        -self.local_x()
    }

    fn up(&self) -> Vec3 {
        self.local_y()
    }
}

use nalgebra::{Quaternion, Unit, UnitQuaternion};

pub trait QuatExt {
    fn to(&self, other: Quat) -> Quat;
    fn to_rotation_vector(&self) -> Vector3<f32>;
    fn to_quaternion(&self) -> Quaternion<f32>;
    fn to_unit_quaternion(&self) -> UnitQuaternion<f32>;
}

impl QuatExt for Quat {
    fn to(&self, other: Quat) -> Quat {
        (self.conjugate() * other).normalize()
    }

    fn to_rotation_vector(&self) -> Vector3<f32> {
        let (axis, angle) = self.to_axis_angle();
        (axis * angle).into()
    }

    fn to_quaternion(&self) -> Quaternion<f32> {
        Quaternion::new(self.w, self.x, self.y, self.z)
    }

    fn to_unit_quaternion(&self) -> UnitQuaternion<f32> {
        UnitQuaternion::from_quaternion(self.to_quaternion())
    }
}

pub trait QuaternionExt {
    fn to_quat(&self) -> Quat;
}

impl QuaternionExt for Unit<Quaternion<f32>> {
    fn to_quat(&self) -> Quat {
        Quat::from_xyzw(self.i, self.j, self.k, self.w)
    }
}

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

pub trait AtomicBoolExt {
    fn toggle(&self);
    fn set(&self, value: bool);
}

impl AtomicBoolExt for AtomicBool {
    fn toggle(&self) {
        self.store(!self.load(SeqCst), SeqCst);
    }

    fn set(&self, value: bool) {
        self.store(value, SeqCst);
    }
}
