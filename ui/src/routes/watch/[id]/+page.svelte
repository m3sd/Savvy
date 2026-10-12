<script lang="ts">
    import { page } from '$app/state';
    import { HugeiconsIcon } from '@hugeicons/svelte';
    import { ArrowLeft01Icon } from '@hugeicons/core-free-icons';
    import { goto, beforeNavigate } from '$app/navigation';
    import { onMount, onDestroy } from 'svelte';
    import { api, preResolve, type VideoInfoDto, type StreamUrlsDto, type StreamVariantDto, type RelatedVideoDto, type SponsorSegmentDto, type ChapterDto, type AdaptiveFormatDto, type CaptionTrack } from '$lib/api';
    import { loadSettings, settingNum, settingStr, settingBool, setSetting } from '$lib/settings.svelte';
    import { DEFAULT_SHORTCUTS, comboOf } from '$lib/shortcuts';

    const videoId = $derived(page.params.id);

    let info = $state<VideoInfoDto | null>(null);
    let streams = $state<StreamUrlsDto | null>(null);
    let variants = $state<StreamVariantDto[]>([]);
    let videoVariants = $state<StreamVariantDto[]>([]);
    let audioVariant = $state<StreamVariantDto | null>(null);
    /** URL of the server-remuxed HD file, available as soon as ffmpeg writes. */
    let muxUrl = $state<string | null>(null);
    let muxState = $state<'idle' | 'pending' | 'ready' | 'failed'>('idle');
    // The first `play()` attempt happens before media data exists, so it always
    // rejects; this tracks that we have already made a real attempt.
    let autoplayDone = false;
    /**
     * Whether the `dash` audio element has produced sound.
     *
     * The failure this guards against is the quiet one: `dash` cannot fall back on
     * its own, because if the audio track never starts the picture plays on in
     * silence and looks like it is working. Watching for a real `playing` event on
     * the audio element is the only way to tell silence apart from a slow start,
     * so a `dash` that cannot get audio within a few seconds drops to `progressive`.
     */
    let dashAudioStarted = false;
    let muxError = $state<string | null>(null);
    /** Bytes of the remux written so far, for the progress line. */
    let selectedItag = $state<number | null>(null);
    /** `muxed` plays the server-remuxed HD file; `progressive` is the 360p fallback. */
    let mode = $state<'muxed' | 'progressive' | 'dash'>('progressive');
    let selectedVideoItag = $state<number | null>(null);
    let playerError = $state<string | null>(null);
    let proxyAddr = $state<string | null>(null);
    let loading = $state(true);
    let error = $state<string | null>(null);
    let videoEl = $state<HTMLVideoElement | null>(null);
    /**
     * The audio track for `dash` playback, playing nothing you can see.
     *
     * `dash` is how playback works without a progressive rendition: the video-only
     * and audio-only adaptive streams go to two elements and are kept in step. That
     * is the shape LibreTube plays in, and it needs no server-side muxing, so
     * playback starts as soon as the two streams have buffered instead of waiting
     * for ffmpeg to produce a file.
     */
    let audioEl = $state<HTMLAudioElement | null>(null);
    /** Video id the element's current `src` belongs to, so a same-video source
     *  swap (remux completing, quality change) is told apart from navigation to
     *  a new video. */
    let srcVideoId: string | null = null;
    /** Watch-next list for the current video; fetched behind the resolve. */
    let related = $state<RelatedVideoDto[]>([]);
    /** Bumped per video so a stale poll can't clobber the next one. */
    let audioGen = 0;
    /** Where to resume this video, from its history row; null = start over. */
    let pendingResumeAt = $state<number | null>(null);
    /** One resume-seek per load, straddling the pending→ready URL swap. */
    let resumeApplied = false;
    /**
     * The resume seek is only safe once playback has actually started. Setting
     * `currentTime` before/while `play()` is in flight aborts it with an
     * `AbortError`, which is what produced "Press play to start" on a resumed video.
     */
    let resumeArmed = false;
    /**
     * After a resume seek the audio track has to fetch a brand-new range at a
     * position it has never buffered, so it legitimately has no current data for a
     * second or two. The 360p watchdog must not read that as "audio is broken".
     */
    let audioGraceUntil = 0;
    /** True once `settingsAll` has landed, so defaults can be applied. */
    let settingsReady = $state(false);
    /** Player enlarged to the page width instead of the two-column layout. */
    let theater = $state(false);
    /** The "Stream details" popup (format table + diagnostics) below the player. */
    let streamDetailsOpen = $state(false);
    /**
     * Caption tracks from the resolved player response (empty when the video
     * has none). WebView2's native `<track>` pipeline refuses cross-origin VTT
     * (`error` fires on the element even though the same URL fetches cleanly),
     * so captions are rendered here instead: the backend VTT is fetched as
     * text and timed cues are drawn over the player by `updateActiveCaption`.
     */
    let captionTracks = $state<CaptionTrack[]>([]);
    let activeCaptionLang = $state<string | null>(null);
    /** Proxied loopback `.vtt` returned by `captions_fetch`, fetched as text. */
    let captionVttUrl = $state<string | null>(null);
    let captionError = $state<string | null>(null);
    /** Parsed cues for the selected language, `{ start, end, text }`. */
    let captionCues = $state<{ start: number; end: number; text: string }[]>([]);
    /** The cue currently in `captionCues` covering `videoEl.currentTime`. */
    let activeCaptionText = $state<string | null>(null);
    /** User-chosen caption `font-family`; '' keeps the app default stack. */
    let captionFont = $derived(settingStr('captions.font').trim());
    /**
     * Skippable SponsorBlock segments for this video, oldest-start first. Only
     * present once the (anonymous, cached) fetch has landed.
     */
    let sponsorSegments = $state<SponsorSegmentDto[]>([]);
    /** Index into `sponsorSegments` the playhead is inside; -1 outside all. */
    let inSponsorSegment = $state(-1);
    /**
     * Chapter markers for this video, sorted by `startMs`. Empty for videos
     * with no chapters (or none the backend could read).
     */
    let chapters = $state<ChapterDto[]>([]);
    /** Index into `chapters` the playhead is inside; -1 before the first. */
    let activeChapter = $state(-1);
    /** The horizontal chapters strip, scrolled so the active chapter stays visible. */
    let chapterRow = $state<HTMLElement | null>(null);
    /** The configured volume is applied once, never mid-playback. */
    let volumeApplied = false;
    /** Last progress heartbeat sent, for the 5s/2s throttle. */
    let lastProgressReport = { at: 0, pos: 0 };
    let title = $derived(info?.details.title ?? '');
    let author = $derived(info?.details.author ?? '');
    let channelId = $derived(info?.details.channelId ?? '');
    let length = $derived(info?.details.lengthSeconds ?? 0);
    let description = $derived(info?.details.description ?? '');
    /** Release date, rendered as `Jan 5, 2024` and shown beside the runtime. */
    let releaseDate = $derived(formatReleaseDate(info?.details.publishDate ?? ''));
    let thumbUrl = $derived(() => {
        const t = info?.details.thumbnails;
        return t && t.length > 0 ? t[t.length - 1].url : '';
    });

    /**
     * Fetch the Watch Next list once per video, off the playback critical path.
     * The backend caches it for an hour, so repeat visits are near-instant and
     * never cost another InnerTube request.
     */
    $effect(() => {
        const id = videoId;
        related = [];
        if (!id) return;
        let cancelled = false;
        // Let the resolve/remux get ahead before spending a `/next` request.
        const t = setTimeout(() => {
            void api
                .relatedVideos(id)
                .then((res) => {
                    if (!cancelled) related = res.videos;
                })
                .catch(() => {
                    // Quiet: the panel is optional, playback is not.
                });
        }, 300);
        return () => {
            cancelled = true;
            clearTimeout(t);
        };
    });

    function formatDuration(s: number): string {
        if (!Number.isFinite(s) || s < 0) return '0:00';
        const total = Math.floor(s);
        const h = Math.floor(total / 3600);
        const m = Math.floor((total % 3600) / 60);
        const sec = total % 60;
        return h > 0
            ? `${h}:${String(m).padStart(2, '0')}:${String(sec).padStart(2, '0')}`
            : `${m}:${String(sec).padStart(2, '0')}`;
    }

    /**
     * The ISO 8601 release date from the microformat, short and without the
     * time: `2024-01-05T00:00:00+00:00` becomes `Jan 5, 2024`.
     *
     * The calendar day is taken straight from the string rather than via
     * `Date` alone — a channel-local midnight would otherwise slide a day
     * either way for anyone in another time zone.
     */
    function formatReleaseDate(iso: string): string {
        const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(iso);
        if (!m) return '';
        const d = new Date(Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3])));
        if (Number.isNaN(d.getTime())) return '';
        return d.toLocaleDateString(undefined, {
            year: 'numeric',
            month: 'short',
            day: 'numeric',
            timeZone: 'UTC',
        });
    }

    /** `12.4 MB` for the size column of the stream-details table. */
    function formatBytes(n: number | null | undefined): string {
        if (!n || !Number.isFinite(n) || n <= 0) return '—';
        const units = ['B', 'KB', 'MB', 'GB'];
        let value = n;
        let unit = 0;
        while (value >= 1024 && unit < units.length - 1) {
            value /= 1024;
            unit += 1;
        }
        const shown = unit === 0 ? value.toFixed(0) : value.toFixed(value >= 100 ? 0 : 1);
        return `${shown} ${units[unit]}`;
    }

    /** The player hands back a bare count; group it unless it already reads well. */
    function formatViews(raw: string): string {
        const n = Number(raw);
        return Number.isFinite(n) ? n.toLocaleString() : raw;
    }

    /** Audio-only rows: `48 kHz · 2 ch · English original`. */
    function audioSpecs(f: AdaptiveFormatDto): string {
        const parts: string[] = [];
        if (f.audioSampleRate) parts.push(`${f.audioSampleRate / 1000} kHz`);
        if (f.audioChannels) parts.push(`${f.audioChannels} ch`);
        if (f.audioTrack) parts.push(f.audioTrack);
        else if (f.language) parts.push(f.language);
        return parts.length > 0 ? parts.join(' · ') : '—';
    }

    /** The remux streams straight from ffmpeg, so a URL is available almost
     * immediately; the window only has to cover a slow CDN and a long file. */
    const MUX_TIMEOUT_MS = 300_000;
    const MUX_POLL_MS = 500;

    /**
     * Ask the backend for the HD file and poll until it has one.
     *
     * `pending` already carries a playable URL — the output is a fragmented MP4
     * and the proxy streams it while ffmpeg is still writing — so this returns as
     * soon as the first fragment exists and keeps polling only to report
     * progress. Resolves on failure too; the caller falls back to 360p.
     */
    async function prepareHd(id: string, gen: number): Promise<void> {
        muxState = 'pending';
        muxUrl = null;
        muxError = null;
        autoplayDone = false;
        const deadline = Date.now() + MUX_TIMEOUT_MS;
        while (Date.now() < deadline) {
            if (gen !== audioGen) return;
            try {
                const s = await api.videoMuxSource(id, selectedVideoItag);
                if (gen !== audioGen) return;
                // `pending` carries a fresh token for the same `.part` on every
                // poll, so the URL is *not* stable while ffmpeg works. Adopting
                // each one reassigns `src` and calls `load()`, which discards
                // whatever the element had buffered — so it would reload every
                // 500ms and never reach `canplay`. Take the first one and hold
                // it: the same growing stream keeps producing bytes.
                //
                // `ready` is the one case that must swap, because ffmpeg has
                // renamed the file and the growing URL is now dead.
                if (s.state === 'ready') {
                    if (s.url) muxUrl = s.url;
                    muxState = 'ready';
                    return;
                }
                if (s.state === 'failed') {
                    muxState = 'failed';
                    muxError = s.error ?? 'HD remux failed';
                    return;
                }
                if (s.url && !muxUrl) {
                    muxUrl = s.url;
                }
            } catch (e) {
                if (gen !== audioGen) return;
                muxState = 'failed';
                muxError = String(e);
                return;
            }
            await new Promise((r) => setTimeout(r, MUX_POLL_MS));
        }
        if (gen !== audioGen) return;
        muxState = 'failed';
        muxError = 'timed out preparing the HD stream';
    }

    let lastLoaded: string | null = null;

    async function load() {
        // SvelteKit sometimes re-runs this on initial mount with `page.params.id`
        // still empty during the route transition — guard against that.
        const id = videoId;
        if (!id) {
            loading = false;
            error = 'No video id in URL';
            return;
        }
        if (id === lastLoaded) return;
        lastLoaded = id;
        // Invalidate any remux poll still running for the previous video.
        audioGen++;
        const gen = audioGen;
        loading = true;
        error = null;
        playerError = null;
        info = null;
        captionTracks = [];
        activeCaptionLang = null;
        captionVttUrl = null;
        captionCues = [];
        activeCaptionText = null;
        captionError = null;
        settingsOpen = false;
        captionsOpen = false;
        streams = null;
        variants = [];
        videoVariants = [];
        audioVariant = null;
        muxUrl = null;
        muxState = 'idle';
        muxError = null;
        selectedItag = null;
        selectedVideoItag = null;
        pendingResumeAt = null;
        resumeApplied = false;
        resumeArmed = false;
        audioGraceUntil = 0;
        lastProgressReport = { at: 0, pos: 0 };
        sponsorSegments = [];
        inSponsorSegment = -1;
        chapters = [];
        activeChapter = -1;
        try {
            // Settings decide the default rendition and volume below, so they have to
            // be in place before the resolve lands. `loadSettings` shares one in-flight
            // request with the layout, so this does not cost a second round trip.
            await loadSettings();
            settingsReady = true;
            theater = settingBool('player.theater');
            applyDefaultVolume();

            // SponsorBlock segments, fetched off the playback critical path. The
            // request is anonymous, cached server-side for a day, and never
            // surfaced as an error: a failure simply leaves the list empty.
            const sponsorCats = settingStr('sponsorblock.categories')
                .split(',')
                .filter(Boolean);
            if (settingBool('sponsorblock.enabled') && sponsorCats.length > 0) {
                void api
                    .sponsorSegments(id, sponsorCats)
                    .then((res) => {
                        if (gen !== audioGen || String(id) !== String(videoId)) return;
                        sponsorSegments = res.segments
                            .slice()
                            .sort((a, b) => a.startSeconds - b.startSeconds);
                    })
                    .catch(() => {
                        sponsorSegments = [];
                    });
            }

            // Chapters, fetched off the playback critical path. The markers ride
            // inside the cached resolve (`video_chapters` reads the same cache
            // `videoResolveStreams` just populated), so a re-visit costs nothing;
            // a failure simply leaves the row hidden.
            void api
                .videoChapters(id)
                .then((list) => {
                    if (gen !== audioGen || String(id) !== String(videoId)) return;
                    chapters = list ?? [];
                })
                .catch(() => {
                    chapters = [];
                });

            const boot = await api.bootstrap();
            proxyAddr = boot.proxyAddr;
            const resp = await api.videoResolveStreams(id);
            info = resp.info;
            captionTracks = resp.info.captions ?? [];
            streams = resp.streams;
            variants = resp.variants ?? [];
            videoVariants = resp.videoVariants ?? [];
            audioVariant = resp.audioVariant ?? null;
            const mp4s = (resp.videoVariants ?? []).filter((v) =>
                v.mime.startsWith('video/mp4')
            );
            // Default to the best progressive rendition the backend ranked first.
            selectedItag = variants.length > 0 ? variants[0].itag : null;
            // Default to 1080p rather than the top rendition. The remux cannot
            // begin until ffmpeg has read the source's index, so start time
            // scales with the file we ask for; a 4K source is ~4x the bytes of
            // 1080p for no benefit on a typical window. The picker still offers
            // everything up to the top.
            selectedVideoItag = pickDefaultHd(mp4s);
            // Prefer `dash`: it plays the two adaptive tracks directly, so it starts
            // as soon as they buffer and switching quality is just re-pointing the
            // video element - no ffmpeg, no wait. It needs an MP4/H.264 video track
            // (the only video codec WebView2 reliably decodes) and an audio track.
            // `progressive` remains the fallback, and is the *only* option when the
            // winning client had no progressive rendition but did hand us both
            // adaptive tracks - which is the case that keeps audio alive when every
            // progressive-capable client is refused.
            // `playback.hd_mode: always_360p` opts out of DASH entirely and keeps the
            // single-file progressive rendition that carries an audio track baked in —
            // the path that survives even when both adaptive tracks go silent.
            mode =
                settingStr('playback.hd_mode') === 'always_360p'
                    ? 'progressive'
                    : mp4s.length > 0 && resp.audioVariant
                      ? 'dash'
                      : 'progressive';

            // No remux is started here. `muxed` is only ever reached by an explicit HD
            // request on a video with no audio track to pair, and that path starts
            // its own ffmpeg pass in `toggleHd`.

            // Add to history (best-effort).
            try {
                await api.historyAdd({
                    videoId: resp.info.details.videoId,
                    title: resp.info.details.title,
                    author: resp.info.details.author,
                    thumbnail:
                        resp.info.details.thumbnails[resp.info.details.thumbnails.length - 1]
                            ?.url ?? '',
                    durationSeconds: resp.info.details.lengthSeconds,
                    channelId: resp.info.details.channelId,
                    progressSeconds: 0
                });
            } catch (e) {
                console.warn('history_add failed', e);
            }

            // Continue-watching: restore where we left off. Rows whose playhead
            // sits in the first/last 5s are treated by the backend as "not
            // started" / "finished", so mirror that here instead of seeking into
            // a video that should play from the top.
            try {
                const h = await api.historyGet(id);
                if (
                    h &&
                    h.progressSeconds > 5 &&
                    (h.durationSeconds <= 0 || h.progressSeconds < h.durationSeconds - 5)
                ) {
                    pendingResumeAt = h.progressSeconds;
                }
            } catch (e) {
                console.warn('history_get failed', e);
            }
        } catch (e) {
            error = String(e);
        } finally {
            loading = false;
        }
    }

    onMount(load);
    onMount(() => {
        window.addEventListener('pagehide', flushOnPageHide);
        window.addEventListener('keydown', onKeydown);
        document.addEventListener('fullscreenchange', onFullscreenChange);
    });

    // Re-load when the URL changes between two videos (SvelteKit reuses this
    // component instance when only the param changes — `onMount` would not re-fire).
    $effect(() => {
        videoId; // depend on it
        load();
    });

    // The playhead is only known after `historyGet` resolves, which can land
    // *after* the element already reached `canplay`. Try again as soon as the
    // value shows up (idempotent: `resumeApplied` gates it).
    $effect(() => {
        if (pendingResumeAt && videoEl) tryResumeSeek();
    });

    // Prefer the explicitly chosen rendition; fall back to whatever the
    // backend picked, then to adaptive-only sources.
    let selectedVariant = $derived(variants.find((v) => v.itag === selectedItag) ?? null);

    /**
     * Only the MP4/H.264 renditions are offered.
     *
     * WebView2 has no dependable decoder for VP9/AV1 in the formats YouTube serves,
     * so those would be handed to the element and fail with no codec error to act
     * on. Filtering them here means `dash` and the remux draw from the same set.
     */
    let mpegVariants = $derived(
        videoVariants.filter((v) => v.mime.startsWith('video/mp4'))
    );

    let activeVideoVariant = $derived(
        mpegVariants.find((v) => v.itag === selectedVideoItag) ?? null
    );

    /** The two URLs `dash` plays: a video-only track and a standalone audio track. */
    let dashUrls = $derived.by(() => {
        if (!proxyAddr || !activeVideoVariant || !audioVariant) return null;
        return {
            video: `${proxyAddr}/stream/${activeVideoVariant.token}`,
            audio: `${proxyAddr}/stream/${audioVariant.token}`,
        };
    });

    /**
     * Drive the audio element from the video element.
     *
     * The user only ever touches the `<video>` controls, so play, pause, seek and
     * volume all have to be mirrored onto the audio element - otherwise the track
     * either stays silent, or keeps playing after the picture stops. Drift between
     * two independent decoders is corrected with a nudge rather than a hard seek,
     * which would be audible.
     */
    $effect(() => {
        const video = videoEl;
        const audio = audioEl;
        const urls = dashUrls;
        if (mode !== 'dash' || !video || !audio || !urls) return;

        if (audio.getAttribute('src') !== urls.audio) {
            audio.src = urls.audio;
            audio.load();
            // The `loadedmetadata` event proves the container parsed, but only a
            // real `playing` proves audio is actually coming out.
            dashAudioStarted = false;
        }
        audio.volume = video.volume;
        audio.muted = video.muted;
        audio.playbackRate = video.playbackRate;

        const onAudioPlaying = () => {
            dashAudioStarted = true;
            playerError = null;
            audioStuckSince = 0;
            lastAudioPos = audio.currentTime;
            // The track is running again, so there is nothing left to wait for.
            release();
        };
        const onAudioError = () => {
            if (mode !== 'dash') return;
            playerError = 'HD audio track would not play; switching to 360p.';
            mode = 'progressive';
        };

        // Bring the audio element in line with the video element.
        //
        // `play()` rejects whenever the element has no data, which is the normal
        // case here, so this is safe to call repeatedly and is the single thing that
        // revives a track that stopped. Seeking is the interesting case: jumping
        // forward leaves the audio element with no buffered data at the new
        // position, the browser suspends it, and without a supervisor it never
        // restarts - which is how the audio cut out and stayed out.
        const syncAudio = () => {
            if (video.paused) return;
            // Mid-seek both elements are between states; re-issuing play() here just
            // races the seek that is already in flight.
            if (video.seeking) return;
            if (audio.paused || audio.readyState < HTMLMediaElement.HAVE_CURRENT_DATA) {
                void audio.play().catch(() => {});
            }
        };

        /**
         * Force a real pause/play transition on the audio element.
         *
         * A seek that lands on a track which is already playing aborts the element's
         * in-flight media operation. Chromium then leaves it reporting `paused ===
         * false` with a healthy `readyState` — the whole file can be buffered — while
         * the playhead never moves again. `play()` is a no-op in that state, so a
         * pause/play pair is the only thing that revives it, and it is also what makes
         * the element re-fire `playing`.
         */
        const kickAudio = () => {
            audioStuckSince = 0;
            audioSeekingSince = 0;
            lastAudioPos = -1;
            // A seek that never settled is the usual reason the playhead is frozen,
            // and while `audio.seeking` is true the stuck test below never runs, so
            // this is the only thing that will ever touch it. Writing a fresh
            // position abandons the fetch in flight.
            if (audio.seeking) audio.currentTime = video.currentTime;
            audio.pause();
            void audio.play().catch(() => {});
        };

        /** Last observed audio playhead, and when it was last seen to move. */
        let lastAudioPos = -1;
        let audioStuckSince = 0;
        /** When the current unbroken `audio.seeking` stretch began, if any. */
        let audioSeekingSince = 0;

        const alignAudio = () => {
            if (Number.isNaN(video.currentTime)) return;
            // Pushing a position onto an element that has not parsed yet can leave it
            // permanently un-seekable, and that is exactly the state the track is in
            // when a resume seek fires at `loadedmetadata`. `onAudioReady` realigns as
            // soon as it does have metadata.
            if (audio.readyState < HTMLMediaElement.HAVE_METADATA) return;
            // Already aiming at (or sitting on) the right position. Rewriting it
            // aborts the seek that is in flight and restarts its range request from
            // scratch - every redundant align was costing the fetch it interrupted,
            // which is how a single skip turned into two cold fetches. A target that
            // is right but never actually fetched is the stuck test's problem, not
            // this one's.
            if (Math.abs(audio.currentTime - video.currentTime) < 0.1) return;
            audio.currentTime = video.currentTime;
        };

        /**
         * Park the picture until the audio track has landed on the same position.
         *
         * This exists because re-seeking the audio can never converge on its own.
         * After a scrub the track is aimed at where the video *was*; its cold range
         * takes `D` seconds to arrive, and the video plays on for those `D` seconds,
         * so it lands exactly `D` behind. Correcting that costs another `D`, which
         * manufactures the same gap again — the drift test would then fire every
         * second or two and the result is audio that keeps jumping as it chases a
         * picture it can never catch.
         *
         * Stopping the video removes the term that makes it diverge: the position it
         * is aligned to stops moving, the track lands on it, and both resume together
         * in sync. The deadline is the escape hatch — a track that never settles must
         * not leave the picture frozen, so playback is released anyway.
         *
         * Returns whether the picture was actually parked. A track that keeps
         * failing to land must not turn this into a freeze loop, so a park shortly
         * after the previous one is refused and the caller falls back to a plain
         * align — a frozen picture is a worse failure than a gap.
         */
        let catchingUp = false;
        let parkResume = false;
        let parkDeadline = 0;
        let lastParkAt = 0;

        const park = (): boolean => {
            if (catchingUp) {
                // Already stopped for the track: a fresh target only has to move the
                // aim and restart the clock it is being given to land on.
                parkDeadline = performance.now() + 8000;
                alignAudio();
                return true;
            }
            if (audio.readyState < HTMLMediaElement.HAVE_METADATA) return false;
            if (performance.now() - lastParkAt < 3000) return false;
            lastParkAt = performance.now();
            catchingUp = true;
            parkResume = !video.paused;
            parkDeadline = performance.now() + 8000;
            if (!video.paused) video.pause();
            alignAudio();
            return true;
        };

        const release = () => {
            if (!catchingUp) return;
            catchingUp = false;
            parkDeadline = 0;
            if (parkResume) void video.play().catch(() => {});
        };

        /** The track just became seekable: catch it up and start it. */
        const onAudioReady = () => {
            alignAudio();
            syncAudio();
        };

        const onPlay = () => {
            if (audio.getAttribute('src') !== urls.audio) return;
            syncAudio();
        };
        const onPause = () => {
            // A seek can surface as a pause on the video element. Pausing the audio
            // track there would strand it - the video resumes on its own, the audio
            // never does - so only a genuine pause is mirrored.
            if (video.seeking) return;
            // Likewise the park: the picture is stopped on purpose while the track
            // fetches, and stopping the track too would leave it mid-seek with
            // nobody left to start it.
            if (catchingUp) return;
            audio.pause();
        };
        const onAudioSeeked = () => {
            release();
            syncAudio();
        };
        const onVolume = () => {
            audio.volume = video.volume;
            audio.muted = video.muted;
        };
        const onRate = () => {
            // The two halves must run at the same rate: a mismatch reads as
            // drift to the supervisor and it would seek to correct a gap that
            // is really just one element playing faster than the other.
            if (audio.playbackRate !== video.playbackRate) audio.playbackRate = video.playbackRate;
        };

        audio.addEventListener('playing', onAudioPlaying);
        audio.addEventListener('canplay', syncAudio);
        audio.addEventListener('loadedmetadata', onAudioReady);
        audio.addEventListener('seeked', onAudioSeeked);
        audio.addEventListener('error', onAudioError);

        // Watchdog for the silent case: the track parsed but never actually started.
        //
        // Every path that can start the audio element is gated on the video's own
        // state - `syncAudio` refuses mid-seek, the supervisor timer refuses
        // mid-seek, and `alignAudio` refuses before metadata - so there are windows
        // where the track *cannot* have started and none of them are its fault.
        // A cold resume seek is the worst: the video holds `seeking` while it fetches
        // a range from far into the file (measured: a 172 MB fetch that outruns the
        // old 8s grace easily), and until that clears the audio element has never
        // even been asked to play. Reporting that as a dead track turned a slow seek
        // into a spurious 360p fallback.
        //
        // So: a legitimate wait pushes the deadline out, but a wait that runs past
        // `AUDIO_START_TIMEOUT` still fails - otherwise a track that genuinely never
        // loads would be deferred forever instead of falling back.
        const AUDIO_START_TIMEOUT = 30000;
        let waitingSince = 0;
        const watchdog = window.setInterval(() => {
            if (mode !== 'dash' || dashAudioStarted) return;
            if (video.paused || video.currentTime < 2) return;
            if (performance.now() < audioGraceUntil) return;

            const waiting =
                video.seeking ||
                audio.seeking ||
                audio.readyState < HTMLMediaElement.HAVE_CURRENT_DATA;
            if (waiting) {
                if (!waitingSince) waitingSince = performance.now();
                if (performance.now() - waitingSince < AUDIO_START_TIMEOUT) {
                    audioGraceUntil = performance.now() + 2000;
                    return;
                }
            } else {
                waitingSince = 0;
            }
            playerError = 'HD audio track did not start; switching to 360p.';
            mode = 'progressive';
        }, 2000);

        const onVideoSeeking = () => {
            // The playhead moved to somewhere that is not our resume target, so the
            // user scrubbed. The resume point must never be applied after that:
            // re-applying drags them forward to where they used to be, and each
            // round of that costs another cold fetch of a range just downloaded.
            if (pendingResumeAt !== null && !resumeApplied) {
                const target = resumeTarget();
                if (target !== null && Math.abs(video.currentTime - target) > 1) {
                    pendingResumeAt = null;
                    resumeApplied = true;
                }
            }
            alignAudio();
        };

        video.addEventListener('play', onPlay);
        video.addEventListener('pause', onPause);
        video.addEventListener('seeking', onVideoSeeking);
        video.addEventListener('volumechange', onVolume);
        video.addEventListener('ratechange', onRate);

        // Decoders started at different times and buffer at different rates, so the
        // two clocks walk apart. The supervisor below both restarts a track that
        // dropped out for any reason - a forward seek, a refill, the browser
        // suspending it in the background - and parks the picture when the gap is
        // too wide for a plain re-seek to close.
        /**
         * How far the two clocks may walk apart before they are corrected.
         *
         * Tight enough to stay well inside lip-sync, wide enough that ordinary
         * decoder jitter does not cost a seek on every tick.
         */
        const DRIFT_TOLERANCE = 0.15;
        /**
         * The gap past which a correction cannot converge on its own.
         *
         * Below this a re-seek is cheap - the track has data near the target. Past
         * it the target is usually somewhere the video has already left, so each
         * correction spends `D` seconds fetching and hands back a position that is
         * stale by exactly `D`. That is the case only `park` can close.
         */
        const DRIFT_COLD = 0.7;

        let sinceSeek = performance.now();
        const onSeeked = () => {
            const drift = audio.currentTime - video.currentTime;
            sinceSeek = performance.now();
            // The track now has to fetch a cold range for where we just landed, and
            // the video would otherwise spend those seconds running ahead of it -
            // which is precisely the gap the drift test then has to unwind. Park for
            // the duration whenever it is still fetching, or would be far enough
            // behind for the same chase to start.
            if (
                mode === 'dash' &&
                !video.paused &&
                (audio.seeking || Math.abs(drift) > DRIFT_COLD)
            ) {
                if (park()) return;
            }
            alignAudio();
            syncAudio();
        };
        video.addEventListener('seeked', onSeeked);

        // SponsorBlock auto-skip. Called every tick while playing: it only ever
        // *disables* itself (feature off, no segments) or seeks forward past a
        // segment the playhead has entered, so missing a tick is harmless.
        const sponsorIndexOf = (t: number): number => {
            if (sponsorSegments.length === 0) return -1;
            for (let i = 0; i < sponsorSegments.length; i++) {
                const s = sponsorSegments[i];
                if (t >= s.startSeconds && t < s.endSeconds) return i;
                // `sponsorSegments` is sorted by start; everything after starts
                // later, so nothing else can contain the playhead.
                if (s.startSeconds > t) return -1;
            }
            return -1;
        };
        const tryAutoSkipSponsor = (): void => {
            if (inSponsorSegment < 0) return;
            if (video.seeking) return;
            // Respect the post-seek grace the drift logic also uses: the user
            // (or the resume) just landed somewhere; let that settle first.
            if (performance.now() - sinceSeek < 1500) return;
            if (pendingResumeAt !== null && !resumeApplied) return;
            const s = sponsorSegments[inSponsorSegment];
            video.currentTime = s.endSeconds + 0.05;
            sinceSeek = performance.now();
        };

        const timer = window.setInterval(() => {
            // Parked: the picture is stopped while the track catches up. Only the
            // deadline matters here - `release` does the rest the moment it lands.
            if (catchingUp) {
                if (performance.now() > parkDeadline) release();
                return;
            }
            if (video.paused) return;
            // SponsorBlock: track where the playhead sits, and hop the segment
            // in auto mode. Runs before the audio sync so the seek below gets
            // the same drift handling as any other.
            if (settingBool('sponsorblock.enabled')) {
                inSponsorSegment = sponsorIndexOf(video.currentTime);
                if (settingStr('sponsorblock.mode') === 'auto') tryAutoSkipSponsor();
            } else if (inSponsorSegment !== -1) {
                inSponsorSegment = -1;
            }
            syncAudio();
            if (audio.paused || video.seeking) return;

            // While the audio element is seeking it is fetching a cold range and its
            // playhead is legitimately frozen. Hold off the stuck test until the seek
            // settles, otherwise a slow-but-healthy resume looks wedged and gets kicked.
            //
            // But a seek pointed at a position the video has already left is never
            // going to catch up, and waiting keeps the stuck test from ever running -
            // that is the skip-ahead case where the track went silent for good. After
            // a few seconds of that, abandon the stale fetch and re-aim it.
            if (audio.seeking) {
                if (!audioSeekingSince) {
                    audioSeekingSince = performance.now();
                } else if (performance.now() - audioSeekingSince > 6000) {
                    audioSeekingSince = performance.now();
                    if (Math.abs(audio.currentTime - video.currentTime) > 0.5) {
                        kickAudio();
                        return;
                    }
                    // On target, just slow: give the range another window rather
                    // than throwing away its progress.
                }
                audioStuckSince = 0;
                lastAudioPos = audio.currentTime;
                return;
            }
            audioSeekingSince = 0;

            // Is the audio playhead actually advancing? This - not `readyState` - is
            // the honest test for a wedged track: after an aborted seek the element
            // can be fully buffered and still report itself as playing forever.
            if (Math.abs(audio.currentTime - lastAudioPos) > 0.05) {
                lastAudioPos = audio.currentTime;
                audioStuckSince = 0;
            } else {
                if (!audioStuckSince) {
                    audioStuckSince = performance.now();
                } else if (performance.now() - audioStuckSince > 1500) {
                    kickAudio();
                    return;
                }
            }

            if (performance.now() - sinceSeek < 1500) return;
            const drift = audio.currentTime - video.currentTime;
            // Wide gap: a plain align would have to re-fetch from a position the
            // video is about to leave again, so it can chase forever - stop the
            // picture instead. Otherwise a small correction is the cheaper fix.
            if (Math.abs(drift) > DRIFT_COLD && park()) return;
            if (Math.abs(drift) > DRIFT_TOLERANCE) {
                alignAudio();
                sinceSeek = performance.now();
            }
        }, 500);

        return () => {
            window.clearInterval(timer);
            window.clearInterval(watchdog);
            video.removeEventListener('play', onPlay);
            video.removeEventListener('pause', onPause);
            video.removeEventListener('seeking', onVideoSeeking);
            video.removeEventListener('seeked', onSeeked);
            video.removeEventListener('volumechange', onVolume);
            video.removeEventListener('ratechange', onRate);
            audio.removeEventListener('playing', onAudioPlaying);
            audio.removeEventListener('canplay', syncAudio);
            audio.removeEventListener('loadedmetadata', onAudioReady);
            audio.removeEventListener('seeked', onAudioSeeked);
            audio.removeEventListener('error', onAudioError);
            audio.pause();
        };
    });

    /**
     * Flip between HD and the progressive 360p fallback.
     *
     * HD prefers `dash`, which is just the two adaptive tracks and needs no
     * remux, so this is instant. `muxed` is only reached if `dash` turned out to
     * be unplayable - it costs an ffmpeg pass, so it stays the second choice.
     */
    function toggleHd() {
        if (mode === 'dash' || mode === 'muxed') {
            mode = 'progressive';
            return;
        }
        if (mpegVariants.length === 0) {
            playerError = 'No HD track available. Playing 360p.';
            mode = 'progressive';
            return;
        }
        if (audioVariant) {
            // Instant: re-pointing the elements starts playing straight away.
            mode = 'dash';
            return;
        }
        mode = 'muxed';
        const id = videoId;
        if (!id) return;
        const gen = audioGen;
        void prepareHd(id, gen).then(() => {
            if (audioGen !== gen) return;
            if (muxState === 'failed') {
                playerError = muxError ?? 'HD stream unavailable; using 360p.';
                mode = 'progressive';
            }
        });
    }

    let sourceUrl = $derived(() => {
        if (!proxyAddr) return '';
        // The HD path is a plain URL like any other; the only difference is that
        // the proxy streams it while ffmpeg is still writing.
        if (mode === 'muxed') return muxUrl ?? '';
        // `dash` plays the video-only track; the audio rides along on its own
        // element, so this URL carries picture alone.
        if (mode === 'dash') return dashUrls?.video ?? '';
        if (selectedVariant) return `${proxyAddr}/stream/${selectedVariant.token}`;
        if (!streams) return '';
        if (streams.combined) return `${proxyAddr}/stream/${streams.combined}`;
        if (streams.hlsManifest) return `${proxyAddr}/stream/${streams.hlsManifest}`;
        if (streams.video) return `${proxyAddr}/stream/${streams.video}`;
        return '';
    });

    // Both modes are driven the same way now: a `src` the proxy serves. Waiting
    // for a URL keeps the element from loading anything until the HD remux has
    // produced its first fragment.
    $effect(() => {
        const url = sourceUrl();
        if (!videoEl || !url) return;
        if (videoEl.getAttribute('src') === url) return;

        // When the remux finishes, the file is renamed and the growing-file URL
        // we were streaming dies. The replacement is a different URL for the
        // same video, so carry the playhead over instead of snapping to 0.
        const resumeAt = videoEl.currentTime;
        const behindLiveEdge = videoEl.duration - resumeAt < 10;
        // A continue-watching seek that hasn't landed yet takes over on the
        // finalized file (it can always honour it via its own `canplay`); a
        // `currentTime` of ~0 would otherwise sidetrack it. Once it *has*
        // landed, or when this is a fresh watch, carry the playhead across the
        // swap.
        const resumeStillPending = pendingResumeAt !== null && !resumeApplied;

        // A swap within the same video (the remux completing, or a quality
        // change) must not disturb the user: if they paused it, keep it paused.
        // Opening a different video always autoplays, as does the first load.
        // An element that never reached a frame (`readyState` below
        // HAVE_CURRENT_DATA) is merely still buffering, not user-paused, so it
        // still starts.
        const currentId = videoId ?? null;
        const isNewVideo = srcVideoId !== currentId;
        const wasPlaying =
            !videoEl.paused || videoEl.readyState < HTMLMediaElement.HAVE_CURRENT_DATA;
        const shouldPlay = isNewVideo || wasPlaying;
        srcVideoId = currentId;

        videoEl.src = url;
        videoEl.load();

        const seekBack = () => {
            videoEl?.removeEventListener('loadedmetadata', seekBack);
            if (resumeStillPending) return;
            if (resumeAt > 0.5 && behindLiveEdge) {
                videoEl!.currentTime = resumeAt;
            }
        };
        videoEl.addEventListener('loadedmetadata', seekBack);

        // `play()` is called before any bytes have arrived, so it rejects with
        // AbortError and nothing would ever retry — the video would sit paused
        // until the user pressed play. Retry once the element can actually
        // play.
        const tryStart = () => {
            videoEl?.removeEventListener('canplay', tryStart);
            if (!videoEl) return;
            autoplayDone = true;
            // No resume seek here on purpose. Writing `currentTime` in the same tick
            // as `play()` aborts the play request, which is what left a resumed video
            // sitting at "Press play to start". The `playing` handler applies it once
            // playback is genuinely under way.
            if (!shouldPlay) return;
            void videoEl.play().catch(() => {
                // Genuinely blocked by autoplay policy; controls remain.
                playerError = 'Press play to start';
            });
        };
        if (videoEl.readyState >= HTMLMediaElement.HAVE_FUTURE_DATA) {
            tryStart();
        } else {
            videoEl.addEventListener('canplay', tryStart);
        }
    });

    // `videoEl` is often not bound yet when `load()` resolves, so re-try once both
    // the element and the settings exist. `volumeApplied` makes every later run a
    // no-op, which is what keeps this from fighting the in-session volume slider.
    $effect(() => {
        settingsReady;
        videoEl;
        applyDefaultVolume();
    });

    /**
     * Best rendition at or below `player.default_resolution`.
     *
     * The configured height is a ceiling, not a target: a 720p ceiling on a 4K
     * video picks the top 720p rendition, and a `0` ceiling means "highest
     * available". Nothing at or below the ceiling falls through to the smallest
     * rendition rather than refusing to play — a 144p ceiling on a 360p-only
     * video still has to work.
     */
    function pickDefaultHd(list: StreamVariantDto[]): number | null {
        if (list.length === 0) return null;
        const height = (v: StreamVariantDto) => v.height ?? 0;
        const cap = settingNum('player.default_resolution');
        const eligible = cap > 0 ? list.filter((v) => height(v) > 0 && height(v) <= cap) : list;
        if (eligible.length > 0) return eligible[0].itag;
        const smallest = [...list].sort((a, b) => height(a) - height(b))[0];
        return smallest.itag;
    }

    /**
     * Apply `player.default_volume` once, when the element first exists.
     *
     * Deliberately not re-applied per video: the user dragging the volume
     * slider during a session is their setting for the rest of that session,
     * and the configured default is only the starting point.
     */
    function applyDefaultVolume(): void {
        if (volumeApplied || !settingsReady || !videoEl) return;
        volumeApplied = true;
        videoEl.volume = Math.min(1, Math.max(0, settingNum('player.default_volume') / 100));
    }

    function variantLabel(v: StreamVariantDto): string {
        const parts: string[] = [];
        if (v.height) parts.push(`${v.height}p`);
        if (v.fps && v.fps >= 50) parts.push(`${v.fps}`);
        if (v.isHdr) parts.push('HDR');
        parts.push(`${Math.round(v.bitrate / 1000)}k`);
        if (v.qualityLabel && !parts.some((p) => v.qualityLabel?.includes(p))) {
            parts.unshift(v.qualityLabel);
        }
        return parts.join(' · ');
    }

    /**
     * Where a resumed video should seek to. The full runtime comes from the resolve
     * (`length`) rather than the element's own `duration`, and the target is held a
     * second short of the end so resuming never lands on the final frame.
     */
    function resumeTarget(): number | null {
        if (!pendingResumeAt) return null;
        const cap = length > 0 ? length - 1 : Number.MAX_VALUE;
        const target = Math.min(pendingResumeAt, cap);
        return target > 0.5 ? target : null;
    }

    /**
     * Apply the resume seek.
     *
     * There is deliberately no "is the target buffered yet" check here. That guard
     * existed for the ffmpeg remux, which grew as it was written and whose `buffered`
     * range was only ever the live edge — seeking past it was genuinely dropped by
     * the UA. The HD path is an ordinary ranged DASH URL now: the element issues a
     * range request for wherever it is sent, so *any* target is seekable as soon as
     * the resource has metadata. Keeping the guard would have capped resume to
     * roughly the first half-minute of a video, since a fresh load only ever buffers
     * from zero.
     *
     * The seek is confirmed by observing the playhead, so an attempt the UA clamped
     * or discarded re-arms and gets another shot on the next retry.
     */
    function tryResumeSeek(): void {
        const el = videoEl;
        if (!el || resumeApplied) return;
        // Wait for playback to genuinely start. A `currentTime` write while `play()`
        // is still resolving aborts it, and seeking a paused element that the user
        // may never press play on is wasted work anyway.
        if (!resumeArmed) return;
        const target = resumeTarget();
        if (target === null) return;
        if (!el.getAttribute('src')) return;
        if (el.readyState < HTMLMediaElement.HAVE_METADATA) return;
        // Never fight the user: if the playhead is already past the target, the
        // resume happened (or they skipped ahead themselves) and this is done.
        if (el.currentTime > target + 1) {
            resumeApplied = true;
            return;
        }
        el.currentTime = target;
        resumeApplied = true;
        // The audio track now has to fetch a range it has never touched. Give it
        // room before the watchdog is allowed to blame it.
        audioGraceUntil = performance.now() + 8000;
        // Setting `currentTime` is fire-and-forget. If the UA clamps or queues
        // the seek and the playhead never reaches the target, undo the claim so
        // a later retry (progress / durationchange / canplay) gets a shot.
        const t0 = el.currentTime;
        window.setTimeout(() => {
            const v = videoEl;
            if (v && v.currentTime < target - 1 && v.currentTime >= t0 - 0.5) {
                resumeApplied = false;
            }
        }, 250);
    }

    async function copyProxyUrl() {
        if (!sourceUrl()) return;
        try {
            await navigator.clipboard.writeText(sourceUrl());
        } catch {
            // ignore
        }
    }

    /**
     * Persist the playhead so "continue watching" can offer a resume later.
     * Throttled while playing (every ~5s, only when the position moved by >=2s);
     * `force` is used on pause, unload and navigation to capture the final spot.
     * The backend zeroes anything inside the first/last 5s of the video, so an
     * unfinished 30-second view keeps its resume point while a restarted or
     * finished one drops out on its own.
     */
    function reportProgress(force = false): void {
        const el = videoEl;
        const id = videoId;
        if (!el || !id || !el.getAttribute('src')) return;
        if (Number.isNaN(el.currentTime)) return;
        if (force && Date.now() - lastProgressReport.at < 1500) return;
        const pos = el.currentTime;
        // Never let a heartbeat destroy a resume point that has not been applied yet.
        // Until the seek lands the playhead sits near zero, and the backend treats
        // anything inside the first 5s as "not started" and stores 0 — which would
        // silently wipe the very position this page is about to resume from.
        if (pendingResumeAt !== null && !resumeApplied && pos < 10) return;
        if (!force && pos - lastProgressReport.pos < 2) return;
        if (!force && Date.now() - lastProgressReport.at < 5000) return;
        lastProgressReport = { at: Date.now(), pos };
        void api
            .historyProgress({
                videoId: id,
                positionSeconds: Math.round(pos),
                durationSeconds: Math.round(length > 0 ? length : (el.duration || 0))
            })
            .catch(() => {
                // Best-effort; losing a heartbeat must never break playback.
            });
    }

    /** Flush the playhead when the app closes. */
    function flushOnPageHide() {
        reportProgress(true);
    }

    // Flush the playhead whenever the user navigates away. `beforeNavigate` has no
    // unsubscribe, but it runs exactly once per mount and the guard inside
    // `reportProgress` makes a later stale call a no-op, so this cannot leak
    // meaningfully.
    beforeNavigate(() => reportProgress(true));

    onDestroy(() => {
        window.removeEventListener('pagehide', flushOnPageHide);
        window.removeEventListener('keydown', onKeydown);
        document.removeEventListener('fullscreenchange', onFullscreenChange);
        if (hideTimer) clearTimeout(hideTimer);
    });

    function back() {
        history.back();
    }

    function toggleTheater(): void {
        theater = !theater;
        setSetting('player.theater', theater ? 'true' : 'false');
    }

    /** Human labels for the skip-button text. */
    const SPONSOR_LABELS: Record<string, string> = {
        sponsor: 'sponsor',
        selfpromo: 'self-promotion',
        interaction: 'interaction',
        intro: 'intro',
        outro: 'outro',
        preview: 'intermission preview',
        hook: 'hook',
        filler: 'filler',
        music_offtopic: 'music break'
    };

    /**
     * Scrubber tint per SponsorBlock category. The class names are written out
     * in full on purpose: Tailwind only generates the names it can find in the
     * source, so a `bg-${color}` built at runtime would silently produce nothing.
     */
    const SPONSOR_SEGMENT_CLASSES: Record<string, string> = {
        sponsor: 'bg-amber-400/75',
        selfpromo: 'bg-violet-400/75',
        interaction: 'bg-sky-400/75',
        intro: 'bg-teal-400/75',
        outro: 'bg-teal-400/75',
        preview: 'bg-yellow-400/75',
        hook: 'bg-orange-400/75',
        filler: 'bg-slate-400/75',
        music_offtopic: 'bg-lime-400/75'
    };

    /** The segment the playhead is inside, if any. */
    function activeSponsorSegment(): SponsorSegmentDto | null {
        return inSponsorSegment >= 0 ? (sponsorSegments[inSponsorSegment] ?? null) : null;
    }

    /** The chapter the playhead is inside: -1 before the first chapter starts. */
    function chapterIndexOf(t: number): number {
        // `chapters` is sorted by startMs, so the active one is the last whose
        // start the playhead has reached.
        let index = -1;
        for (let i = 0; i < chapters.length; i++) {
            if (t >= chapters[i].startMs / 1000) {
                index = i;
            } else {
                break;
            }
        }
        return index;
    }

    /** Jump to a chapter. In `dash` the `seeked` handler re-aligns the audio track. */
    function seekToChapter(i: number): void {
        const c = chapters[i];
        if (!c) return;
        activeChapter = i;
        if (videoEl) videoEl.currentTime = c.startMs / 1000;
    }

    /** Chapter timestamps are always `m:ss`, `h:mm:ss` on the chip. */
    function formatChapterMs(ms: number): string {
        return formatDuration(ms / 1000);
    }

    /**
     * The active chapter follows the playhead in every playback mode (the dash
     * supervisor that handles SponsorBlock only runs for `dash`). `seeked` keeps
     * a click-while-paused honest: the chip lights up the moment the seek lands.
     */
    $effect(() => {
        const el = videoEl;
        if (!el || chapters.length === 0) return;
        const update = () => {
            if (Number.isNaN(el.currentTime)) return;
            activeChapter = chapterIndexOf(el.currentTime);
        };
        el.addEventListener('timeupdate', update);
        el.addEventListener('seeked', update);
        return () => {
            el.removeEventListener('timeupdate', update);
            el.removeEventListener('seeked', update);
        };
    });

    /** Keep the active chip in sight as playback moves through the strip. */
    $effect(() => {
        const i = activeChapter;
        if (i < 0 || !chapterRow) return;
        chapterRow
            .querySelector(`[data-chapter-index="${i}"]`)
            ?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
    });

    /** ── Custom player control bar (Catppuccin) ────────────────────────── */
    let playerFrame = $state<HTMLDivElement | null>(null);
    let playhead = $state(0);
    let duration = $state(0);
    let playing = $state(false);
    let volume = $state(1);
    let muted = $state(false);
    let controlsVisible = $state(true);
    let isFullscreen = $state(false);
    /** Settings (gear) and captions (CC) popovers mounted on the control bar. */
    let settingsOpen = $state(false);
    let captionsOpen = $state(false);
    let settingsRoot: HTMLDivElement | undefined = $state();
    let captionsRoot: HTMLDivElement | undefined = $state();
    /** Rates `<` and `>` step through, and the chip in the bar displays. */
    const SPEED_STEPS = [0.25, 0.5, 0.75, 1, 1.25, 1.5, 1.75, 2];
    /** Current playback rate, mirrored from the video element. */
    let speed = $state(1);
    /** Where the timeline has data buffered past the playhead, in percent. */
    let bufferedPct = $state(0);
    /** The element has nothing decoded to show yet: first load, or a refill. */
    let buffering = $state(false);
    /** Cursor position on the timeline as a 0..1 fraction, for the time bubble. */
    let hoverPct = $state<number | null>(null);
    /** Chapter under the cursor, so the bubble can name it the way YouTube does. */
    let hoverChapterIdx = $derived(
        hoverPct !== null && duration > 0 ? chapterIndexOf(hoverPct * duration) : -1
    );
    /**
     * Sponsor segments as positions on the scrubber, in percent, recomputed only
     * when the duration changes. Clamped to the bar so a segment that claims to
     * run past the end still stops at the edge instead of overflowing it.
     */
    let sponsorSpans = $derived(
        duration > 0
            ? sponsorSegments
                  .map((s) => {
                      const left = Math.min(1, Math.max(0, s.startSeconds / duration));
                      const right = Math.min(1, Math.max(left, s.endSeconds / duration));
                      return {
                          key: s.UUID,
                          category: s.category,
                          left: left * 100,
                          width: (right - left) * 100
                      };
                  })
                  .filter((s) => s.width > 0)
            : []
    );
    /**
     * Chapter boundaries as positions on the scrubber, in percent. The first
     * chapter always starts at zero, so it draws no divider of its own.
     */
    let chapterTicks = $derived(
        duration > 0
            ? chapters
                  .filter((c) => c.startMs > 0 && c.startMs / 1000 < duration)
                  .map((c) => (c.startMs / 1000 / duration) * 100)
            : []
    );
    let hideTimer: ReturnType<typeof setTimeout> | null = null;

    /** Keep the bar state mirroring the single `<video>` element. The dash
     *  supervisor mirrors play/pause/seek/volume onto the hidden audio track, so
     *  the custom controls only ever drive the video element. */
    $effect(() => {
        const el = videoEl;
        if (!el) return;
        const sync = () => {
            if (!Number.isNaN(el.currentTime)) playhead = el.currentTime;
            duration = el.duration > 0 ? el.duration : 0;
        };
        const syncPaused = () => {
            playing = !el.paused;
            pokeControls();
        };
        const syncVolume = () => {
            volume = el.volume;
            muted = el.muted;
        };
        const syncRate = () => {
            speed = el.playbackRate;
        };
        /** The range that contains the playhead: buffered ahead, not the whole file. */
        const syncBuffered = () => {
            const ranges = el.buffered;
            let end = 0;
            for (let i = 0; i < ranges.length; i++) {
                if (el.currentTime >= ranges.start(i) && el.currentTime <= ranges.end(i)) {
                    end = ranges.end(i);
                    break;
                }
            }
            bufferedPct = el.duration > 0 ? Math.min(100, (end / el.duration) * 100) : 0;
        };
        /**
         * True whenever the element cannot show the next frame yet, which covers
         * both the very first load and a stall mid-playback. `HAVE_FUTURE_DATA`
         * is the line: below it the next frame is not decoded, above it play can
         * continue without waiting.
         */
        const syncBuffering = () => {
            buffering = el.readyState < HTMLMediaElement.HAVE_FUTURE_DATA && !el.ended;
        };
        sync();
        syncPaused();
        syncVolume();
        syncRate();
        syncBuffered();
        syncBuffering();
        el.addEventListener('timeupdate', sync);
        el.addEventListener('durationchange', sync);
        el.addEventListener('loadedmetadata', sync);
        el.addEventListener('seeked', sync);
        el.addEventListener('play', syncPaused);
        el.addEventListener('pause', syncPaused);
        el.addEventListener('ended', syncPaused);
        el.addEventListener('volumechange', syncVolume);
        el.addEventListener('ratechange', syncRate);
        el.addEventListener('progress', syncBuffered);
        el.addEventListener('seeked', syncBuffered);
        el.addEventListener('waiting', syncBuffering);
        el.addEventListener('stalled', syncBuffering);
        el.addEventListener('canplay', syncBuffering);
        el.addEventListener('playing', syncBuffering);
        el.addEventListener('seeked', syncBuffering);
        el.addEventListener('ended', syncBuffering);
        return () => {
            el.removeEventListener('timeupdate', sync);
            el.removeEventListener('durationchange', sync);
            el.removeEventListener('loadedmetadata', sync);
            el.removeEventListener('seeked', sync);
            el.removeEventListener('play', syncPaused);
            el.removeEventListener('pause', syncPaused);
            el.removeEventListener('ended', syncPaused);
            el.removeEventListener('volumechange', syncVolume);
            el.removeEventListener('ratechange', syncRate);
            el.removeEventListener('progress', syncBuffered);
            el.removeEventListener('seeked', syncBuffered);
            el.removeEventListener('waiting', syncBuffering);
            el.removeEventListener('stalled', syncBuffering);
            el.removeEventListener('canplay', syncBuffering);
            el.removeEventListener('playing', syncBuffering);
            el.removeEventListener('seeked', syncBuffering);
            el.removeEventListener('ended', syncBuffering);
        };
    });

    function pokeControls(): void {
        controlsVisible = true;
        if (hideTimer) clearTimeout(hideTimer);
        hideTimer = null;
        // An open popover pins the bar: it overhangs the control-bar edge, so
        // leaving the bar to reach it must not hide the chrome underneath.
        if (videoEl && !videoEl.paused && !settingsOpen && !captionsOpen) {
            hideTimer = setTimeout(() => (controlsVisible = false), 2500);
        }
    }

    function togglePlay(): void {
        const el = videoEl;
        if (!el) return;
        if (el.paused) void el.play().catch(() => {});
        else el.pause();
    }

    function seekTo(t: number): void {
        if (!videoEl || Number.isNaN(t)) return;
        videoEl.currentTime = t;
        pokeControls();
    }

    function relativeSeek(d: number): void {
        if (videoEl) videoEl.currentTime += d;
        pokeControls();
    }

    function toggleMute(): void {
        if (videoEl) videoEl.muted = !videoEl.muted;
        pokeControls();
    }

    function setVolume(v: number): void {
        if (!videoEl) return;
        const clamped = Math.min(1, Math.max(0, v));
        videoEl.volume = clamped;
        if (videoEl.muted && clamped > 0) videoEl.muted = false;
        pokeControls();
    }

    function volumeStep(d: number): void {
        setVolume((muted ? 0 : volume) + d);
    }

    async function toggleFullscreen(): Promise<void> {
        if (document.fullscreenElement) {
            await document.exitFullscreen().catch(() => {});
        } else if (playerFrame?.requestFullscreen) {
            await playerFrame.requestFullscreen().catch(() => {});
        }
        pokeControls();
    }

    function onFullscreenChange(): void {
        isFullscreen = document.fullscreenElement != null;
        pokeControls();
    }

    /**
     * Set the playback rate on the element the user can see; the dash supervisor
     * mirrors it onto the audio track through `ratechange`, so a rate the UI
     * applies here cannot leave the two halves running at different speeds.
     */
    function setSpeed(rate: number): void {
        if (!videoEl) return;
        videoEl.playbackRate = rate;
        speed = rate;
        pokeControls();
    }

    /**
     * Apply a caption language selection. `''` turns captions off — the parsed
     * cues are dropped so the overlay goes blank. A real language warms the
     * backend VTT once per (video, language), fetches it as plain text, parses
     * it, and hands the cues to `updateActiveCaption` for rendering.
     */
    async function selectCaptions(lang: string): Promise<void> {
        captionError = null;
        captionCues = [];
        activeCaptionText = null;
        if (!lang) {
            activeCaptionLang = null;
            captionVttUrl = null;
            return;
        }
        activeCaptionLang = lang;
        captionVttUrl = null;
        try {
            const res = await api.captionsFetch(String(videoId), lang);
            // The user switched again while the fetch was in flight.
            if (activeCaptionLang !== lang) return;
            captionVttUrl = res.url;
            await loadCaptions(res.url);
        } catch (err) {
            if (activeCaptionLang !== lang) return;
            captionError = typeof err === 'string' ? err : 'Failed to load captions.';
        }
    }

    /** Fetch the proxied VTT as text and parse it into `captionCues`. */
    async function loadCaptions(url: string): Promise<void> {
        let body: string;
        try {
            const r = await fetch(url);
            if (!r.ok) throw `Captions request failed (${r.status}).`;
            body = await r.text();
        } catch (err) {
            captionError = typeof err === 'string' ? err : 'Failed to fetch captions.';
            return;
        }
        const cues = parseWebVtt(body);
        if (cues.length === 0) {
            captionError = 'The caption track contained no cues.';
            return;
        }
        captionCues = cues;
        updateActiveCaption();
    }

    /**
     * Cycle the active caption track: off → first track → … → last → off. The
     * `c` default binding lands on this; the CC popover picks directly instead.
     */
    function cycleCaptions(): void {
        if (captionTracks.length === 0) return;
        const idx = activeCaptionLang
            ? captionTracks.findIndex((t) => t.lang === activeCaptionLang)
            : -1;
        const next = idx >= captionTracks.length - 1 ? '' : captionTracks[idx + 1].lang;
        void selectCaptions(next);
    }

    /**
     * Dismiss the control-bar popovers on an outside press or `Escape`, like the
     * shared context menu. Capture phase, so a card's own handlers cannot eat it.
     */
    $effect(() => {
        if (!settingsOpen && !captionsOpen) return;
        const onPointerDown = (e: PointerEvent) => {
            if (settingsOpen && settingsRoot && !settingsRoot.contains(e.target as Node)) {
                settingsOpen = false;
            }
            if (captionsOpen && captionsRoot && !captionsRoot.contains(e.target as Node)) {
                captionsOpen = false;
            }
        };
        const onKeyDown = (e: KeyboardEvent) => {
            if (e.key === 'Escape') {
                settingsOpen = false;
                captionsOpen = false;
            }
        };
        document.addEventListener('pointerdown', onPointerDown, true);
        document.addEventListener('keydown', onKeyDown, true);
        return () => {
            document.removeEventListener('pointerdown', onPointerDown, true);
            document.removeEventListener('keydown', onKeyDown, true);
        };
    });

    /**
     * Pick the cue covering `videoEl.currentTime` and surface its text. Driven
     * by `timeupdate`, so it does no work while the media is still.
     */
    function updateActiveCaption(): void {
        if (!captionCues.length || !videoEl) {
            activeCaptionText = null;
            return;
        }
        const t = videoEl.currentTime;
        let found: { start: number; end: number; text: string } | null = null;
        for (const c of captionCues) {
            if (t >= c.start && t < c.end && (!found || c.start >= found.start)) {
                found = c;
            }
        }
        activeCaptionText = found?.text ?? null;
    }

    /**
     * Minimal WebVTT parser: skips the header and `STYLE`/`NOTE`/`REGION`
     * blocks, reads `HH:MM:SS.mmm` (or `MM:SS.mmm`) timing lines followed by
     * their text up to the blank line, strips inline tags for plain display.
     */
    function parseWebVtt(vtt: string): { start: number; end: number; text: string }[] {
        const cues: { start: number; end: number; text: string }[] = [];
        const lines = vtt.replace(/\r\n/g, '\n').split('\n');
        let i = 0;
        if (lines[0]?.startsWith('\uFEFF')) lines[0] = lines[0].slice(1);
        if (lines[i]?.trim() !== 'WEBVTT') return cues;
        i++;
        while (i < lines.length) {
            const line = lines[i];
            i++;
            if (!line) continue;
            const head = line.trim().toUpperCase();
            if (head === 'STYLE' || head === 'NOTE' || head === 'REGION') {
                while (i < lines.length && lines[i] !== '') i++;
                continue;
            }
            const m = line.match(
                /^(\d{2}:)?(\d{2}):(\d{2})[.,](\d{3})\s*-->\s*(\d{2}:)?(\d{2}):(\d{2})[.,](\d{3})/
            );
            if (!m) continue;
            // Groups: 1/5 optional `HH:` (with colon), 2/6 minutes, 3/7 seconds,
            // 4/8 milliseconds. The minutes/seconds/ms indices don't shift when
            // the hours group is absent — only its own group goes unmatched.
            const toSec = (h: string | undefined, min: string, s: string, ms: string) =>
                (h ? Number(h.slice(0, -1)) : 0) * 3600 + Number(min) * 60 + Number(s) + Number(ms) / 1000;
            const start = toSec(m[1], m[2], m[3], m[4]);
            const end = toSec(m[5], m[6], m[7], m[8]);
            const text: string[] = [];
            while (i < lines.length && lines[i] !== '') {
                text.push(lines[i]);
                i++;
            }
            if (text.length > 0) {
                cues.push({
                    start,
                    end,
                    text: text.join('\n').replace(/<[^>]+>/g, ''),
                });
            }
        }
        return cues;
    }

    /** The bar button: wrap around rather than stop at 2×. */
    function cycleSpeed(): void {
        const i = Math.max(0, SPEED_STEPS.indexOf(speed));
        setSpeed(SPEED_STEPS[(i + 1) % SPEED_STEPS.length]);
    }

    /** `<` and `>`: step towards the ends, stopping there. */
    function stepSpeed(dir: 1 | -1): void {
        const i = Math.max(0, SPEED_STEPS.indexOf(speed));
        setSpeed(SPEED_STEPS[Math.min(SPEED_STEPS.length - 1, Math.max(0, i + dir))]);
    }

    /**
     * `,` and `.`: one frame at a time, only while paused — the frame's own
     * duration is the right step, so the rendition's fps decides how far to move
     * rather than a guess that is wrong on 24fps content.
     */
    function stepFrame(dir: 1 | -1): void {
        const el = videoEl;
        if (!el || !el.paused || duration <= 0) return;
        const fps = activeVideoVariant?.fps || 30;
        const step = 1 / fps;
        el.currentTime = Math.min(Math.max(0, el.currentTime + dir * step), duration - step);
    }

    function onTimelineMove(e: PointerEvent): void {
        const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
        hoverPct =
            rect.width > 0 ? Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width)) : null;
    }

    /**
     * Bindings come from `$lib/shortcuts`, shared with the Settings editor: the
     * default key always matches, and `player.shortcuts.<action>` adds an
     * alternative for it. `settingStr` returns '' when nothing is stored, which
     * is exactly "no alternative".
     */
    function actionMatching(e: KeyboardEvent): string | null {
        const combo = comboOf(e).toLowerCase();
        for (const action of Object.keys(DEFAULT_SHORTCUTS)) {
            if (DEFAULT_SHORTCUTS[action].toLowerCase() === combo) return action;
        }
        for (const action of Object.keys(DEFAULT_SHORTCUTS)) {
            const alt = settingStr(`player.shortcuts.${action}`).toLowerCase();
            if (alt && alt === combo) return action;
        }
        return null;
    }

    function onKeydown(e: KeyboardEvent): void {
        const el = document.activeElement;
        if (el && /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName)) return;
        if (!videoEl) return;
        // Digits and Space are fixed bindings (seek-to-percentage, play/pause),
        // so they are matched before the configurable ones and never while a
        // modifier is down — `Ctrl+0` is then free to be an alternative.
        const bare = !e.ctrlKey && !e.altKey && !e.metaKey;
        if (bare && e.key === ' ') {
            togglePlay();
            e.preventDefault();
            return;
        }
        if (bare && /^[0-9]$/.test(e.key)) {
            if (duration > 0) videoEl.currentTime = (Number(e.key) / 10) * duration;
            return;
        }
        // Fixed pairs: `,`/`.` step a frame while paused, `<`/`>` (Shift with
        // them) step the playback speed. Both live here rather than in the
        // binding table so they cannot be shadowed by an alternative.
        if (bare && (e.key === ',' || e.key === '.')) {
            stepFrame(e.key === ',' ? -1 : 1);
            e.preventDefault();
            return;
        }
        if (bare && (e.key === '<' || e.key === '>')) {
            stepSpeed(e.key === '<' ? -1 : 1);
            e.preventDefault();
            return;
        }
        const action = actionMatching(e);
        if (!action) return;
        e.preventDefault();
        switch (action) {
            case 'playpause':
                togglePlay();
                break;
            case 'back10':
                relativeSeek(-10);
                break;
            case 'forward10':
                relativeSeek(10);
                break;
            case 'back5':
                relativeSeek(-5);
                break;
            case 'forward5':
                relativeSeek(5);
                break;
            case 'volumeUp':
                volumeStep(0.1);
                break;
            case 'volumeDown':
                volumeStep(-0.1);
                break;
            case 'mute':
                toggleMute();
                break;
            case 'fullscreen':
                void toggleFullscreen();
                break;
            case 'theater':
                toggleTheater();
                break;
            case 'captions':
                cycleCaptions();
                break;
        }
    }
