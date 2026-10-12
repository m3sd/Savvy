//! Savvy's InnerTube client. Thin wrapper over `innertube-rs` exposing
//! only the surfaces the Tauri core actually needs.

mod client;
pub mod dto;
mod error;

pub use client::InnerTube;
pub use dto::{
    AdaptiveFormatDto, AuthorDto, CaptionTrack, CaptionTrackDto, SearchItemDto, SearchResultsDto,
    StreamUrlsDto, ThumbnailDto, VideoDetailsDto, VideoInfoDto, VideoSearchItemDto,
};
pub use error::{Error, Result};
