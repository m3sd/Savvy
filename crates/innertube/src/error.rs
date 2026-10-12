pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("innertube: {0}")]
    InnerTube(#[from] innertube_rs::error::InnertubeError),

    #[error("http: {0}")]
    Http(#[from] reqwest::Error),

    #[error("unsupported: {0}")]
    Unsupported(String),
}
