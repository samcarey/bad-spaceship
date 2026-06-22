use std::marker::PhantomData;

use bevy::{
    asset::{io::Reader, AssetLoader, LoadContext},
    prelude::*,
};
use serde::Deserialize;

use crate::{character, player};

pub struct ConfigPlugin;

impl Plugin for ConfigPlugin {
    fn build(&self, app: &mut App) {
        // Bevy 0.12's typed asset system picks a loader purely by file
        // extension, so the single dynamic loader of 0.11 (which produced either
        // config type from any `.ron`) no longer works. Register one typed loader
        // per config under its own extension; the matching asset files are named
        // `*.character.ron` / `*.player.ron` so each resolves to the right type.
        app.register_asset_loader(RonConfigLoader::<character::Config>::new("character.ron"))
            .register_asset_loader(RonConfigLoader::<player::Config>::new("player.ron"));
    }
}

/// Generic RON asset loader for a single config type, registered under a
/// type-specific extension.
struct RonConfigLoader<T> {
    extension: &'static str,
    _marker: PhantomData<fn() -> T>,
}

impl<T> RonConfigLoader<T> {
    fn new(extension: &'static str) -> Self {
        Self {
            extension,
            _marker: PhantomData,
        }
    }
}

impl<T> AssetLoader for RonConfigLoader<T>
where
    T: Asset + for<'de> Deserialize<'de>,
{
    type Asset = T;
    type Settings = ();
    // 0.12 dropped the hard `anyhow` dependency on loaders, but any
    // `Into<Box<dyn Error>>` works — reuse `anyhow` (already a dep) for brevity.
    type Error = anyhow::Error;

    // Bevy 0.15 simplified `AssetLoader::load` to fully elided lifetimes and turned
    // `Reader` into a trait object (`&mut dyn Reader`) — no more explicit `'a` tying
    // the arguments together (0.14 had `&'a mut Reader<'_>`).
    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        _load_context: &mut LoadContext<'_>,
    ) -> Result<T, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        Ok(ron::de::from_bytes::<T>(&bytes)?)
    }

    fn extensions(&self) -> &[&str] {
        std::slice::from_ref(&self.extension)
    }
}