</script>

<div class="flex flex-col gap-4 p-6">
    <!-- The same related card renders in two places: a 2-wide grid below the
         player in theater mode, and a stacked rail beside it otherwise. `stacked`
         only picks the container; the card markup is identical so hover
         pre-resolve and duration badges cannot drift between the two. -->
    {#snippet relatedCards(stacked: boolean)}
        <div class={stacked ? 'flex flex-col gap-1' : 'grid grid-cols-1 gap-x-6 gap-y-4 sm:grid-cols-2'}>
            {#each related as r (r.videoId)}
                <button
                    type="button"
                    data-ctx="card"
                    data-video-id={r.videoId}
                    data-title={r.title}
                    data-author={r.author}
                    data-thumbnail={r.thumbnails[r.thumbnails.length - 1]?.url ?? ''}
                    data-duration={r.durationSeconds ?? 0}
                    data-channel-id={r.channelId ?? ''}
                    class="group flex gap-3 rounded-lg p-2 text-left transition-colors hover:bg-muted/40"
                    onclick={() => goto(`/watch/${r.videoId}`)}
                    onmouseenter={() => preResolve(r.videoId)}
                    onfocus={() => preResolve(r.videoId)}
                >
                    <div class="relative aspect-video w-40 shrink-0 overflow-hidden rounded-md bg-muted">
                        {#if r.thumbnails.length > 0}
                            <img
                                src={r.thumbnails[r.thumbnails.length - 1].url}
                                alt=""
                                class="h-full w-full object-cover"
                                loading="lazy"
                            />
                        {/if}
                        {#if r.durationSeconds}
                            <span
                                class="absolute right-1 bottom-1 rounded bg-black/70 px-1.5 py-0.5 text-[10px] font-medium text-white"
                            >
                                {formatDuration(r.durationSeconds)}
                            </span>
                        {/if}
                    </div>
                    <div class="min-w-0 flex-1">
                        <p class="line-clamp-2 text-sm font-medium leading-snug group-hover:text-primary">
                            {r.title}
                        </p>
                        <p class="mt-1 text-xs text-muted-foreground">{r.author}</p>
                        {#if r.viewCount}
                            <p class="text-xs text-muted-foreground">{r.viewCount} views</p>
                        {/if}
                    </div>
                </button>
            {/each}
        </div>
    {/snippet}

    <button
        type="button"
        onclick={back}
        class="flex items-center gap-2 self-start rounded-md px-2 py-1 text-sm text-muted-foreground hover:bg-muted hover:text-foreground"
    >
        <HugeiconsIcon icon={ArrowLeft01Icon} strokeWidth={2} class="h-4 w-4" />
        Back
    </button>

    {#if loading}
        <div class="flex items-center gap-2 text-sm text-muted-foreground">
            <span
                class="h-4 w-4 animate-spin rounded-full border-2 border-muted-foreground/30 border-t-muted-foreground"
                aria-hidden="true"
            ></span>
            Loading video…
        </div>
    {/if}

    {#if error}
        <div class="rounded-xl border border-destructive/30 bg-destructive/10 p-4 text-sm">
            <p class="font-medium text-destructive">Could not load this video</p>
            <p class="mt-1 text-destructive/80">{error}</p>
            <p class="mt-2 text-xs text-muted-foreground">
                Try a different video. Some videos are geo-restricted, age-gated, or
                rate-limited by YouTube.
            </p>
        </div>
    {/if}

    {#if info}
        <div class="grid grid-cols-1 gap-6 {theater || related.length === 0 ? 'lg:grid-cols-1' : 'lg:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]'}">
            <div class="flex min-w-0 flex-col gap-3">
                <div
                    class="player-frame relative overflow-hidden rounded-xl bg-black"
                    bind:this={playerFrame}
                    data-ctx="player"
                    data-video-id={info.details.videoId}
                    data-title={info.details.title}
                    data-author={info.details.author}
                    data-thumbnail={info?.details.thumbnails[info.details.thumbnails.length - 1]?.url ?? ''}
                    data-duration={info.details.lengthSeconds}
                    data-channel-id={info.details.channelId}
                >
                    <!-- svelte-ignore a11y_media_has_caption -->
                    <video
                        bind:this={videoEl}
                        class="aspect-video w-full cursor-pointer"
                        onclick={togglePlay}
                        ontimeupdate={() => {
                            reportProgress();
                            updateActiveCaption();
                        }}
                        onseeked={() => updateActiveCaption()}
                        onpause={() => reportProgress(true)}
                        onended={() => reportProgress(true)}
                        onplaying={() => {
                            resumeArmed = true;
                            tryResumeSeek();
                        }}
                        ondurationchange={() => tryResumeSeek()}
                        onprogress={() => tryResumeSeek()}
                        onmouseenter={pokeControls}
                        onmousemove={pokeControls}
                        onmouseleave={() => {
                            if (videoEl && !videoEl.paused && !settingsOpen && !captionsOpen) {
                                controlsVisible = false;
                            }
                        }}
                    ></video>

                    <!--
                        `dash` audio. Kept out of the layout: it is driven entirely
                        by the video element above, and the visible controls belong to
                        that one so the user has a single thing to operate.
                    -->
                    {#if mode === 'dash'}
                        <!-- svelte-ignore a11y_media_has_caption -->
                        <audio bind:this={audioEl} class="hidden"></audio>
                    {/if}

                    <!--
                        Buffering: nothing is decoded to show yet. Drawn above the
                        picture but below the control bar, which is what the native
                        controls did and what the bar's own hide timer expects.
                    -->
                    {#if buffering}
                        <div
                            class="pointer-events-none absolute inset-0 flex items-center justify-center"
                            aria-hidden="true"
                        >
                            <div
                                class="h-9 w-9 animate-spin rounded-full border-2 border-white/25 border-t-white"
                            ></div>
                        </div>
                    {/if}

                    <!--
                        Big centre play button, shown whenever the picture is
                        paused and there is something decoded to look at. Below the
                        bar's stacking order on purpose: the bar is the primary
                        control surface, this is the fallback for someone who just
                        clicked the frame.
                    -->
                    {#if videoEl && !playing && !buffering && !playerError}
                        <button
                            type="button"
                            aria-label="Play"
                            class="pointer-events-auto absolute inset-0 z-[5] flex items-center justify-center"
                            onclick={(e) => {
                                e.stopPropagation();
                                togglePlay();
                            }}
                        >
                            <span
                                class="flex h-14 w-14 items-center justify-center rounded-full bg-black/40 text-white transition-[background-color,transform] duration-150 hover:scale-105 hover:bg-black/60"
                            >
                                <svg
                                    viewBox="0 0 12 12"
                                    class="ml-0.5 h-6 w-6"
                                    aria-hidden="true"
                                >
                                    <polygon points="3,2 10,6 3,10" fill="currentColor" />
                                </svg>
                            </span>
                        </button>
                    {/if}

                    <!--
                        Rendered captions. WebView2's native text-track pipeline
                        rejects the cross-origin VTT, so cues are drawn here over
                        the picture (below the control bar), matching when
                        `updateActiveCaption` says a cue covers the playhead.
                    -->
                    {#if activeCaptionText}
                        <div
                            class="pointer-events-none absolute inset-x-0 bottom-16 flex justify-center px-3"
                            aria-live="polite"
                        >
                            <p
                                class="max-w-[90%] text-center text-base leading-snug font-medium text-white [text-shadow:0_1px_2px_rgba(0,0,0,0.9),0_0_4px_rgba(0,0,0,0.9)] sm:text-lg"
                                style:font-family={captionFont || undefined}
                            >
                                {activeCaptionText}
                            </p>
                        </div>
                    {/if}

                    <!-- Custom Catppuccin control bar -->
                    <div
                        class="pointer-events-none absolute inset-x-0 bottom-0 flex flex-col gap-1 bg-gradient-to-t from-black/70 via-black/30 to-transparent px-3 pb-2 pt-8 transition-opacity duration-200 {controlsVisible
                            ? 'opacity-100'
                            : 'opacity-0'}"
                        onmouseenter={() => {
                            controlsVisible = true;
                            if (hideTimer) clearTimeout(hideTimer);
                        }}
                        onmouseleave={() => {
                            if (videoEl && !videoEl.paused && !settingsOpen && !captionsOpen) {
                                controlsVisible = false;
                            }
                        }}
                    >
                        <div
                            class="group relative mb-1 h-1.5 w-full cursor-pointer pointer-events-auto"
                            role="slider"
                            aria-label="Seek"
                            aria-valuemin={0}
                            aria-valuemax={duration || 0}
                            aria-valuenow={playhead}
                            tabindex="0"
                            onpointermove={onTimelineMove}
                            onpointerleave={() => (hoverPct = null)}
                            onclick={(e) => {
                                const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
                                if (rect.width <= 0 || duration <= 0) return;
                                const pct = (e.clientX - rect.left) / rect.width;
                                seekTo(pct * duration);
                            }}
                        >
                            <div class="absolute inset-0 rounded-full bg-white/20"></div>
                            <!-- Buffered ahead of the playhead, under the played fill. -->
                            <div
                                class="absolute inset-y-0 left-0 rounded-full bg-white/35"
                                style="width: {bufferedPct}%"
                                aria-hidden="true"
                            ></div>
                            <div
                                class="absolute inset-y-0 left-0 rounded-full bg-primary"
                                style="width: {duration > 0 ? (playhead / duration) * 100 : 0}%"
                            ></div>
                            <!--
                                Sponsor segments sit above the played fill, so a
                                segment the playhead has already passed stays
                                visible instead of being repainted primary.
                            -->
                            {#each sponsorSpans as seg (seg.key)}
                                <div
                                    class="absolute inset-y-0 {SPONSOR_SEGMENT_CLASSES[seg.category] ??
                                        'bg-slate-400/75'}"
                                    style="left: {seg.left}%; width: {seg.width}%"
                                    aria-hidden="true"
                                ></div>
                            {/each}
                            <!--
                                Chapter boundaries, drawn last so they read as
                                gaps in whatever is underneath — this is what
                                makes a chaptered video scannable at a glance,
                                and why the strip below the player and the bar
                                always agree.
                            -->
                            {#each chapterTicks as left (left)}
                                <div
                                    class="absolute inset-y-0 w-0.5 bg-black/60"
                                    style="left: {left}%"
                                    aria-hidden="true"
                                ></div>
                            {/each}
                            {#if hoverPct !== null && duration > 0}
                                <div
                                    class="absolute -top-7 max-w-[16rem] -translate-x-1/2 truncate rounded bg-black/80 px-1.5 py-0.5 text-[10px] text-white"
                                    style="left: {hoverPct * 100}%"
                                >
                                    {formatDuration(hoverPct * duration)}{#if hoverChapterIdx >= 0 && chapters[hoverChapterIdx]?.title}
                                        · {chapters[hoverChapterIdx].title}{/if}
                                </div>
                                <div
                                    class="absolute -top-1 h-2 w-2 -translate-x-1/2 rounded-full bg-primary shadow"
                                    style="left: {hoverPct * 100}%"
                                ></div>
                            {/if}
                        </div>

                        <div class="flex items-center justify-between gap-2 pointer-events-auto">
                            <div class="flex items-center gap-1">
                                <button
                                    type="button"
                                    class="flex h-8 w-8 items-center justify-center rounded hover:bg-white/10"
                                    aria-label={playing ? 'Pause' : 'Play'}
                                    onclick={togglePlay}
                                >
                                    {#if playing}
                                        <svg viewBox="0 0 12 12" class="h-4 w-4 text-white" aria-hidden="true">
                                            <rect x="3" y="2" width="2" height="8" fill="currentColor" />
                                            <rect x="7" y="2" width="2" height="8" fill="currentColor" />
                                        </svg>
                                    {:else}
                                        <svg viewBox="0 0 12 12" class="h-4 w-4 text-white" aria-hidden="true">
                                            <polygon points="3,2 10,6 3,10" fill="currentColor" />
                                        </svg>
                                    {/if}
                                </button>

                                <button
                                    type="button"
                                    class="flex h-8 w-8 items-center justify-center rounded hover:bg-white/10"
                                    aria-label={muted ? 'Unmute' : 'Mute'}
                                    onclick={toggleMute}
                                >
                                    {#if muted || volume === 0}
                                        <svg viewBox="0 0 12 12" class="h-4 w-4 text-white" aria-hidden="true">
                                            <path d="M2 4.5 H5 L7 2.5 V9.5 L5 7.5 H2 V4.5 Z" fill="currentColor" />
                                            <line x1="9" y1="4" x2="10" y2="8" stroke="currentColor" stroke-width="1" stroke-linecap="round" />
                                            <line x1="7" y1="4.5" x2="10.5" y2="2.5" stroke="currentColor" stroke-width="1" stroke-linecap="round" />
                                        </svg>
                                    {:else}
                                        <svg viewBox="0 0 12 12" class="h-4 w-4 text-white" aria-hidden="true">
                                            <path d="M2 4.5 H5 L7 2.5 V9.5 L5 7.5 H2 V4.5 Z" fill="currentColor" />
                                            <path d="M8 4 A2 2 0 0 1 8 8" stroke="currentColor" stroke-width="1" fill="none" />
                                            <path d="M9 3 A3 3 0 0 1 9 9" stroke="currentColor" stroke-width="1" fill="none" />
                                        </svg>
                                    {/if}
                                </button>

                                <input
                                    type="range"
                                    min="0"
                                    max="1"
                                    step="0.01"
                                    value={muted ? 0 : volume}
                                    class="h-1 w-20 cursor-pointer"
                                    oninput={(e) => setVolume(Number(e.currentTarget.value))}
                                />

                                <span class="ml-1 text-[11px] text-white/80">
                                    {formatDuration(playhead)} / {formatDuration(duration || 0)}
                                </span>
                            </div>

                            <div class="flex items-center gap-1">
                                <button
                                    type="button"
                                    class="flex h-8 min-w-8 items-center justify-center rounded px-1.5 text-[11px] font-medium {speed === 1
                                        ? 'text-white'
                                        : 'text-primary'} hover:bg-white/10"
                                    aria-label="Playback speed"
                                    title="Playback speed — Shift+, / Shift+. keys"
                                    onclick={cycleSpeed}
                                >
                                    {speed}×
                                </button>

                                {#if captionTracks.length > 0}
                                    <div class="relative" bind:this={captionsRoot}>
                                        <button
                                            type="button"
                                            class="flex h-8 w-8 items-center justify-center rounded {captionsOpen
                                                ? 'bg-white/10'
                                                : ''} hover:bg-white/10 {activeCaptionLang
                                                ? 'text-primary'
                                                : 'text-white'}"
                                            aria-label="Captions"
                                            aria-haspopup="menu"
                                            aria-expanded={captionsOpen}
                                            title="Captions (c)"
                                            onclick={() => {
                                                captionsOpen = !captionsOpen;
                                                settingsOpen = false;
                                            }}
                                        >
                                            <svg viewBox="0 0 12 12" class="h-4 w-4" aria-hidden="true">
                                                <rect x="0.8" y="2.6" width="10.4" height="6.8" rx="1.4" fill="none" stroke="currentColor" stroke-width="1" />
                                                <path d="M4.4 4.7 A 1.25 1.25 0 0 0 3.2 6 A 1.25 1.25 0 0 0 4.4 7.3 A 1.3 1.3 0 0 0 5.3 7.05" fill="none" stroke="currentColor" stroke-width="0.9" stroke-linecap="round" />
                                                <path d="M7.8 4.7 A 1.25 1.25 0 0 0 6.6 6 A 1.25 1.25 0 0 0 7.8 7.3 A 1.3 1.3 0 0 0 8.7 7.05" fill="none" stroke="currentColor" stroke-width="0.9" stroke-linecap="round" />
                                            </svg>
                                        </button>
                                        {#if captionsOpen}
                                            <div
                                                role="menu"
                                                aria-label="Caption tracks"
                                                tabindex="-1"
                                                class="absolute bottom-full right-0 z-50 mb-2 min-w-44 overflow-hidden rounded-lg border bg-popover py-1 text-popover-foreground shadow-lg"
                                            >
                                                <button
                                                    type="button"
                                                    role="menuitem"
                                                    class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm transition-colors hover:bg-muted hover:text-foreground {!activeCaptionLang
                                                        ? 'text-primary'
                                                        : ''}"
                                                    onclick={() => {
                                                        void selectCaptions('');
                                                        captionsOpen = false;
                                                    }}
                                                >
                                                    <span class="w-4">
                                                        {#if !activeCaptionLang}
                                                            <svg viewBox="0 0 12 12" class="h-3.5 w-3.5" aria-hidden="true">
                                                                <path d="M2.5 6.5 L5 9 L9.5 3.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
                                                            </svg>
                                                        {/if}
                                                    </span>
                                                    Off
                                                </button>
                                                {#each captionTracks as c (`${c.lang}\u0000${c.label}`)}
                                                    <button
                                                        type="button"
                                                        role="menuitem"
                                                        class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm transition-colors hover:bg-muted hover:text-foreground {activeCaptionLang === c.lang
                                                            ? 'text-primary'
                                                            : ''}"
                                                        onclick={() => {
                                                            void selectCaptions(c.lang);
                                                            captionsOpen = false;
                                                        }}
                                                    >
                                                        <span class="w-4">
                                                            {#if activeCaptionLang === c.lang}
                                                                <svg viewBox="0 0 12 12" class="h-3.5 w-3.5" aria-hidden="true">
                                                                    <path d="M2.5 6.5 L5 9 L9.5 3.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
                                                                </svg>
                                                            {/if}
                                                        </span>
                                                        <span class="line-clamp-1">{c.label}</span>
                                                    </button>
                                                {/each}
                                            </div>
                                        {/if}
                                    </div>
                                {/if}

                                <div class="relative" bind:this={settingsRoot}>
                                    <button
                                        type="button"
                                        class="flex h-8 w-8 items-center justify-center rounded {settingsOpen
                                            ? 'bg-white/10'
                                            : ''} hover:bg-white/10 text-white"
                                        aria-label="Settings"
                                        aria-haspopup="menu"
                                        aria-expanded={settingsOpen}
                                        title="Settings"
                                        onclick={() => {
                                            settingsOpen = !settingsOpen;
                                            captionsOpen = false;
                                        }}
                                    >
                                        <svg viewBox="0 0 12 12" class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1" aria-hidden="true">
                                            <circle cx="6" cy="6" r="1.5" fill="currentColor" stroke="none" />
                                            <path d="M6 1.5 L6 2.6" stroke-linecap="round" />
                                            <path d="M6 9.4 L6 10.5" stroke-linecap="round" />
                                            <path d="M1.5 6 L2.6 6" stroke-linecap="round" />
                                            <path d="M9.4 6 L10.5 6" stroke-linecap="round" />
                                            <path d="M3.18 3.18 L3.97 3.97" stroke-linecap="round" />
                                            <path d="M8.03 8.03 L8.82 8.82" stroke-linecap="round" />
                                            <path d="M8.82 3.18 L8.03 3.97" stroke-linecap="round" />
                                            <path d="M3.97 8.03 L3.18 8.82" stroke-linecap="round" />
                                        </svg>
                                    </button>
                                    {#if settingsOpen}
                                        <div
                                            role="menu"
                                            aria-label="Player settings"
                                            tabindex="-1"
                                            class="absolute bottom-full right-0 z-50 mb-2 w-64 overflow-hidden rounded-lg border bg-popover py-2 text-popover-foreground shadow-lg"
                                        >
                                            <p class="px-3 pb-1 text-[10px] font-semibold tracking-wider text-muted-foreground uppercase">
                                                Speed
                                            </p>
                                            <div class="flex flex-wrap gap-1 px-3">
                                                {#each SPEED_STEPS as s (s)}
                                                    <button
                                                        type="button"
                                                        role="menuitemradio"
                                                        aria-checked={speed === s}
                                                        class="rounded px-1.5 py-0.5 text-[11px] font-medium {speed === s
                                                            ? 'bg-primary text-primary-foreground'
                                                            : 'hover:bg-muted'}"
                                                        onclick={() => setSpeed(s)}
                                                    >
                                                        {s}×
                                                    </button>
                                                {/each}
                                            </div>
                                            {#if videoVariants.length > 0 && variants.length > 0}
                                                <div class="my-1.5 border-t"></div>
                                                <p class="px-3 pb-1 text-[10px] font-semibold tracking-wider text-muted-foreground uppercase">
                                                    Quality
                                                </p>
                                                <div class="px-3">
                                                    <select
                                                        class="h-8 w-full rounded-md border bg-card px-2 text-xs outline-hidden focus:ring-1 focus:ring-ring"
                                                        value={mode === 'progressive' ? (selectedItag ?? '') : (selectedVideoItag ?? '')}
                                                        onchange={(e) => {
                                                            const raw = e.currentTarget.value;
                                                            if (mode === 'progressive') {
                                                                selectedItag = raw === '' ? null : Number(raw);
                                                                videoEl?.load();
                                                            } else {
                                                                selectedVideoItag = raw === '' ? null : Number(raw);
                                                                if (mode === 'muxed') {
                                                                    // The remux is per-rendition, so a new pick
                                                                    // needs a new one started. Bump the
                                                                    // generation so the in-flight poll stops.
                                                                    audioGen += 1;
                                                                    muxUrl = null;
                                                                    if (videoId) void prepareHd(videoId, audioGen);
                                                                }
                                                                // In `dash` this is all it takes: `sourceUrl`
                                                                // re-derives and the element picks up the new
                                                                // video-only track, with no ffmpeg involved.
                                                            }
                                                        }}
                                                    >
                                                        {#if mode === 'progressive'}
                                                            {#each variants as v (v.itag)}
                                                                <option value={v.itag}>{variantLabel(v)}</option>
                                                            {/each}
                                                        {:else}
                                                            {#each mpegVariants as v (v.itag)}
                                                                <option value={v.itag}>{variantLabel(v)}</option>
                                                            {/each}
                                                        {/if}
                                                    </select>
                                                </div>
                                                <div class="px-3 pt-1.5 pb-0.5">
                                                    <button
                                                        type="button"
                                                        role="menuitem"
                                                        class="flex w-full items-center rounded-md border px-2 py-1 text-xs hover:bg-muted"
                                                        onclick={toggleHd}
                                                    >
                                                        {mode === 'progressive' ? 'Use HD (adaptive)' : 'Use 360p fallback'}
                                                    </button>
                                                </div>
                                            {/if}
                                        </div>
                                    {/if}
                                </div>

                                <button
                                    type="button"
                                    class="flex h-8 w-8 items-center justify-center rounded hover:bg-white/10"
                                    aria-label="Theater"
                                    title="Theater (t)"
                                    onclick={toggleTheater}
                                >
                                    <svg viewBox="0 0 12 12" class="h-4 w-4 text-white" aria-hidden="true">
                                        <rect x="1.5" y="3" width="9" height="6" fill="none" stroke="currentColor" stroke-width="1" />
                                    </svg>
                                </button>
                                <button
                                    type="button"
                                    class="flex h-8 w-8 items-center justify-center rounded hover:bg-white/10"
                                    aria-label={isFullscreen ? 'Exit fullscreen' : 'Fullscreen'}
                                    title="Fullscreen (f)"
                                    onclick={toggleFullscreen}
                                >
                                    {#if isFullscreen}
                                        <svg viewBox="0 0 12 12" class="h-4 w-4 text-white" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                                            <path d="M4.5 4.5 L4.5 2.5 M4.5 4.5 L2.5 4.5" />
                                            <path d="M7.5 7.5 L7.5 9.5 M7.5 7.5 L9.5 7.5" />
                                        </svg>
                                    {:else}
                                        <svg viewBox="0 0 12 12" class="h-4 w-4 text-white" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                                            <path d="M4.5 2.5 L4.5 4.5 M2.5 4.5 L4.5 4.5" />
                                            <path d="M7.5 9.5 L7.5 7.5 M9.5 7.5 L7.5 7.5" />
                                            <path d="M7.5 2.5 L7.5 4.5 M9.5 4.5 L7.5 4.5" />
                                            <path d="M4.5 9.5 L4.5 7.5 M2.5 7.5 L4.5 7.5" />
                                        </svg>
                                    {/if}
                                </button>
                            </div>
                        </div>
                    </div>
                </div>

                {#if playerError}
                    <p class="text-xs text-destructive/90">{playerError}</p>
                {/if}
                {#if captionError}
                    <p class="text-xs text-destructive/90">{captionError}</p>
                {/if}

                <div class="flex flex-wrap items-center gap-2">
                    <button
                        type="button"
                        title="Theater mode (`t`)"
                        class="h-8 rounded-md border px-2 text-xs hover:bg-accent"
                        onclick={toggleTheater}
                    >
                        {theater ? 'Exit theater' : 'Theater'}
                    </button>
                    <button
                        type="button"
                        title="Format table and stream diagnostics"
                        class="h-8 rounded-md border px-2 text-xs hover:bg-accent"
                        onclick={() => (streamDetailsOpen = !streamDetailsOpen)}
                    >
                        Stream details
                    </button>
                    {#if settingBool('sponsorblock.enabled') && settingStr('sponsorblock.mode') === 'manual' && activeSponsorSegment()}
                        <button
                            type="button"
                            title="Skip this segment"
                            class="h-8 rounded-md border border-primary/40 px-2 text-xs hover:bg-accent"
                            onclick={() => {
                                const s = activeSponsorSegment();
                                if (s && videoEl) videoEl.currentTime = s.endSeconds + 0.05;
                            }}
                        >
                            Skip {SPONSOR_LABELS[activeSponsorSegment()?.category ?? ''] ??
                                activeSponsorSegment()?.category ??
                                'segment'}
                        </button>
                    {/if}
                </div>

                {#if chapters.length > 0}
                    <div
                        bind:this={chapterRow}
                        role="list"
                        aria-label="Chapters"
                        class="flex gap-2 overflow-x-auto pb-1"
                    >
                        {#each chapters as c, i (c.startMs)}
                            <button
                                type="button"
                                data-chapter-index={i}
                                title={`Jump to ${formatChapterMs(c.startMs)}`}
                                class="flex h-10 shrink-0 items-center gap-2 rounded-lg border px-2 text-xs transition-colors {i === activeChapter
                                    ? 'border-primary/60 bg-primary/10'
                                    : 'border-border hover:bg-muted/50'}"
                                onclick={() => seekToChapter(i)}
                            >
                                {#if c.thumbUrl}
                                    <img src={c.thumbUrl} alt="" class="h-6 w-10 rounded object-cover" />
                                {/if}
                                <span class="whitespace-nowrap">
                                    <span class="tabular-nums text-muted-foreground">{formatChapterMs(c.startMs)}</span>
                                    <span class="ml-1.5 font-medium">{c.title}</span>
                                </span>
                            </button>
                        {/each}
                    </div>
                {/if}

                <div class="flex flex-col gap-1">
                    <h1 class="font-heading text-xl font-semibold leading-snug">{title}</h1>
                    <div class="flex items-center gap-3 text-sm text-muted-foreground">
                        {#if channelId}
                            <a
                                href={`/channel/${channelId}`}
                                class="font-medium text-foreground hover:text-primary hover:underline"
                            >
                                {author}
                            </a>
                        {:else}
                            <span>{author}</span>
                        {/if}
                        {#if length > 0}
                            <span>·</span>
                            <span>{formatDuration(length)}</span>
                        {/if}
                        {#if releaseDate}
                            <span>·</span>
                            <span>{releaseDate}</span>
                        {/if}
                    </div>
                </div>

                {#if description}
                    <details class="rounded-xl bg-card p-4 text-sm">
                        <summary class="cursor-pointer text-xs text-muted-foreground">Description</summary>
                        <p class="mt-2 whitespace-pre-wrap text-foreground/80">{description}</p>
                    </details>
                {/if}

                {#if theater && related.length > 0}
                    <div class="flex flex-col gap-2 pt-3">
                        <h2 class="font-heading text-sm font-semibold text-muted-foreground">Related</h2>
                        {@render relatedCards(false)}
                    </div>
                {/if}
            </div>

            {#if !theater && related.length > 0}
            <aside class="flex min-w-0 flex-col gap-2">
                <h2 class="font-heading text-sm font-semibold text-muted-foreground">Related</h2>
                {@render relatedCards(true)}
            </aside>
            {/if}
        </div>

        {#if streamDetailsOpen}
            <div
                role="dialog"
                aria-modal="true"
                aria-label="Stream details"
                tabindex="-1"
                class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
                onclick={(e) => {
                    if (e.target === e.currentTarget) streamDetailsOpen = false;
                }}
                onkeydown={(e) => {
                    if (e.key === 'Escape') streamDetailsOpen = false;
                }}
            >
                <div
                    class="flex max-h-[85vh] w-[min(40rem,94vw)] flex-col gap-3 overflow-y-auto rounded-lg border bg-background p-4 shadow-xl"
                >
                    <div class="flex items-center justify-between gap-3">
                        <h2 class="font-heading text-sm font-semibold">Stream details</h2>
                        <button
                            type="button"
                            aria-label="Close"
                            class="rounded p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
                            onclick={() => (streamDetailsOpen = false)}
                        >
                            ✕
                        </button>
                    </div>

                    <div class="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
                        <span>mode: <span class="font-medium text-foreground">{mode}</span></span>
                        {#if info.details.viewCount}
                            <span>
                                views:
                                <span class="font-medium text-foreground">{formatViews(info.details.viewCount)}</span>
                            </span>
                        {/if}
                        {#if releaseDate}
                            <span>published: <span class="font-medium text-foreground">{releaseDate}</span></span>
                        {/if}
                        {#if muxState !== 'idle'}
                            <span>remux: <span class="font-medium text-foreground">{muxState}</span></span>
                        {/if}
                        {#if muxError}
                            <span class="text-destructive">remux error: {muxError}</span>
                        {/if}
                        {#if playerError}
                            <span class="text-destructive">player: {playerError}</span>
                        {/if}
                    </div>

                    {#if info.adaptiveFormats.length > 0}
                        <div class="overflow-hidden rounded-xl border bg-card">
                            <table class="w-full text-xs">
                                <thead class="text-left text-muted-foreground">
                                    <tr>
                                        <th class="px-3 py-2">itag</th>
                                        <th class="px-3 py-2">kind</th>
                                        <th class="px-3 py-2">format</th>
                                        <th class="px-3 py-2">codec</th>
                                        <th class="px-3 py-2">bitrate</th>
                                        <th class="px-3 py-2">size</th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {#each info.adaptiveFormats.slice(0, 12) as f}
                                        <tr class="border-t">
                                            <td class="px-3 py-1.5 font-mono">{f.itag}</td>
                                            <td class="px-3 py-1.5">
                                                {f.isCombined ? 'AV' : f.isVideoOnly ? 'V' : 'A'}
                                            </td>
                                            <td class="px-3 py-1.5">
                                                {#if f.width}
                                                    {f.width}×{f.height ?? ''}
                                                    {#if f.fps && f.fps > 30}
                                                        · {f.fps}fps
                                                    {/if}
                                                {:else if f.isAudioOnly}
                                                    {audioSpecs(f)}
                                                {:else}
                                                    —
                                                {/if}
                                            </td>
                                            <td class="px-3 py-1.5 font-mono">{f.codec ?? '—'}</td>
                                            <td class="px-3 py-1.5">{Math.round(f.bitrate / 1000)} kbps</td>
                                            <td class="px-3 py-1.5">{formatBytes(f.contentLength)}</td>
                                        </tr>
                                    {/each}
                                </tbody>
                            </table>
                        </div>
                    {/if}

                    {#if sourceUrl()}
                        <button
                            type="button"
                            onclick={copyProxyUrl}
                            class="self-start rounded-md border px-3 py-1 text-xs hover:bg-accent"
                        >
                            Copy proxy URL
                        </button>
                    {/if}
                </div>
            </div>
        {/if}
    {/if}
</div>

<style>
    /*
     * The player must never run off the bottom of the window. The frame is
     * width:100% with a 16:9 video, so on a window that is wide relative to its
     * height the width is what overflows. Capping the width to the height
     * budget times the aspect ratio keeps the video (and its controls) fully on
     * screen at any window shape; `margin-inline: auto` centres the frame.
     */
    .player-frame {
        width: 100%;
        margin-inline: auto;
        max-width: min(100%, calc((100vh - 10rem) * (16 / 9)));
    }
</style>
