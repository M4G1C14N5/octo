//! `getNowPlaying` with the outside songs playing now.
//!
//! Navidrome lists only what it was told is playing, and an outside song's scrobble never
//! reaches it (#60). So the last now-playing scrobble (`submission=false`) of an outside song is
//! kept here, per listener and player, and joins Navidrome's answer as an entry the way
//! Navidrome writes its own. An entry goes when the same player starts another song, or after
//! [`EXPIRY`] (sooner when the song's length is known and has run out), as Navidrome's do.
//! The family filter (`own_now_playing`) runs on the joined answer, so members still see only
//! their own.

use std::collections::HashMap;
use std::sync::LazyLock;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use bytes::Bytes;
use chrono::{DateTime, TimeDelta, Utc};
use octo_core::models::domain::Song;
use octo_subsonic::xml::XElement;
use parking_lot::Mutex;
use serde_json::Value;

use super::caller_checks::{MAX_REQUEST_BODY, SubsonicCall};
use crate::app::AppState;
use crate::http::routes::RouteSet;
use crate::services::family::list_rules::{Edit, edit};
use crate::services::i_music_metadata_service::IMusicMetadataService;

/// How long an outside song stays in Now Playing after its now-playing scrobble, at most.
pub const EXPIRY: TimeDelta = TimeDelta::minutes(10);
/// Kept past a known song length, for pauses and a late next scrobble.
const GRACE: TimeDelta = TimeDelta::seconds(60);
/// The largest Navidrome answer read to add the outside songs to.
const ANSWER_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playing {
    pub username: String,
    pub player: String,
    pub song_id: String,
    pub since: DateTime<Utc>,
}

/// Outside songs playing now, by lower-case listener and player.
#[derive(Default)]
pub struct OutsideNowPlaying {
    playing: Mutex<HashMap<(String, String), Playing>>,
}

pub static OUTSIDE_NOW_PLAYING: LazyLock<OutsideNowPlaying> = LazyLock::new(OutsideNowPlaying::default);

impl OutsideNowPlaying {
    /// A now-playing scrobble: `outside` is whether `song_id` is an outside song. Any song the
    /// player starts replaces what it played; only an outside one is kept.
    pub fn started(&self, username: &str, player: &str, song_id: &str, outside: bool, at: DateTime<Utc>) {
        let key = (username.trim().to_lowercase(), player.to_string());
        let mut playing = self.playing.lock();
        if outside && !song_id.is_empty() {
            playing.insert(
                key,
                Playing {
                    username: username.trim().to_string(),
                    player: player.to_string(),
                    song_id: song_id.to_string(),
                    since: at,
                },
            );
        } else {
            playing.remove(&key);
        }
    }

    /// What is playing at `now`, oldest first. `length` gives a song's seconds when known.
    /// Entries past their time are dropped.
    pub fn current(&self, now: DateTime<Utc>, length: impl Fn(&str) -> Option<i64>) -> Vec<Playing> {
        let mut playing = self.playing.lock();
        playing.retain(|_, p| now - p.since < EXPIRY);
        let mut list: Vec<Playing> = playing
            .values()
            .filter(|p| {
                length(&p.song_id)
                    .filter(|s| *s > 0)
                    .is_none_or(|s| now - p.since < TimeDelta::seconds(s) + GRACE)
            })
            .cloned()
            .collect();
        list.sort_by(|a, b| (a.since, &a.username, &a.player).cmp(&(b.since, &b.username, &b.player)));
        list
    }

    #[cfg(test)]
    pub fn clear_user(&self, username: &str) {
        let user = username.to_lowercase();
        self.playing.lock().retain(|(u, _), _| *u != user);
    }
}

/// The player a scrobble came from: the client name, as Navidrome's Now Playing shows it.
pub(crate) fn player_of(call: &SubsonicCall) -> String {
    call.param_opt("c").unwrap_or_default().to_string()
}

pub fn routes() -> RouteSet {
    RouteSet::new().subsonic("getNowPlaying", get(get_now_playing).post(get_now_playing))
}

/// Navidrome's `getNowPlaying`, with the outside songs playing now added.
pub async fn get_now_playing(State(state): State<AppState>, req: Request) -> axum::response::Response {
    let (parts, body) = req.into_parts();
    let Ok(body) = axum::body::to_bytes(body, MAX_REQUEST_BODY).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let answer =
        crate::http::catch_all::catch_all(State(state.clone()), Request::from_parts(parts, Body::from(body)))
            .await;
    if answer.status() != StatusCode::OK || answer.headers().contains_key(header::CONTENT_ENCODING) {
        return answer;
    }
    let now = Utc::now();
    // Lengths come from what the metadata already knows; nothing new is looked up.
    let mut songs: HashMap<String, Song> = HashMap::new();
    for p in OUTSIDE_NOW_PLAYING.current(now, |_| None) {
        if !songs.contains_key(&p.song_id)
            && let Some(song) = outside_song(&state, &p.song_id).await
        {
            songs.insert(p.song_id.clone(), song);
        }
    }
    let playing = OUTSIDE_NOW_PLAYING.current(now, |id| songs.get(id).and_then(|s| s.duration).map(i64::from));
    if playing.is_empty() {
        return answer;
    }
    let (mut parts, body) = answer.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, ANSWER_LIMIT).await else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let edited = with_outside(&state, &bytes, &playing, &songs, now);
    let bytes = match edited {
        Edit::Changed(changed) => Bytes::from(changed),
        Edit::Unchanged | Edit::Unreadable => bytes,
    };
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.remove(header::TRANSFER_ENCODING);
    axum::response::Response::from_parts(parts, Body::from(bytes))
}

