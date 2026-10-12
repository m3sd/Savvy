import { invoke } from '@tauri-apps/api/core';

// ─── Canonical DTOs ────────────────────────────────────────────────────────────
// These mirror the Rust types one-for-one and must stay in step with them:
// `db.rs` (HistoryRow, PlaylistRow, PlaylistItemRow, SubscriptionRow,
// CategoryRow), `crates/innertube/src/dto.rs` (the rest) and the command DTOs in
// `commands.rs`. All are camelCase on the wire.

export interface ThumbnailDto {
    url: string;
    width?: number | null;
    height?: number | null;
}

export interface VideoDetailsDto {
    videoId: string;
    title: string;
    lengthSeconds: number;
    description?: string | null;
    viewCount?: string | null;
    channelId: string;
    author: string;
    thumbnails: ThumbnailDto[];
    /** ISO 8601 release date, when the player extras carried one. */
    publishDate?: string | null;
}

export interface AdaptiveFormatDto {
    itag: number;
    mime: string;
    bitrate: number;
    width?: number | null;
    height?: number | null;
    fps?: number | null;
    qualityLabel?: string | null;
    audioQuality?: string | null;
    isAudioOnly: boolean;
    isVideoOnly: boolean;
    isCombined: boolean;
    /** Codec alone, e.g. `avc1.640028`, split out of `mime`. */
    codec?: string | null;
    contentLength?: number | null;
    averageBitrate?: number | null;
    /** Audio only. */
    audioSampleRate?: number | null;
    audioChannels?: number | null;
    audioTrack?: string | null;
    language?: string | null;
}

export interface VideoInfoDto {
    details: VideoDetailsDto;
    adaptiveFormats: AdaptiveFormatDto[];
    hlsManifestUrl?: string | null;
    bestCombinedUrl?: string | null;
    bestVideoUrl?: string | null;
    bestAudioUrl?: string | null;
    /** Caption tracks, empty when the video has none. */
    captions: CaptionTrack[];
}

export interface StreamUrlsDto {
    video?: string | null;
    audio?: string | null;
    combined?: string | null;
    hlsManifest?: string | null;
}

export interface StreamVariantDto {
    itag: number;
    qualityLabel?: string | null;
    height?: number | null;
    fps?: number | null;
    bitrate: number;
    isHdr: boolean;
    mime: string;
    contentLength?: number | null;
    token: string;
}

export interface MuxSourceDto {
    /** `pending` | `ready` | `failed`. */
    state: string;
    url?: string | null;
    bytes?: number | null;
    error?: string | null;
}

export interface RelatedVideoDto {
    videoId: string;
    title: string;
    author: string;
    channelId?: string | null;
    durationSeconds?: number | null;
    viewCount?: string | null;
    thumbnails: ThumbnailDto[];
    isLive: boolean;
}

export interface RelatedVideosDto {
    videos: RelatedVideoDto[];
}

export interface ChannelDto {
    id: string;
    name: string;
    description?: string | null;
    thumbnails: ThumbnailDto[];
    subscriberCount?: string | null;
    videosCount?: string | null;
}

export interface ChannelVideoItem {
    videoId: string;
    title: string;
    thumbnails: ThumbnailDto[];
    durationSeconds?: number | null;
    publishedText?: string | null;
    viewCount?: string | null;
    channelId: string;
    author?: string | null;
}

export interface ChannelVideosDto {
    channelId: string;
    channelName: string;
    videos: ChannelVideoItem[];
    continuation?: string | null;
}

export interface SearchChannelItemDto {
    channelId: string;
    title: string;
    subscriberCount?: string | null;
    videoCount?: string | null;
    thumbnails: ThumbnailDto[];
}

export interface VideoSearchItemDto {
    videoId: string;
    title: string;
    durationSeconds?: number | null;
    viewCount?: string | null;
    channelId: string;
    author: string;
    thumbnails: ThumbnailDto[];
}

