//! NewPipe subscription-export parsing (spec item 1).
//!
//! Deliberately network-free: every URL shape and malformed row is decided
//! here, so it can be tested without spending InnerTube quota. `commands.rs`
//! owns the part that needs the network (resolving vanity URLs) and the
//! database writes.

use std::collections::HashSet;

use serde::Deserialize;

/// NewPipe keeps SoundCloud subscriptions in the same array as YouTube ones.
/// `0` is YouTube; anything else has no channel id for us.
const YOUTUBE_SERVICE_ID: i64 = 0;

/// A parsed NewPipe export.
///
/// Everything is optional: NewPipe omits `app_version` and `groups` entirely
/// when there are none, older versions wrote `name` as `""`, and a
/// hand-edited file can be missing `url` altogether. None of that should fail
/// the whole import.
#[derive(Debug, Default, Deserialize)]
pub struct NewPipeFile {
    #[serde(default)]
    pub app_version: Option<String>,
    #[serde(default)]
    pub subscriptions: Vec<NewPipeSubscription>,
    #[serde(default)]
    pub groups: Vec<NewPipeGroup>,
}

#[derive(Debug, Deserialize)]
pub struct NewPipeSubscription {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub service_id: Option<i64>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct NewPipeGroup {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub subscriptions: Vec<String>,
}

/// What a subscription URL points at.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChannelRef {
    /// `.../channel/UC…`, or a bare id: already canonical, costs no request.
    Id(String),
    /// `.../c/…`, `.../user/…`, `.../@handle`: needs one `/browse` to resolve.
    Vanity(String),
}

impl ChannelRef {
    /// The id when it is already known, otherwise the string to hand
    /// `get_channel_about` as a `browseId`.
    pub fn browse_id(&self) -> &str {
        match self {
            ChannelRef::Id(id) => id,
            ChannelRef::Vanity(v) => v,
        }
    }

    /// Whether importing this entry has to ask InnerTube anything: either the
    /// id is not canonical yet, or the file carried no usable name.
    fn needs_lookup(&self, name: &str) -> bool {
        matches!(self, ChannelRef::Vanity(_)) || name.trim().is_empty()
    }
}

/// One subscription row after normalisation, ready to be written.
#[derive(Debug, Clone)]
pub struct PlannedSub {
    pub channel: ChannelRef,
    /// May be empty; the caller falls back to the channel id.
    pub name: String,
    pub thumbnail: String,
}

/// A NewPipe group, which becomes a category.
#[derive(Debug, Clone)]
pub struct PlannedGroup {
    pub name: String,
    pub channels: Vec<ChannelRef>,
}

/// What an import will do, before any network or database work.
#[derive(Debug, Default)]
pub struct Plan {
    pub subscriptions: Vec<PlannedSub>,
    pub groups: Vec<PlannedGroup>,
    /// Rows already dropped as unusable: other services, unparseable URLs,
    /// duplicate rows, nameless groups.
    pub skipped: usize,
    /// Distinct channels that will need an InnerTube lookup, in file order.
    /// The caller caps this — it is the only part that costs quota.
    pub lookups: Vec<ChannelRef>,
}

/// Parse raw file contents into a NewPipe export.
pub fn parse(json: &str) -> Result<NewPipeFile, String> {
    let file: NewPipeFile = serde_json::from_str(json.trim())
        .map_err(|e| format!("that does not look like a NewPipe export: {e}"))?;
    if file.subscriptions.is_empty() && file.groups.is_empty() {
        return Err("that file has no subscriptions in it".into());
    }
    Ok(file)
}

