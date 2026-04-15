pub mod detect;
pub mod raw;
pub mod registry;
pub mod standard;
pub mod video;

// TODO: Make these private once RenderPipeline builds the registry internally.
pub use raw::RawHandler;
pub use registry::FormatRegistry;
pub use standard::StandardHandler;
pub use video::VideoHandler;