/** Tagged union, serialised with `#[serde(tag = "kind")]`. */
export type SearchItemDto =
    | ({ kind: 'video' } & VideoSearchItemDto)
    | ({ kind: 'channel' } & SearchChannelItemDto)
    | {
          kind: 'playlist';
          id: string;
          title: string;
          thumbnail?: string | null;
          author?: string | null;
          videoCount?: string | null;
      };

export interface SearchPlaylistVideoDto {
    videoId: string;
    title: string;
    author: string;
    channelId?: string | null;
    thumbnail?: string | null;
    durationSeconds?: number | null;
    viewCount?: string | null;
    published?: string | null;
    index?: number | null;
}

export interface YoutubePlaylistDto {
    id: string;
    title: string;
    author?: string | null;
    description?: string | null;
    videoCount?: number | null;
    thumbnail?: string | null;
    videos: SearchPlaylistVideoDto[];
}

export interface SearchResultsDto {
    query: string;
    items: SearchItemDto[];
    continuation?: string | null;
}

export interface HistoryRow {
    videoId: string;
    title: string;
    author: string;
    thumbnail: string;
    durationSeconds: number;
    channelId: string;
    watchedAt: number;
    progressSeconds: number;
}

export interface SavedVideoRow {
    videoId: string;
    title: string;
    author: string;
    thumbnail: string;
    durationSeconds: number;
    channelId: string;
    savedAt: number;
}

export interface PlaylistRow {
    id: string;
    title: string;
    createdAt: number;
}

export interface PlaylistItemRow {
    playlistId: string;
    videoId: string;
    title: string;
    author: string;
    thumbnail: string;
    durationSeconds: number;
    channelId: string;
    position: number;
}

export interface SubscriptionRow {
    channelId: string;
    title: string;
    thumbnail: string;
    subscriberCount?: number | null;
    addedAt: number;
    /** Many-to-many: a subscription can be in several categories at once. */
    categoryIds: string[];
}

export interface CategoryRow {
    id: string;
    name: string;
    position: number;
}

export interface FeedItemDto {
    videoId: string;
    title: string;
    channelId: string;
    channelName: string;
    channelThumbnail: string;
    thumbnails: ThumbnailDto[];
    durationSeconds?: number | null;
    viewCount?: string | null;
    publishedText?: string | null;
    publishedAt?: number | null;
}

export interface FeedDto {
    /** The next slice, in chronological order across all subscribed channels. */
    items: FeedItemDto[];
    /** Opaque token to pass back for the following slice; null when exhausted. */
    nextCursor: string | null;
    /** Whether asking for more would yield anything. */
    hasMore: boolean;
    /** The server lost the session and started over; replace the list. */
    restarted: boolean;
}

export interface ImportResult {
    added: number;
    existing: number;
    skipped: number;
    categoriesCreated: number;
    warnings: string[];
}

export interface SponsorSegmentDto {
    category: string;
    startSeconds: number;
    endSeconds: number;
    UUID: string;
}

export interface SponsorSegmentsDto {
    segments: SponsorSegmentDto[];
}

export interface ChapterDto {
    title: string;
    startMs: number;
    thumbUrl: string | null;
}

/** One caption track the resolved player offered. No URL — the webview only
 *  ever loads the proxied VTT the backend mints for it. */
export interface CaptionTrack {
    /** BCP-47-ish code from `languageCode`, e.g. `en`, `pt-BR`. */
    lang: string;
    /** Display name, e.g. `English (auto-generated)`. */
    label: string;
    /** True for ASR (auto-generated) tracks. */
    isAsr: boolean;
}

// ─── Commands ──────────────────────────────────────────────────────────────────