/// The outside song as the metadata knows it, else as its routing remembers it.
async fn outside_song(state: &AppState, id: &str) -> Option<Song> {
    let (_, provider, external_id) = state.local_library.parse_song_id(id);
    let known = state
        .music_metadata
        .get_song(
            provider.as_deref().unwrap_or_default(),
            external_id.as_deref().unwrap_or_default(),
        )
        .await;
    let mut song = match known {
        Some(song) => song,
        None => {
            let routing = state.external_id_registry.lookup(id).map(|r| r.snapshot())?;
            Song {
                title: routing.title.clone().unwrap_or_default(),
                artist: routing.artist.clone().unwrap_or_default(),
                album: routing.album.clone().unwrap_or_default(),
                duration: routing.duration.filter(|d| *d > 0),
                ..Default::default()
            }
        }
    };
    song.id = id.to_string();
    song.is_local = false;
    let mut one = vec![song];
    state.music_metadata.refresh_outside_songs(&mut one);
    one.pop()
}

fn with_outside(
    state: &AppState,
    bytes: &[u8],
    playing: &[Playing],
    songs: &HashMap<String, Song>,
    now: DateTime<Utc>,
) -> Edit {
    let builder = &state.subsonic_response_builder;
    let shown: Vec<(&Playing, Song)> = playing
        .iter()
        .map(|p| {
            let song = songs.get(&p.song_id).cloned().unwrap_or_else(|| Song {
                id: p.song_id.clone(),
                is_local: false,
                ..Default::default()
            });
            (p, song)
        })
        .collect();
    let minutes = |p: &Playing| (now - p.since).num_minutes().max(0);
    edit(
        bytes,
        |response| {
            if response.get("status").and_then(Value::as_str) != Some("ok") {
                return false;
            }
            let holder = response
                .entry("nowPlaying")
                .or_insert_with(|| Value::Object(Default::default()));
            let Some(holder) = holder.as_object_mut() else {
                return false;
            };
            let list = holder.entry("entry").or_insert_with(|| Value::Array(Vec::new()));
            // One entry written as an object rather than a list of one.
            if list.is_object() {
                *list = Value::Array(vec![list.take()]);
            }
            let Some(list) = list.as_array_mut() else {
                return false;
            };
            for (p, song) in &shown {
                let mut fields = builder.convert_song_to_json(song);
                fields.insert("username".into(), p.username.clone().into());
                fields.insert("minutesAgo".into(), minutes(p).into());
                fields.insert("playerId".into(), 0.into());
                fields.insert("playerName".into(), p.player.clone().into());
                list.push(Value::Object(fields));
            }
            true
        },
        |root| {
            if root.attribute("status") != Some("ok") {
                return false;
            }
            let ns = root.namespace.clone();
            if !root.elements().any(|e| e.name == "nowPlaying") {
                root.push(XElement::in_namespace(ns.as_deref(), "nowPlaying"));
            }
            let Some(holder) = root.elements_mut().find(|e| e.name == "nowPlaying") else {
                return false;
            };
            for (p, song) in &shown {
                let mut element = builder.convert_song_to_xml(song, ns.as_deref());
                element.name = "entry".into();
                element.set_attr("username", p.username.clone());
                element.set_attr("minutesAgo", minutes(p));
                element.set_attr("playerId", 0);
                element.set_attr("playerName", p.player.clone());
                holder.push(element);
            }
            true
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + seconds, 0).expect("a time")
    }

    #[test]
    fn an_outside_song_plays_until_the_player_moves_on() {
        let now = OutsideNowPlaying::default();
        now.started("Tom", "Octo for iPhone", "ext-yt-1", true, at(0));
        let list = now.current(at(30), |_| None);
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].username.as_str(), list[0].song_id.as_str()), ("Tom", "ext-yt-1"));
        // Another outside song replaces it; a library song clears it.
        now.started("tom", "Octo for iPhone", "ext-yt-2", true, at(40));
        assert_eq!(now.current(at(41), |_| None)[0].song_id, "ext-yt-2");
        now.started("tom", "Octo for iPhone", "lib-1", false, at(50));
        assert!(now.current(at(51), |_| None).is_empty());
    }

    #[test]
    fn players_and_listeners_are_apart() {
        let now = OutsideNowPlaying::default();
        now.started("tom", "iPhone", "ext-a", true, at(0));
        now.started("tom", "desktop", "ext-b", true, at(5));
        now.started("sam", "iPhone", "ext-c", true, at(10));
        now.started("tom", "desktop", "lib", false, at(20));
        let ids: Vec<String> = now.current(at(21), |_| None).into_iter().map(|p| p.song_id).collect();
        assert_eq!(ids, ["ext-a", "ext-c"]);
    }

    #[test]
    fn entries_expire_after_ten_minutes_or_the_song() {
        let now = OutsideNowPlaying::default();
        now.started("tom", "iPhone", "long", true, at(0));
        now.started("tom", "desktop", "short", true, at(0));
        let length = |id: &str| (id == "short").then_some(120);
        let ids = |t| -> Vec<String> { now.current(at(t), length).into_iter().map(|p| p.song_id).collect() };
        // Same start: by listener, then player.
        assert_eq!(ids(150), ["short", "long"]);
        assert_eq!(ids(181), ["long"]);
        assert_eq!(ids(599), ["long"]);
        assert!(ids(600).is_empty());
    }
}