/// Decide what an import of `file` will do, without touching the network.
pub fn plan(file: &NewPipeFile) -> Plan {
    let mut plan = Plan::default();
    let mut seen: HashSet<ChannelRef> = HashSet::new();
    let mut queued: HashSet<ChannelRef> = HashSet::new();

    for sub in &file.subscriptions {
        if sub.service_id.unwrap_or(YOUTUBE_SERVICE_ID) != YOUTUBE_SERVICE_ID {
            plan.skipped += 1;
            continue;
        }
        let Some(channel) = channel_ref(&sub.url) else {
            plan.skipped += 1;
            continue;
        };
        // The same channel twice (two URL shapes, one id) must be written once.
        if !seen.insert(channel.clone()) {
            plan.skipped += 1;
            continue;
        }
        let name = sub.name.as_deref().unwrap_or_default().trim().to_string();
        if channel.needs_lookup(&name) {
            queue(&mut plan.lookups, &mut queued, &channel);
        }
        plan.subscriptions.push(PlannedSub {
            channel,
            name,
            thumbnail: sub.avatar_url.as_deref().unwrap_or_default().trim().to_string(),
        });
    }

    for group in &file.groups {
        let name = group.name.as_deref().unwrap_or_default().trim().to_string();
        if name.is_empty() {
            plan.skipped += 1;
            continue;
        }
        let mut channels: Vec<ChannelRef> = Vec::new();
        for url in &group.subscriptions {
            let Some(channel) = channel_ref(url) else {
                plan.skipped += 1;
                continue;
            };
            if channels.contains(&channel) {
                continue;
            }
            // An `Id` member never needs a lookup of its own; its id is the
            // join key already.
            if matches!(channel, ChannelRef::Vanity(_)) {
                queue(&mut plan.lookups, &mut queued, &channel);
            }
            channels.push(channel);
        }
        plan.groups.push(PlannedGroup { name, channels });
    }

    plan
}

fn queue(lookups: &mut Vec<ChannelRef>, queued: &mut HashSet<ChannelRef>, channel: &ChannelRef) {
    if queued.insert(channel.clone()) {
        lookups.push(channel.clone());
    }
}

/// Classify a subscription URL, accepting every form NewPipe writes.
///
/// `/channel/UC…` is the only shape that yields the id offline. The legacy
/// `/c/…` and `/user/…` paths and `@handle`s are vanity URLs: YouTube resolves
/// them server-side, so they are passed through as a `browseId` and cost one
/// request (see `commands::subscription_import`).
pub fn channel_ref(raw: &str) -> Option<ChannelRef> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    // Some exports (and hand-pasted rows) hold a bare channel id.
    if is_channel_id(raw) {
        return Some(ChannelRef::Id(raw.to_string()));
    }

    // Strip scheme, then query/fragment, then walk the path.
    let rest = raw.split_once("://").map_or(raw, |(_, rest)| rest);
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let mut segments = rest.split('/').filter(|s| !s.is_empty());

    let host = segments.next()?;
    if !host.to_ascii_lowercase().contains("youtube.") {
        return None;
    }
    let first = segments.next()?;
    match first {
        "channel" => {
            let id = segments.next()?;
            is_channel_id(id).then(|| ChannelRef::Id(id.to_string()))
        }
        // Legacy vanity paths. Innertube follows the redirect, so the leading
        // segment is kept as part of the browse id.
        "c" | "user" => segments
            .next()
            .map(|name| ChannelRef::Vanity(format!("{first}/{name}"))),
        handle if handle.starts_with('@') => Some(ChannelRef::Vanity(handle.to_string())),
        _ => None,
    }
}

