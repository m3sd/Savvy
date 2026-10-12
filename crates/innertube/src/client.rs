use std::time::Duration;

use innertube_rs::error::InnertubeError;
use innertube_rs::{
    FormatFilter, FormatType, Innertube, PlaylistView, QualityPreference, SearchFilters,
    SearchResults, SearchSuggestionsResult, Session, StreamingFormat,
};
use std::sync::Arc;

#[derive(Clone)]
pub struct InnerTube {
    inner: Innertube,
    http: reqwest::Client,
}

impl InnerTube {
    pub async fn new() -> Result<Self, InnertubeError> {
        Self::new_with_dir(None).await
    }

    /// Builds a client, reusing a `visitorData` from `dir` when one is present.
    ///
    /// `visitorData` is YouTube's notion of "which visitor is this". Generating a
    /// fresh one per launch — which `Innertube::new()` does — makes the app look
    /// like a brand-new anonymous client every single time it starts, with no
    /// history and no returning identity. A real browser keeps one for months.
    /// Persisting it costs one file and makes the traffic pattern considerably
    /// less anomalous, which matters directly to the bot check that blocks
    /// playback.
    ///
    /// If the stored value is missing or unreadable this silently falls back to a
    /// generated one: a bad identity is not worth failing startup over.
    pub async fn new_with_dir(dir: Option<&std::path::Path>) -> Result<Self, InnertubeError> {
        let (path, visitor_data) = match dir {
            Some(d) => {
                let p = d.join("visitor_data.txt");
                let stored = std::fs::read_to_string(&p).ok().map(|s| s.trim().to_string());
                let usable = stored.filter(|s| !s.is_empty());
                if usable.is_none() {
                    tracing::debug!("no reusable visitorData at {}", p.display());
                }
                (Some(p), usable)
            }
            None => (None, None),
        };

        let inner = match &visitor_data {
            Some(vd) => {
                let options = innertube_rs::SessionOptions {
                    visitor_data: Some(vd.clone()),
                    ..Default::default()
                };
                Innertube::with_options(options).await?
            }
            None => Innertube::new().await?,
        };

        // Persist what we actually ended up with, so the *next* launch reuses it
        // even when we generated it this time.
        if let Some(p) = &path {
            if let Some(actual) = &inner.session.context.client.visitor_data {
                if std::fs::write(p, actual.as_bytes()).is_ok() {
                    tracing::debug!("persisted visitorData to {}", p.display());
                }
            }
        }

        tracing::info!(
            visitor_data = ?inner.session.context.client.visitor_data,
            reused = visitor_data.is_some(),
            client = inner.session.context.client.client_name,
            "innertube session ready"
        );
        let http = reqwest::Client::builder()
            .user_agent(concat!("Savvy/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        Ok(Self { inner, http })
    }

    pub fn raw(&self) -> &Innertube {
        &self.inner
    }

    /// Deletes the persisted `visitorData` in `dir`, so the next
    /// [`InnerTube::new_with_dir`] generates a fresh identity.
    ///
    /// Only the deliberate session reset calls this. Normal launches must *keep*
    /// the identity — that is the whole point of persisting it.
    /// Returns whether a file was actually removed.
    pub fn forget_visitor_data(dir: &std::path::Path) -> std::io::Result<bool> {
        let p = dir.join("visitor_data.txt");
        match std::fs::remove_file(&p) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Resolve a `StreamingFormat` into a final playable googlevideo URL by
    /// running it through the embedded QuickJS decipherer (signature + n-token).
    pub fn resolve_format_url(&self, f: &StreamingFormat) -> Result<String, InnertubeError> {
        innertube_rs::endpoints::player::resolve_stream_url(&f, &self.inner.player.decipherer)
    }

    pub async fn stream_url_audio(&self, video_id: &str) -> Result<String, InnertubeError> {
        let filter = FormatFilter {
            format_type: FormatType::AudioOnly,
            quality: QualityPreference::Highest,
            container: None,
        };
        self.inner.get_stream_url(video_id, &filter).await
    }

    pub async fn stream_url_video(&self, video_id: &str) -> Result<String, InnertubeError> {
        let filter = FormatFilter {
            format_type: FormatType::VideoOnly,
            quality: QualityPreference::Highest,
            container: None,
        };
        self.inner.get_stream_url(video_id, &filter).await
    }

    pub async fn stream_url_combined(&self, video_id: &str) -> Result<String, InnertubeError> {
        let filter = FormatFilter {
            format_type: FormatType::AudioVideo,
            quality: QualityPreference::Highest,
            container: None,
        };
        self.inner.get_stream_url(video_id, &filter).await
    }

    // Pass-throughs to the underlying innertube client.

    pub async fn get_video_info(
        &self,
        video_id: &str,
    ) -> Result<innertube_rs::PlayerResponse, InnertubeError> {
        self.inner.get_video_info(video_id).await
    }

    /// Fetch the player response as a *specific* InnerTube client, and hand back
    /// the User-Agent that client must present when the resulting googlevideo URL
    /// is actually read.
    ///
    /// This exists because of a client-side quota, not a per-video one. YouTube
    /// meters how many bytes an anonymous client may pull from an adaptive
    /// rendition, and once that budget is spent it answers `403` to any range
    /// beyond roughly the first 16 MiB — which silently truncated our HD remux
    /// to a fragment that ffmpeg reported as success. The budget differs per
    /// client, so the fix is to ask with a client that is not throttled
    /// (VISIONOS) instead of the default (WEB, which drains quickly).
    ///
    /// Both halves matter. Resolving the URL with one client and then fetching
    /// it with another client's User-Agent is what the earlier attempts did, and
    /// the meter keys on the pair.
    ///
    /// The session context is restored afterwards, so callers that also need the
    /// default client (e.g. the progressive rendition for audio) are unaffected.
    pub async fn get_video_info_as(
        &mut self,
        client: &str,
        video_id: &str,
    ) -> Result<(innertube_rs::PlayerResponse, String), InnertubeError> {
        let original = self.inner.session.context.clone();
        let mut context = original.clone();

        Session::adjust_context(&mut context, client)
            .map_err(|e| InnertubeError::Other(e.to_string()))?;

        // googlevideo compares this against the client that issued the URL.
        let user_agent = context.client.user_agent.clone();

        // `Arc::get_mut` fails if the session is shared. That must be a hard
        // error: silently continuing would fetch the default client and hand
        // back a URL that is already known to be throttled.
        let session = Arc::get_mut(&mut self.inner.session).ok_or_else(|| {
            InnertubeError::Other(format!(
                "cannot switch InnerTube client to {client}: session is shared"
            ))
        })?;
        session.context = context;

        let result = self.inner.get_video_info(video_id).await;

        // Restore, including on failure, so a throttled VISIONOS call cannot
        // leave the session in a state that breaks later default-client calls.
        if let Some(session) = Arc::get_mut(&mut self.inner.session) {
            session.context = original;
        }

        Ok((result?, user_agent))
    }

    /// `/player` for a specific client, returned as the raw JSON body rather
    /// than the typed `PlayerResponse`.
    ///
    /// The typed struct drops the fields the resolve additionally keeps — the
    /// chapter markers (`playerOverlays`) and the description (`microformat`) —
    /// so Savvy asks once more, with the same client-context dance as
    /// `get_video_info_as`, and reads them from the body directly. Nothing in
    /// this response is deciphered or streamed: it exists strictly so those
    /// extras can be had without touching innertube-rs's parse.
    pub async fn get_video_info_as_raw(
        &mut self,
        client: &str,
        video_id: &str,
    ) -> Result<(serde_json::Value, String), InnertubeError> {
        let original = self.inner.session.context.clone();
        let mut context = original.clone();

        Session::adjust_context(&mut context, client)
            .map_err(|e| InnertubeError::Other(e.to_string()))?;

        let user_agent = context.client.user_agent.clone();

        let session = Arc::get_mut(&mut self.inner.session).ok_or_else(|| {
            InnertubeError::Other(format!(
                "cannot switch InnerTube client to {client}: session is shared"
            ))
        })?;
        session.context = context;

        let payload = serde_json::json!({
            "videoId": video_id,
            "contentCheckOk": true,
            "racyCheckOk": true,
            "playbackContext": {
                "contentPlaybackContext": {
                    "vis": 0,
                    "splay": false,
                    "lactMilliseconds": "-1",
                    "html5Preference": "HTML5_PREF_WANTS"
                }
            }
        });
        let result = self.inner.call("/player", payload).await;

        // Restore, including on failure, so a refused call cannot leave the
        // session pointing at a client that later requests must not use.
        if let Some(session) = Arc::get_mut(&mut self.inner.session) {
            session.context = original;
        }

        Ok((result?, user_agent))
    }

    pub async fn search(
        &self,
        query: &str,
        continuation: Option<&str>,
    ) -> Result<SearchResults, InnertubeError> {
        self.inner.search(query, continuation).await
    }

    pub async fn search_with_filters(
        &self,
        query: &str,
        filters: Option<&SearchFilters>,
        continuation: Option<&str>,
    ) -> Result<SearchResults, InnertubeError> {
        self.inner.search_with_filters(query, filters, continuation).await
    }

    pub async fn get_search_suggestions(
        &self,
        query: &str,
        is_music: bool,
    ) -> Result<SearchSuggestionsResult, InnertubeError> {
        self.inner.get_search_suggestions(query, is_music).await
    }

    pub async fn get_playlist(
        &self,
        playlist_id: &str,
    ) -> Result<PlaylistView, InnertubeError> {
        self.inner.get_playlist(playlist_id).await
    }

    pub async fn get_related_videos(
        &self,
        video_id: &str,
    ) -> Result<Vec<innertube_rs::RelatedVideo>, InnertubeError> {
        self.inner.get_related_videos(video_id).await
    }

    pub async fn get_channel(
        &self,
        id: &str,
    ) -> Result<innertube_rs::ChannelArtistView, InnertubeError> {
        self.inner.get_channel(id).await
    }

    /// Channel profile as a Savvy `ChannelDto`.
    ///
    /// Deliberately built on `get_channel_about` rather than `get_channel`:
    /// the latter returns `ChannelArtistView`, innertube-rs's *music artist*
    /// model, which has no title, description or subscriber count.
    pub async fn get_channel_info(&self, channel_id: &str) -> crate::Result<crate::dto::ChannelDto> {
        crate::dto::get_channel(&self.inner, channel_id)
            .await
            .map_err(Into::into)
    }

    /// Channel videos as a Savvy `ChannelVideosDto`.
    pub async fn get_channel_videos(
        &self,
        channel_id: &str,
    ) -> crate::Result<crate::dto::ChannelVideosDto> {
        crate::dto::get_channel_videos_inner(&self.inner, channel_id)
            .await
            .map_err(Into::into)
    }

    /// One page of a channel's Videos tab. `continuation` is the token from the
    /// previous page, used to page deeper into the channel's back catalogue.
    pub async fn get_channel_videos_page(
        &self,
        channel_id: &str,
        continuation: Option<&str>,
    ) -> crate::Result<crate::dto::ChannelVideosDto> {
        crate::dto::get_channel_videos_page(&self.inner, channel_id, continuation)
            .await
            .map_err(Into::into)
    }
}
