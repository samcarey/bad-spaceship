use bevy::{
    asset::{AssetDynamic, AssetLoader, LoadedAsset},
    prelude::*,
    reflect::TypeUuid,
};
use serde::Deserialize;

use crate::{character, player};

pub struct ConfigPlugin;

impl Plugin for ConfigPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset_loader::<ConfigAssetLoader>();
    }
}

fn load_ron<'a, T: TypeUuid + AssetDynamic + Deserialize<'a>>(
    bytes: &'a [u8],
    load_context: &'a mut bevy::asset::LoadContext,
) -> Result<(), ron::Error> {
    let custom_asset = ron::de::from_bytes::<T>(bytes)?;
    load_context.set_default_asset(LoadedAsset::new(custom_asset));
    Ok(())
}

#[derive(Default)]
pub struct ConfigAssetLoader;

impl AssetLoader for ConfigAssetLoader {
    fn load<'a>(
        &'a self,
        bytes: &'a [u8],
        load_context: &'a mut bevy::asset::LoadContext,
    ) -> bevy::asset::BoxedFuture<'a, Result<(), anyhow::Error>> {
        Box::pin(async move {
            if load_ron::<character::Config>(bytes, load_context).is_ok()
                | load_ron::<player::Config>(bytes, load_context).is_ok()
            {
                Ok(())
            } else {
                Err(anyhow::Error::msg(format!(
                    "Failed to load config: {}",
                    load_context.path().to_string_lossy(),
                )))
            }
        })
    }

    fn extensions(&self) -> &[&str] {
        &["ron"]
    }
}