/// A canonical channel id: `UC` plus 22 url-safe characters. Anything else in a
/// `/channel/` slot is not an id we can store, so it is left to a lookup rather
/// than written as if it were one.
fn is_channel_id(s: &str) -> bool {
    s.len() == 24
        && s.starts_with("UC")
        && s[2..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a real 50-channel export: no `app_version`, no `groups`,
    /// one empty name and one trailing space, all `service_id: 0`.
    const REAL_EXPORT: &str = r#"{
      "subscriptions": [
        { "name": "", "service_id": 0, "url": "https://www.youtube.com/channel/UCsdD3quGf01RWABJt8wLe9g" },
        { "name": "3Blue1Brown", "service_id": 0, "url": "https://www.youtube.com/channel/UCYO_jab_esuFRV4b17AJtAw" },
        { "name": "euanthecooper ", "service_id": 0, "url": "https://www.youtube.com/channel/UCGB0aKcaJlo_rwbYYnTEC2g",
          "avatar_url": "https://yt3.ggpht.com/avatar=s176" }
      ]
    }"#;

    #[test]
    fn a_real_export_plans_with_no_network_work() {
        let plan = plan(&parse(REAL_EXPORT).unwrap());
        assert_eq!(plan.subscriptions.len(), 3);
        assert_eq!(plan.skipped, 0);
        assert!(plan.groups.is_empty());
        // Only the row whose name was blank needs a lookup.
        assert_eq!(plan.lookups.len(), 1);
        assert_eq!(plan.lookups[0], ChannelRef::Id("UCsdD3quGf01RWABJt8wLe9g".into()));
    }

    #[test]
    fn names_are_trimmed_and_avatars_kept() {
        let plan = plan(&parse(REAL_EXPORT).unwrap());
        let eu = plan
            .subscriptions
            .iter()
            .find(|s| s.name.starts_with("euanthecooper"))
            .unwrap();
        assert_eq!(eu.name, "euanthecooper", "trailing space trimmed");
        assert_eq!(eu.thumbnail, "https://yt3.ggpht.com/avatar=s176");
    }

    #[test]
    fn every_url_shape_is_recognised() {
        let id = "UCYO_jab_esuFRV4b17AJtAw";
        for url in [
            format!("https://www.youtube.com/channel/{id}"),
            format!("https://youtube.com/channel/{id}/"),
            format!("https://m.youtube.com/channel/{id}?sub_confirmation=1"),
            format!("https://music.youtube.com/channel/{id}#about"),
            format!("www.youtube.com/channel/{id}"),
            id.to_string(),
        ] {
            assert_eq!(channel_ref(&url), Some(ChannelRef::Id(id.into())), "{url}");
        }
        assert_eq!(
            channel_ref("https://www.youtube.com/c/Fireship"),
            Some(ChannelRef::Vanity("c/Fireship".into()))
        );
        assert_eq!(
            channel_ref("https://www.youtube.com/user/GoogleDevelopers/videos"),
            Some(ChannelRef::Vanity("user/GoogleDevelopers".into()))
        );
        assert_eq!(
            channel_ref("https://www.youtube.com/@mkbhd"),
            Some(ChannelRef::Vanity("@mkbhd".into()))
        );
        assert_eq!(
            channel_ref("https://www.youtube.com/@mkbhd/videos"),
            Some(ChannelRef::Vanity("@mkbhd".into()))
        );
    }

    #[test]
    fn unusable_urls_are_rejected_rather_than_stored() {
        for url in [
            "",
            "   ",
            "https://example.com/channel/UCYO_jab_esuFRV4b17AJtAw",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/channel/short",
            "https://www.youtube.com/playlist?list=PL123",
        ] {
            assert_eq!(channel_ref(url), None, "{url}");
        }
    }

    #[test]
    fn other_services_are_skipped() {
        let json = r#"{"subscriptions":[
            {"name":"YT","service_id":0,"url":"https://www.youtube.com/channel/UCYO_jab_esuFRV4b17AJtAw"},
            {"name":"SC","service_id":1,"url":"https://soundcloud.com/artist"}
        ]}"#;
        let plan = plan(&parse(json).unwrap());
        assert_eq!(plan.subscriptions.len(), 1);
        assert_eq!(plan.skipped, 1);
    }

    #[test]
    fn a_missing_service_id_is_treated_as_youtube() {
        let json = r#"{"subscriptions":[{"name":"YT","url":"https://www.youtube.com/channel/UCYO_jab_esuFRV4b17AJtAw"}]}"#;
        assert_eq!(plan(&parse(json).unwrap()).subscriptions.len(), 1);
    }

    #[test]
    fn the_same_channel_twice_is_written_once() {
        let json = r#"{"subscriptions":[
            {"name":"A","url":"https://www.youtube.com/channel/UCYO_jab_esuFRV4b17AJtAw"},
            {"name":"A again","url":"https://m.youtube.com/channel/UCYO_jab_esuFRV4b17AJtAw?x=1"}
        ]}"#;
        let plan = plan(&parse(json).unwrap());
        assert_eq!(plan.subscriptions.len(), 1);
        assert_eq!(plan.skipped, 1, "the duplicate is reported, not silently lost");
    }

    #[test]
    fn vanity_urls_and_nameless_rows_are_queued_for_lookup_once() {
        let json = r#"{"subscriptions":[
            {"name":"Handle","url":"https://www.youtube.com/@mkbhd"},
            {"name":"Same handle","url":"https://www.youtube.com/@mkbhd/videos"},
            {"name":"","url":"https://www.youtube.com/channel/UCYO_jab_esuFRV4b17AJtAw"},
            {"name":"","url":"https://www.youtube.com/channel/UCsdD3quGf01RWABJt8wLe9g"}
        ]}"#;
        let plan = plan(&parse(json).unwrap());
        assert_eq!(
            plan.subscriptions.len(),
            3,
            "@mkbhd and @mkbhd/videos are one channel"
        );
        assert_eq!(plan.skipped, 1, "the duplicate is reported");
        assert_eq!(
            plan.lookups,
            vec![
                ChannelRef::Vanity("@mkbhd".into()),
                ChannelRef::Id("UCYO_jab_esuFRV4b17AJtAw".into()),
                ChannelRef::Id("UCsdD3quGf01RWABJt8wLe9g".into()),
            ]
        );
    }

    #[test]
    fn groups_become_categories_with_their_members() {
        let json = r#"{
            "app_version": "0.23.3",
            "app_version_int": 1044,
            "subscriptions": [
                {"name":"A","url":"https://www.youtube.com/channel/UCYO_jab_esuFRV4b17AJtAw"},
                {"name":"B","url":"https://www.youtube.com/channel/UCsdD3quGf01RWABJt8wLe9g"},
                {"name":"C","url":"https://www.youtube.com/@mkbhd"}
            ],
            "groups": [
                {"name":"Music","subscriptions":[
                    "https://www.youtube.com/channel/UCYO_jab_esuFRV4b17AJtAw",
                    "https://www.youtube.com/@mkbhd"
                ]},
                {"name":"  ","subscriptions":["https://www.youtube.com/channel/UCsdD3quGf01RWABJt8wLe9g"]},
                {"name":"News","subscriptions":[
                    "https://www.youtube.com/channel/UCYO_jab_esuFRV4b17AJtAw"
                ]}
            ]
        }"#;
        let plan = plan(&parse(json).unwrap());
        assert_eq!(plan.subscriptions.len(), 3);
        assert_eq!(plan.groups.len(), 2, "the nameless group is dropped");
        assert_eq!(plan.groups[0].name, "Music");
        assert_eq!(
            plan.groups[0].channels,
            vec![
                ChannelRef::Id("UCYO_jab_esuFRV4b17AJtAw".into()),
                ChannelRef::Vanity("@mkbhd".into())
            ]
        );
        // The same channel in two groups is expected; the caller accumulates
        // before writing so replace-all does not drop the first one.
        assert_eq!(plan.groups[1].name, "News");
        assert_eq!(plan.groups[1].channels.len(), 1);
        // The vanity subscription and the vanity group member are one lookup.
        assert_eq!(
            plan.lookups,
            vec![ChannelRef::Vanity("@mkbhd".into())]
        );
        assert_eq!(plan.skipped, 1, "the nameless group");
    }

    #[test]
    fn browse_id_is_what_the_lookup_sends() {
        assert_eq!(
            ChannelRef::Id("UCabc".into()).browse_id(),
            "UCabc"
        );
        assert_eq!(ChannelRef::Vanity("@mkbhd".into()).browse_id(), "@mkbhd");
        assert_eq!(ChannelRef::Vanity("c/Fire".into()).browse_id(), "c/Fire");
    }

    #[test]
    fn junk_input_fails_with_a_readable_message() {
        assert!(parse("not json at all").unwrap_err().contains("NewPipe"));
        // A JSON array is a structurally valid but empty document: serde will
        // happily read it as a struct with every field defaulted.
        assert!(parse("[]").unwrap_err().contains("no subscriptions"));
        assert!(parse(r#"{"subscriptions":[]}"#)
            .unwrap_err()
            .contains("no subscriptions"));
        assert!(parse(r#"{"app_version":"0.23.3"}"#)
            .unwrap_err()
            .contains("no subscriptions"));
    }

    #[test]
    fn a_row_without_a_url_does_not_fail_the_file() {
        let json = r#"{"subscriptions":[
            {"name":"no url here"},
            {"name":"A","url":"https://www.youtube.com/channel/UCYO_jab_esuFRV4b17AJtAw"}
        ]}"#;
        let plan = plan(&parse(json).unwrap());
        assert_eq!(plan.subscriptions.len(), 1);
        assert_eq!(plan.skipped, 1);
    }
}