export const api = {
    ping: () => invoke<string>('ping'),
    bootstrap: () =>
        invoke<{ version: string; innertubeReady: boolean; proxyAddr: string | null }>('bootstrap'),

    search: (query: string) => invoke<SearchResultsDto>('search', { query }),
    // Videos *and* channels in one round-trip. YouTube only returns a channel
    // shelf from an unfiltered search when the query matches a channel closely,
    // so `all` mode uses this rather than `search` — otherwise a partial channel
    // name finds nothing.
    searchAll: (query: string) => invoke<SearchResultsDto>('search_all', { query }),
    searchChannels: (query: string) => invoke<SearchChannelItemDto[]>('search_channels', { query }),
    suggestions: (query: string) => invoke<string[]>('search_suggestions', { query }),
    youtubePlaylist: (playlistId: string) => invoke<YoutubePlaylistDto>('youtube_playlist', { playlistId }),

    channelGetInfo: (channelId: string) => invoke<ChannelDto>('channel_get_info', { channelId }),
    channelGetVideos: (channelId: string) => invoke<ChannelVideosDto>('channel_get_videos', { channelId }),
    relatedVideos: (videoId: string) => invoke<RelatedVideosDto>('related_videos', { videoId }),
    sponsorSegments: (videoId: string, categories: string[]) =>
        invoke<SponsorSegmentsDto>('sponsor_segments', { videoId, categories }),
    videoChapters: (videoId: string) => invoke<ChapterDto[]>('video_chapters', { videoId }),
    /** Tracks the resolved player response already carried (no extra request). */
    captionsList: (videoId: string) => invoke<CaptionTrack[]>('captions_list', { videoId }),
    /** Warm one track into the backend VTT store; the returned `url` is the
     *  proxied loopback `.vtt` the native `<track>` element loads. Subsequent
     *  calls for the same language are served from the warm cache. */
    captionsFetch: (videoId: string, lang: string) =>
        invoke<{ url: string; lang: string; label: string }>('captions_fetch', { videoId, lang }),

    videoInfo: (videoId: string) => invoke<VideoInfoDto>('video_info', { videoId }),
    videoResolveStreams: (videoId: string) =>
        invoke<{
            info: VideoInfoDto;
            streams: StreamUrlsDto;
            variants: StreamVariantDto[];
            videoVariants: StreamVariantDto[];
            audioVariant?: StreamVariantDto | null;
        }>('video_resolve_streams', { videoId }),
    audioSource: (videoId: string) => invoke<{ url?: string | null; mime?: string | null }>('audio_source', { videoId }),
    videoMuxSource: (videoId: string, itag?: number | null) =>
        invoke<MuxSourceDto>('video_mux_source', { videoId, itag: itag ?? null }),

    historyList: (limit = 200) => invoke<HistoryRow[]>('history_list', { limit }),
    historyClear: () => invoke<void>('history_clear'),
    /** Drop one row — the context menu's "remove from history". */
    historyRemove: (videoId: string) => invoke<void>('history_remove', { videoId }),
    historyAdd: (args: {
        videoId: string;
        title: string;
        author: string;
        thumbnail: string;
        durationSeconds: number;
        channelId: string;
        progressSeconds: number;
    }) => invoke<void>('history_add', { args }),
    historyGet: (videoId: string) => invoke<HistoryRow | null>('history_get', { videoId }),
    historyProgress: (args: {
        videoId: string;
        positionSeconds: number;
        durationSeconds: number;
    }) => invoke<void>('history_progress', { args }),

    playlistsList: () => invoke<PlaylistRow[]>('playlists_list'),
    playlistCreate: (id: string, title: string) => invoke<PlaylistRow>('playlist_create', { args: { id, title } }),
    playlistDelete: (id: string) => invoke<void>('playlist_delete', { id }),
    playlistAddItem: (args: {
        playlistId: string;
        videoId: string;
        title: string;
        author: string;
        thumbnail: string;
        durationSeconds: number;
        channelId: string;
    }) => invoke<void>('playlist_add_item', { args }),
    playlistRemoveItem: (playlistId: string, videoId: string) =>
        invoke<void>('playlist_remove_item', { playlistId, videoId }),
    playlistItems: (playlistId: string) => invoke<PlaylistItemRow[]>('playlist_items', { playlistId }),

    savedAdd: (args: {
        videoId: string;
        title: string;
        author: string;
        thumbnail: string;
        durationSeconds: number;
        channelId: string;
    }) => invoke<void>('saved_add', { args }),
    savedList: (limit = 500) => invoke<SavedVideoRow[]>('saved_list', { limit }),
    /** Ids only — the cheap "already saved?" check for the context menu. */
    savedIds: () => invoke<string[]>('saved_ids'),
    savedRemove: (videoId: string) => invoke<void>('saved_remove', { videoId }),

    settingSet: (key: string, value: string) => invoke<void>('setting_set', { key, value }),
    settingGet: (key: string) => invoke<string | null>('setting_get', { key }),
    /** Every setting at once — stored overrides merged over the Rust defaults. */
    settingsAll: () => invoke<Record<string, string>>('settings_all'),
    /** Drop an override so a key returns to its default. */
    settingReset: (key: string) => invoke<void>('setting_reset', { key }),
    /** Discard the persisted InnerTube visitor data (item 15). */
    innertubeReset: () => invoke<{ version: string; innertubeReady: boolean; proxyAddr: string | null }>(
        'innertube_reset'
    ),
    /** Hand a YouTube URL to the system browser (context menu). */
    openExternal: (url: string) => invoke<void>('open_external', { url }),

    subscriptionList: (categoryId?: string) =>
        invoke<SubscriptionRow[]>('subscription_list', { categoryId: categoryId ?? null }),
    subscriptionFeed: (cursor?: string | null, limit?: number) =>
        invoke<FeedDto>('subscription_feed', { cursor: cursor ?? null, limit: limit ?? null }),
    subscriptionAdd: (input: {
        channelId: string;
        title: string;
        thumbnail: string;
        subscriberCount?: number | null;
    }) => invoke<void>('subscription_add', { input }),
    subscriptionRemove: (channelId: string) => invoke<void>('subscription_remove', { channelId }),
    subscriptionExists: (channelId: string) => invoke<boolean>('subscription_exists', { channelId }),
    /** Backfill missing avatars; returns the refreshed list (spec item 16). */
    subscriptionRefreshAvatars: () =>
        invoke<SubscriptionRow[]>('subscription_refresh_avatars'),
    subscriptionSetCategories: (channelId: string, categoryIds: string[]) =>
        invoke<void>('subscription_set_categories', {
            input: { channelId, categoryIds }
        }),

    categoryList: () => invoke<CategoryRow[]>('category_list'),
    categoryAdd: (name: string) => invoke<CategoryRow>('category_add', { input: { name } }),
    categoryRename: (id: string, name: string) =>
        invoke<CategoryRow>('category_rename', { id, input: { name } }),
    categoryRemove: (id: string) => invoke<void>('category_remove', { id }),

    subscriptionImport: (subscriptionsJson: string) =>
        invoke<ImportResult>('subscription_import', { input: { subscriptionsJson } }),
    subscriptionImportFile: (path: string) => invoke<ImportResult>('subscription_import_file', { path })
};

// ─── Helpers ───────────────────────────────────────────────────────────────────

export function isTauri(): boolean {
    return '__TAURI_INTERNALS__' in window;
}

/** Ids whose resolve is already in flight, so a hover never stacks requests. */
const preResolving = new Set<string>();
/** Coalescing timers, so scrubbing a list does not resolve every card. */
const preResolveTimers = new Map<string, ReturnType<typeof setTimeout>>();
const PRE_RESOLVE_DELAY_MS = 200;

/**
 * Warm a video's resolve cache from a card hover/focus.
 *
 * Debounced and deduplicated: the backend resolve is cached and single-flight,
 * but there is no point paying for a card the user only grazes.
 */
export function preResolve(videoId: string): void {
    if (!videoId || preResolving.has(videoId)) return;
    const existing = preResolveTimers.get(videoId);
    if (existing) clearTimeout(existing);
    preResolveTimers.set(
        videoId,
        setTimeout(() => {
            preResolveTimers.delete(videoId);
            preResolving.add(videoId);
            api.videoResolveStreams(videoId).catch(() => {
                preResolving.delete(videoId);
            });
        }, PRE_RESOLVE_DELAY_MS)
    );
}
