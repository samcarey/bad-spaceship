use bevy::{
    math::{Quat, Vec3},
    prelude::{GlobalTransform, Transform},
};
use bevy_rapier3d::na::{Quaternion, Translation3, Unit, UnitQuaternion, Vector3};
use std::f32::consts::PI;

pub const DEG_TO_RADIANS: f32 = PI / 180.;

pub trait Vec3Ext {
    fn to_vector(&self) -> Vector3<f32>;
    fn to_translation3(&self) -> Translation3<f32>;
}

pub trait ToVec3 {
    fn to_vec3(self) -> Vec3;
}

impl ToVec3 for (f32, f32, f32) {
    fn to_vec3(self) -> Vec3 {
        Vec3::new(self.0, self.1, self.2)
    }
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