#[cfg(test)]
mod app_tests {
    use octo_core::soulseek::soulseek_metadata_service::{RoutingKind, SoulseekRouting};
    use wiremock::matchers::path;
    use wiremock::{Mock, MockServer};

    use super::super::test_app::{NAVIDROME_OK, app, auth, get, navidrome_json, settings};
    use super::*;
    use crate::app::AppState;

    const EMPTY: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","type":"navidrome","nowPlaying":{}}}"#;
    const LIBRARY: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","type":"navidrome","nowPlaying":{"entry":[{"id":"lib-9","title":"Free Mind","artist":"Tems","username":"ana","minutesAgo":0,"playerId":1,"playerName":"web"}]}}}"#;

    async fn fixture(now_playing: &str) -> (AppState, MockServer, String) {
        let navidrome = MockServer::start().await;
        Mock::given(path("/rest/ping"))
            .respond_with(navidrome_json(NAVIDROME_OK))
            .mount(&navidrome)
            .await;
        Mock::given(path("/rest/scrobble"))
            .respond_with(navidrome_json(NAVIDROME_OK))
            .mount(&navidrome)
            .await;
        Mock::given(path("/rest/getNowPlaying"))
            .respond_with(navidrome_json(now_playing))
            .mount(&navidrome)
            .await;
        let state = AppState::for_tests(settings(&navidrome.uri()));
        let song = state.external_id_registry.register(SoulseekRouting {
            kind: RoutingKind::Song,
            artist: Some("Tems".into()),
            title: Some("Me & U".into()),
            album: Some("Born in the Wild".into()),
            duration: Some(190),
            ..Default::default()
        });
        (state, navidrome, song)
    }

    #[tokio::test]
    async fn an_outside_song_scrobbled_as_playing_joins_now_playing() {
        let (state, _navidrome, song) = fixture(LIBRARY).await;
        OUTSIDE_NOW_PLAYING.clear_user("np-tom");
        let app = app(&state);
        let played = get(
            &app,
            &format!("/rest/scrobble?id={song}&submission=false&{}&f=json", auth("np-tom", "good", "Octo%20for%20iPhone")),
        )
        .await;
        assert_eq!(played.envelope()["status"], "ok", "{}", played.text());

        let answer = get(&app, &format!("/rest/getNowPlaying?{}&f=json", auth("np-tom", "good", "hub"))).await;
        let entries = answer.envelope()["nowPlaying"]["entry"].as_array().cloned().expect("entries");
        let mine: Vec<&Value> = entries.iter().filter(|e| e["username"] == "np-tom").collect();
        assert_eq!(mine.len(), 1, "{}", answer.text());
        assert_eq!(mine[0]["id"], song.as_str());
        assert_eq!(mine[0]["title"], "Me & U");
        assert_eq!(mine[0]["artist"], "Tems");
        assert_eq!(mine[0]["playerName"], "Octo for iPhone");
        assert!(entries.iter().any(|e| e["id"] == "lib-9"), "Navidrome's own entry stays");

        // The play finishing does not clear it (a library song started on the same player does:
        // see an_outside_song_plays_until_the_player_moves_on).
        get(&app, &format!("/rest/scrobble?id={song}&submission=true&{}&f=json", auth("np-tom", "good", "Octo%20for%20iPhone"))).await;
        let answer = get(&app, &format!("/rest/getNowPlaying?{}&f=json", auth("np-tom", "good", "hub"))).await;
        assert!(answer.text().contains(&song));
    }

    #[tokio::test]
    async fn an_empty_now_playing_takes_the_entry() {
        let (state, _navidrome, song) = fixture(EMPTY).await;
        OUTSIDE_NOW_PLAYING.clear_user("np-xml");
        let app = app(&state);
        get(&app, &format!("/rest/scrobble?id={song}&submission=false&{}&f=json", auth("np-xml", "good", "Narjo"))).await;
        let answer = get(&app, &format!("/rest/getNowPlaying?{}&f=json", auth("np-xml", "good", "hub"))).await;
        let entry = &answer.envelope()["nowPlaying"]["entry"];
        assert!(entry.as_array().is_some_and(|e| e.iter().any(|e| e["username"] == "np-xml")), "{}", answer.text());
    }
}
