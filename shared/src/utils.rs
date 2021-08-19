use std::f32::consts::PI;

use bevy::{
    math::{Quat, Vec3},
    prelude::{GlobalTransform, Transform},
};
use bevy_rapier3d::na::{Quaternion, Translation3, Unit, UnitQuaternion, Vector3};

pub const DEG_TO_RADIANS: f32 = PI / 180.;

pub trait Vec3Ext {
    fn to_vector(&self) -> Vector3<f32>;
    fn to_translation3(&self) -> Translation3<f32>;
}

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

#[cfg(not(target_arch = "wasm32"))]
#[macro_use]
mod platform {
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

#[cfg(target_arch = "wasm32")]
#[macro_use]
mod platform {
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
}
