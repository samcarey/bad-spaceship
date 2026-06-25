fn main() -> shadow_rs::SdResult<()> {
    // The build/version identity (short git SHA) now comes from the `shared`
    // crate's build.rs as `bad_spaceship_shared::net::BS_VERSION`, kept in lockstep
    // with the netcode protocol id (and honoring the deploy's BS_BUILD_SHA).
    // shadow-rs still provides BUILD_TIME etc. used in the in-game overlay.
    shadow_rs::new()
}
