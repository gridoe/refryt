//! YouTube Music metadata, via `ytmapi-rs`.
//!
//! Search, artists and albums work without a login. Signing in with a browser cookie
//! (see `auth.rs`) adds *your* library; the client used is chosen per request so a session
//! swap touches only this file.
//!
//! This file is the only place `ytmapi-rs` types appear; everything it returns is mapped into
//! `model.rs`.

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::Mutex;
use ytmapi_rs::YtMusic;
use ytmapi_rs::auth::AuthToken;
use ytmapi_rs::auth::BrowserToken;
use ytmapi_rs::auth::noauth::NoAuthToken;
use ytmapi_rs::common::PlaylistID;
use ytmapi_rs::common::{
    AlbumID, AlbumType, ArtistChannelID, BrowseParams, Explicit, SongTrackingUrl, VideoID,
    YoutubeID,
};
use ytmapi_rs::parse::{
    GetAlbum, GetArtist, GetArtistAlbumsAlbum, MoodCategorySection, MoodPlaylistCategory,
    PlaylistItem, PlaylistSong, SearchResultSong,
};
use ytmapi_rs::query::AddHistoryItemQuery;
use ytmapi_rs::query::SearchQuery;
use ytmapi_rs::query::search::BasicSearch;
use ytmapi_rs::query::search::{AlbumsFilter, FilteredSearch, FilteredSearchType};
use ytmapi_rs::query::song::GetSongTrackingUrlQuery;
use ytmapi_rs::query::{GetArtistAlbumsQuery, GetArtistQuery};

use crate::auth;
use crate::innertube;
use crate::model::{self, Track, parse_duration};

/// Google's "consent already handled" cookie.
///
/// Without it, a request from an EU country is answered with `302 consent.youtube.com`, and the
/// bootstrap page that `ytmapi-rs` needs to read `ytcfg` from is never reached — the crate reports
/// an empty body and the app looks broken for European users only. `ytmapi-rs` builds its own
/// reqwest client and sets no cookies, so we supply one that does.
///
/// It also matters that this is a *default* header: the crate sets `User-Agent`, `X-Origin` and
/// `Content-Type` per request, and those would override anything we passed the same way.
const CONSENT_COOKIE: &str = "SOCS=CAI";

/// Build the HTTP client `ytmapi-rs` will use.
/// Build the HTTP client, optionally browsing as a brand account.
///
/// A Google account can own several YouTube channels. InnerTube selects one with the
/// `X-Goog-PageId` header — absent means the personal channel. `ytmapi-rs` sets its own headers
/// per request and offers no hook to add one, so the page id goes on the client as a default
/// header and switching account means rebuilding the client.
///
/// **`X-Goog-PageId` must travel with `X-Goog-AuthUser`.** On its own it does not select the
/// brand channel — it de-authenticates the request entirely: `account/account_menu` comes back
/// with no `accountName` and the library browse ids return their signed-out empty state, which
/// is indistinguishable from an expired cookie. Measured 2026-09-08 against this account:
///
/// | headers | liked songs | account |
/// |---|---|---|
/// | none | 26 rows | `liasosfp` |
/// | `PageId` alone | empty state | none |
/// | `PageId` + `AuthUser: 0` | 26 rows | `IV` |
///
/// The index is which signed-in Google account of the jar to use, and a cookie jar imported from
/// a browser holds exactly one, so it is always 0.
fn default_headers(page_id: Option<&str>) -> reqwest::header::HeaderMap {
    use reqwest::header::{COOKIE, HeaderMap, HeaderName, HeaderValue};

    let mut headers = HeaderMap::new();
    headers.insert(COOKIE, HeaderValue::from_static(CONSENT_COOKIE));
    if let Some(page_id) = page_id.filter(|p| !p.is_empty()) {
        match HeaderValue::from_str(page_id) {
            Ok(value) => {
                headers.insert(HeaderName::from_static("x-goog-pageid"), value);
                headers.insert(
                    HeaderName::from_static("x-goog-authuser"),
                    HeaderValue::from_static("0"),
                );
            }
            Err(_) => tracing::warn!("ignoring an unusable brand account id"),
        }
    }
    headers
}

fn http_client_for(page_id: Option<&str>) -> Result<ytmapi_rs::Client> {
    let headers = default_headers(page_id);

    let inner = reqwest::Client::builder()
        .use_rustls_tls()
        .gzip(true)
        .cookie_store(true)
        .default_headers(headers)
        .build()
        .context("could not build the HTTP client")?;

    Ok(ytmapi_rs::Client::new_from_reqwest_client(inner))
}

/// A handle to either the anonymous or the signed-in client. `YtMusic` is cheap to clone — the
/// underlying reqwest client is shared — so we clone a handle per request rather than juggling
/// lifetimes across awaits.
#[derive(Clone)]
enum ClientRef {
    NoAuth(YtMusic<NoAuthToken>),
    Browser(YtMusic<BrowserToken>),
}

pub struct Api {
    /// Behind a lock because switching brand account rebuilds it — the page id is a default
    /// header on the client, which is the only hook `ytmapi-rs` leaves us.
    http: Mutex<ytmapi_rs::Client>,
    noauth: Mutex<Option<YtMusic<NoAuthToken>>>,
    browser: Mutex<Option<YtMusic<BrowserToken>>>,
    session: Mutex<Option<auth::Session>>,
    /// The session's cookie as yt-dlp sees it: kept in step with `session` wherever that is
    /// set or cleared, so playback and downloads run as the signed-in account too.
    ytdlp_session: refryt_resolve::SessionCookie,
    /// Single-flights [`Self::refresh_from_source`]: several requests can independently notice
    /// a rotated cookie at once (`Self::library`'s four sections fetch concurrently), and without
    /// this each would launch its own `yt-dlp` cookie re-import in parallel.
    refresh: Mutex<()>,
    /// The cookie the last failed refresh was for. Every caller that meets a stale session asks
    /// for a refresh, and a failure changes nothing they could notice, so without this each one
    /// tried again — at launch that was one sign-in window (or Keychain prompt) per caller.
    refresh_failed: Mutex<Option<String>>,
}

impl Api {
    pub fn new() -> Result<Self> {
        let session = auth::load_session().ok().flatten();
        let page_id = crate::settings::load().account_page_id;
        let ytdlp_session = std::sync::Arc::new(std::sync::RwLock::new(
            session.as_ref().map(|s| s.cookie.clone()),
        ));
        Ok(Self {
            ytdlp_session,
            http: Mutex::new(http_client_for(page_id.as_deref())?),
            noauth: Mutex::new(None),
            browser: Mutex::new(None),
            session: Mutex::new(session),
            refresh: Mutex::new(()),
            refresh_failed: Mutex::new(None),
        })
    }

    /// The current HTTP client. Cloned per use: it wraps a `reqwest::Client`, so this is cheap
    /// and it means a rebuild mid-flight cannot be observed halfway.
    async fn http(&self) -> ytmapi_rs::Client {
        self.http.lock().await.clone()
    }

    async fn noauth(&self) -> Result<YtMusic<NoAuthToken>> {
        if let Some(yt) = self.noauth.lock().await.clone() {
            return Ok(yt);
        }
        let yt = ytmapi_rs::builder::YtMusicBuilder::new_with_client(self.http().await)
            .build()
            .await
            .context("could not reach YouTube Music")?;
        *self.noauth.lock().await = Some(yt.clone());
        Ok(yt)
    }

    /// Build the signed-in client from the stored cookie, so it is ready when needed. `Ok(false)`
    /// means there is no stored session to build one from — not signed in, not an error; `Err` is
    /// a real failure (an invalid cookie, or the client failing to build).
    async fn ensure_browser(&self) -> Result<bool> {
        if self.browser.lock().await.is_some() {
            return Ok(true);
        }
        let Some(cookie) = self.session.lock().await.as_ref().map(|s| s.cookie.clone()) else {
            return Ok(false);
        };
        let http = self.http().await;
        let token = match BrowserToken::from_str(&auth::for_token(&cookie), &http).await {
            Ok(token) => token,
            // Building the token is a request of its own, so with no connection it fails too —
            // and that is no reason to tell someone offline that their sign-in is gone.
            Err(e) if looks_offline(&e.to_string()) => {
                return Err(anyhow::Error::new(e).context("could not reach YouTube Music"));
            }
            Err(e) => {
                let why = match refused_page(&e, &cookie).await {
                    Some(seen) if seen != Refusal::Other => seen.advice(),
                    _ => "the saved cookie is invalid or expired; sign in again",
                };
                return Err(anyhow::Error::new(e).context(why));
            }
        };
        let yt = ytmapi_rs::builder::YtMusicBuilder::new_with_client(http)
            .with_auth_token(token)
            .build()
            .context("could not build the signed-in client")?;
        *self.browser.lock().await = Some(yt);
        Ok(true)
    }

    /// The client to run a request against: the signed-in cookie client if there is one, else
    /// anonymous.
    ///
    /// A single call to `ensure_browser` decides which — checking `is_signed_in` first and then
    /// separately building the browser client used to leave a window where a concurrent
    /// `forget_session`/sign-out landed in between, turning what should have been a silent
    /// fallback to anonymous into a spurious "not signed in" error.
    async fn client_ref(&self) -> Result<ClientRef> {
        if self.ensure_browser().await? {
            let yt = self
                .browser
                .lock()
                .await
                .clone()
                .context("signed-in client not built")?;
            Ok(ClientRef::Browser(yt))
        } else {
            Ok(ClientRef::NoAuth(self.noauth().await?))
        }
    }

    pub async fn is_signed_in(&self) -> bool {
        self.session.lock().await.is_some()
    }

    /// Synchronous check, for the initial auth UI state.
    pub fn is_signed_in_sync(&self) -> bool {
        self.session
            .try_lock()
            .map(|g| g.is_some())
            .unwrap_or(false)
    }

    // ------------------------------------------------------------------------------------------
    // Device-code sign-in
    // ------------------------------------------------------------------------------------------

    /// Import cookies from a browser you are already signed into, and use them.
    ///
    /// The primary sign-in path: it is the only one that can obtain the `HttpOnly` cookies a
    /// session actually needs. Returns the account name YouTube reports back.
    pub async fn sign_in_from_browser(&self, browser: &str) -> Result<String> {
        let cookie = crate::browser_cookies::import(browser).await?;
        self.install_cookie(cookie, Some(browser)).await
    }

    /// Sign in through the app's own browser window (Windows' WebView2, Linux's WebKitGTK).
    ///
    /// The browser import's counterpart for the case that path cannot serve: on Windows,
    /// Chromium's App-Bound Encryption makes a modern Chrome/Edge/Brave store unreadable to
    /// anything but the browser itself, so this window reads its own cookies and hands them over.
    /// On Linux it is simply the path that needs no browser already signed in. The window lives
    /// in a child process of this same binary — see `webview_login`'s module doc — so this only
    /// awaits it.
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    pub async fn sign_in_from_window(&self) -> Result<String> {
        let cookie = crate::webview_login::capture().await?;
        self.install_cookie(cookie, Some(crate::webview_login::SOURCE))
            .await
    }

    /// Renew a window session in the background — [`Self::sign_in_from_window`] without the
    /// window. `install_cookie` still checks the result with YouTube, so a refresh that only
    /// found the same stale cookie fails rather than being saved.
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    async fn refresh_from_window(&self) -> Result<String> {
        let cookie = crate::webview_login::refresh().await?;
        self.install_cookie(cookie, Some(crate::webview_login::SOURCE))
            .await
    }

    /// Install a cookie, verify with YouTube that it is really a session, and persist it.
    ///
    /// Three gates, because the first two are each insufficient on their own:
    ///
    /// 1. [`auth::validate_cookie`] rejects a cookie missing the names that authenticate.
    /// 2. `BrowserToken::from_str` — what `ytmapi-rs` needs. It only looks for `SAPISID`, so it
    ///    accepts cookies that never work; it is a necessary step, not a check.
    /// 3. `account/account_menu` must name an account. This is the only one that distinguishes
    ///    *signed in with an empty library* from *not signed in*, since both render as an empty
    ///    shelf, and it is what stops the app claiming a session it does not have.
    pub async fn install_cookie(&self, cookie: String, source: Option<&str>) -> Result<String> {
        let cookie = auth::extract_cookie(&cookie);
        auth::validate_cookie(&cookie)?;

        let http = self.http().await;
        let token = match BrowserToken::from_str(&auth::for_token(&cookie), &http).await {
            Ok(token) => token,
            Err(e) => {
                let seen = refused_page(&e, &cookie).await;
                let why = match seen {
                    Some(seen) if seen != Refusal::Other => seen.advice(),
                    // Captured, not pasted: advice about pasting would send someone the wrong way.
                    _ if source.is_some() => {
                        "YouTube Music did not accept this session; it may be signed out or \
                         need a check in the browser. Sign in again."
                    }
                    _ => {
                        "YouTube rejected the cookie; paste only the cookie value, on a single \
                         line. It may also have expired since you copied it."
                    }
                };
                return Err(anyhow::Error::new(e).context(why));
            }
        };
        let yt = ytmapi_rs::builder::YtMusicBuilder::new_with_client(http)
            .with_auth_token(token)
            .build()
            .context("could not build the signed-in client")?;

        let account = account_name(&yt).await?.context(
            "YouTube did not recognise that session. If you pasted it, copy the cookie header \
             from the Network tab rather than document.cookie from the Console, which leaves \
             out the parts that sign you in.",
        )?;

        auth::save_session(&cookie, source)?;
        *self.refresh_failed.lock().await = None;
        self.share_with_ytdlp(Some(&cookie));
        *self.session.lock().await = Some(auth::Session {
            cookie,
            source: source.map(str::to_owned),
        });
        *self.browser.lock().await = Some(yt);
        Ok(account)
    }

    /// The channels this Google account can browse as.
    ///
    /// A Google account may own brand channels alongside the personal one. The personal entry has
    /// no page id; the brand ones do, and that id is what selects them.
    pub async fn accounts(&self) -> Result<Vec<model::Account>> {
        if !self.ensure_browser().await? {
            return Ok(Vec::new());
        }
        let yt = self
            .browser
            .lock()
            .await
            .clone()
            .context("signed-in client not built")?;
        let json = yt
            .json_query(AccountsListQuery)
            .await
            .map(ytmapi_rs::json::Json::into_inner)
            .context("could not list the account's channels")?;
        dump("REFRYT_DUMP_ACCOUNTS", &json);
        Ok(innertube::parse_accounts(&json))
    }

    /// Browse as a different channel of the same Google account.
    ///
    /// Rebuilds the client with (or without) the page-id header and drops the signed-in client so
    /// it is rebuilt on the next request; everything cached for the previous account is stale.
    pub async fn switch_account(&self, page_id: Option<String>) -> Result<()> {
        *self.http.lock().await = http_client_for(page_id.as_deref())?;
        // Both derived clients carry the old header; drop them so they rebuild from the new one.
        *self.noauth.lock().await = None;
        *self.browser.lock().await = None;
        Ok(())
    }

    /// Stop using the stored credential, but keep it on disk.
    ///
    /// Used when YouTube answers a request anonymously. That is *not* proof the credential is
    /// worthless — a rotated cookie whose re-import happened to fail (a locked keyring, a browser
    /// holding its database) looks exactly the same — and deleting it there destroyed a working
    /// session that a later retry would have refreshed. Only [`Self::sign_out`] deletes.
    pub async fn forget_session(&self) {
        self.share_with_ytdlp(None);
        *self.session.lock().await = None;
        *self.browser.lock().await = None;
    }

    /// Sign out for real: forget the credential and remove it from disk.
    pub async fn sign_out(&self) {
        self.forget_session().await;
        let _ = auth::delete_cookie();
        let _ = auth::delete_oauth();
        // The sign-in window keeps Google's own session in its profile, and that is a credential
        // too: left behind, the next Log in would silently re-capture the same account instead
        // of asking who is signing in. Both platforms: the window runs on Linux too (WebKitGTK),
        // and gating this on Windows alone is why Log in right after Sign out there signed the
        // same account straight back in.
        #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
        crate::webview_login::forget();
    }

    /// Confirm the stored session with YouTube, refreshing it from the browser if it has gone
    /// stale.
    ///
    /// `Ok(Some(name))` is confirmed, `Ok(None)` is confirmed anonymous — only then should the
    /// stored session be discarded. `Err` means the check could not be made, and the session must
    /// be kept: a stored sign-in should survive being offline.
    pub async fn verify_session(&self) -> Result<Option<String>> {
        if !self.ensure_browser().await? {
            return Ok(None);
        }
        let yt = self
            .browser
            .lock()
            .await
            .clone()
            .context("signed-in client not built")?;

        if let Some(account) = account_name(&yt).await? {
            return Ok(Some(account));
        }
        // Recognised as anonymous: the cookies have rotated under us. Re-import once.
        if !self.refresh_from_source().await {
            return Ok(None);
        }
        let yt = self
            .browser
            .lock()
            .await
            .clone()
            .context("signed-in client not built")?;
        account_name(&yt).await
    }

    /// Re-import from wherever this session came from, once, when YouTube stops recognising it.
    ///
    /// Google rotates `__Secure-1PSIDTS`, so a stored cookie goes stale on its own. Returns false
    /// when there is nothing to refresh from — a pasted session, or no stored session at all.
    async fn refresh_from_source(&self) -> bool {
        let before = self.session.lock().await.as_ref().map(|s| s.cookie.clone());
        let _guard = self.refresh.lock().await;
        // Another caller may have refreshed while this one waited for the lock above:
        // `install_cookie` replaces the stored cookie on success, so a cookie that changed
        // while waiting is what says the herd's first request already did this work. A built
        // client merely existing is not that signal — one was already built from the stale
        // cookie, which is why this refresh was called at all.
        let after = self.session.lock().await.as_ref().map(|s| s.cookie.clone());
        if after != before {
            return after.is_some();
        }
        // This very cookie has already failed to refresh: trying again would only repeat it.
        if after.is_some() && *self.refresh_failed.lock().await == after {
            return false;
        }
        let source = self
            .session
            .lock()
            .await
            .as_ref()
            .and_then(|s| s.source.clone());
        let Some(source) = source else {
            return false;
        };
        let refreshed = match source.as_str() {
            // Not a browser `--cookies-from-browser` knows about: the sign-in window, run hidden,
            // finds Google's session still in its profile. It never shows a login page here —
            // a refresh is not the user asking to sign in.
            #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
            crate::webview_login::SOURCE => self.refresh_from_window().await,
            _ => self.sign_in_from_browser(&source).await,
        };
        match refreshed {
            Ok(_) => {
                tracing::info!(%source, "refreshed the session from its source");
                true
            }
            Err(e) => {
                tracing::warn!(%source, error = %format!("{e:#}"), "could not refresh the session");
                *self.refresh_failed.lock().await = after;
                false
            }
        }
    }

    /// Search, as one request, in YouTube's own relevance order.
    ///
    /// We ask `ytmapi-rs` for the raw JSON and parse it in [`crate::innertube`], because its typed
    /// parser cannot read the shape YouTube now returns: an unfiltered search comes back as a flat
    /// `itemSectionRenderer` list rather than `musicShelfRenderer` shelves, and the whole
    /// ecosystem — `ytmusicapi` included — reads that as "no results". Parsing it ourselves is
    /// what restores the cross-type ranking that makes `travis` mean Travis Scott.
    ///
    /// If that parse comes back empty, we fall back to the three typed filtered queries this
    /// replaced, so the worst case is the behaviour that shipped before.
    pub async fn search(&self, query: &str) -> Result<model::SearchResults> {
        let client = self.client_ref().await?;
        let q = query.to_owned();
        let json = match client {
            ClientRef::NoAuth(yt) => run_search_json(&yt, &q).await,
            ClientRef::Browser(yt) => run_search_json(&yt, &q).await,
        };

        match json {
            Ok(json) => {
                // Diagnostic hatch: `REFRYT_DUMP_SEARCH=<path>` writes the raw response, which is how
                // a parsing disagreement gets settled with evidence instead of guesswork.
                if let Some(path) = std::env::var_os("REFRYT_DUMP_SEARCH") {
                    let _ =
                        std::fs::write(path, serde_json::to_vec_pretty(&json).unwrap_or_default());
                }
                let results = innertube::parse_search(&json);
                // An empty answer YouTube gave in words is the answer. The filtered queries
                // would go and find something anyway: for gibberish, songs from the account's
                // own history.
                if !results.is_empty() || results.no_results {
                    return Ok(results);
                }
                tracing::warn!(%query, "ranked search parsed to nothing; using filtered queries");
            }
            Err(e) => {
                tracing::warn!(%query, error = %format!("{e:#}"), "raw search failed; using filtered queries");
            }
        }
        self.search_filtered(query).await
    }

    /// Autocomplete suggestions for the search field. Works signed out. `ytmapi-rs` already wraps
    /// `music/get_search_suggestions`; this just flattens its formatted runs back to plain text.
    pub async fn suggestions(&self, query: &str) -> Result<Vec<String>> {
        let client = self.client_ref().await?;
        let items = match client {
            ClientRef::NoAuth(yt) => yt.get_search_suggestions(query).await,
            ClientRef::Browser(yt) => yt.get_search_suggestions(query).await,
        }
        .with_context(|| format!("could not fetch suggestions for {query}"))?;
        Ok(items
            .into_iter()
            .map(|s| {
                s.runs
                    .into_iter()
                    .map(ytmapi_rs::common::TextRun::take_text)
                    .collect::<String>()
            })
            .filter(|s| !s.is_empty())
            .collect())
    }

    /// A dedicated, category-specific search — never called for [`model::SearchScope::All`],
    /// which stays the one cross-type ranked request `search` already makes (see that type's
    /// own doc comment for why the two are not the same request with the results split up
    /// afterward: the ranked response was found to carry far too few of a given kind for that,
    /// while a real per-category request routinely returns a dozen or more).
    pub async fn search_scoped(
        &self,
        query: &str,
        scope: model::SearchScope,
    ) -> Result<Vec<model::Entry>> {
        let client = self.client_ref().await?;
        match scope {
            model::SearchScope::All => Ok(Vec::new()),
            model::SearchScope::Songs => {
                let songs = match &client {
                    ClientRef::NoAuth(yt) => yt.search_songs(query).await,
                    ClientRef::Browser(yt) => yt.search_songs(query).await,
                }
                .with_context(|| format!("song search failed for {query:?}"))?;
                Ok(songs
                    .into_iter()
                    .map(|s| model::Entry::Song(map_song(s)))
                    .collect())
            }
            model::SearchScope::Videos => {
                let videos = match &client {
                    ClientRef::NoAuth(yt) => yt.search_videos(query).await,
                    ClientRef::Browser(yt) => yt.search_videos(query).await,
                }
                .with_context(|| format!("video search failed for {query:?}"))?;
                Ok(videos
                    .into_iter()
                    .filter_map(map_video_ref)
                    .map(model::Entry::Video)
                    .collect())
            }
            model::SearchScope::Artists => {
                let artists = match &client {
                    ClientRef::NoAuth(yt) => yt.search_artists(query).await,
                    ClientRef::Browser(yt) => yt.search_artists(query).await,
                }
                .with_context(|| format!("artist search failed for {query:?}"))?;
                Ok(artists
                    .into_iter()
                    .map(|a| model::Entry::Artist(map_artist_ref(a)))
                    .collect())
            }
            model::SearchScope::Albums => {
                let albums = match &client {
                    ClientRef::NoAuth(yt) => yt.search_albums(query).await,
                    ClientRef::Browser(yt) => yt.search_albums(query).await,
                }
                .with_context(|| format!("album search failed for {query:?}"));
                match albums {
                    Ok(albums) => Ok(albums
                        .into_iter()
                        .map(|a| model::Entry::Album(map_album_ref(a)))
                        .collect()),
                    // `ytmapi-rs` wants every row to read "Album • Artist • Year", and one row
                    // that does not fails the whole list. Our own row parser takes what it can
                    // read and skips the rest.
                    Err(e) => {
                        tracing::warn!(error = %format!("{e:#}"), "album search: reading the raw reply instead");
                        let json = match &client {
                            ClientRef::NoAuth(yt) => {
                                run_filtered_json::<_, AlbumsFilter>(yt, query).await
                            }
                            ClientRef::Browser(yt) => {
                                run_filtered_json::<_, AlbumsFilter>(yt, query).await
                            }
                        }?;
                        Ok(innertube::parse_search(&json)
                            .items
                            .into_iter()
                            .filter(|e| matches!(e, model::Entry::Album(_)))
                            .collect())
                    }
                }
            }
            model::SearchScope::Playlists => {
                let playlists = match &client {
                    ClientRef::NoAuth(yt) => yt.search_playlists(query).await,
                    ClientRef::Browser(yt) => yt.search_playlists(query).await,
                }
                .with_context(|| format!("playlist search failed for {query:?}"))?;
                Ok(playlists
                    .into_iter()
                    .filter_map(map_playlist_ref)
                    .map(model::Entry::Playlist)
                    .collect())
            }
        }
    }

    /// The pre-M5 path: three filtered queries, whose shelves `ytmapi-rs` still parses. Kept as
    /// the fallback for the day the unfiltered response changes shape again.
    async fn search_filtered(&self, query: &str) -> Result<model::SearchResults> {
        let client = self.client_ref().await?;
        let q = query.to_owned();
        let (songs, artists, albums) = match client {
            ClientRef::NoAuth(yt) => {
                tokio::join!(
                    yt.search_songs(q.as_str()),
                    yt.search_artists(q.as_str()),
                    yt.search_albums(q.as_str()),
                )
            }
            ClientRef::Browser(yt) => {
                tokio::join!(
                    yt.search_songs(q.as_str()),
                    yt.search_artists(q.as_str()),
                    yt.search_albums(q.as_str()),
                )
            }
        };
        // One kind failing to parse costs that kind, not the search: `ytmapi-rs` rejects a whole
        // album list over one row it cannot read. Only all three failing is an error.
        if let (Err(e), Err(_), Err(_)) = (&songs, &artists, &albums) {
            return Err(anyhow::anyhow!("{e}"))
                .with_context(|| format!("search failed for {query:?}"));
        }
        fn or_empty<T>(kind: &str, r: ytmapi_rs::Result<Vec<T>>) -> Vec<T> {
            r.unwrap_or_else(|e| {
                tracing::warn!(kind, error = %e, "filtered search: one kind failed; showing the rest");
                Vec::new()
            })
        }
        // Interleaved rather than concatenated: without YouTube's ranking, leading with twenty
        // songs would bury the artist you were probably looking for.
        let songs: Vec<_> = or_empty("songs", songs)
            .into_iter()
            .map(|s| model::Entry::Song(map_song(s)))
            .collect();
        let artists: Vec<_> = or_empty("artists", artists)
            .into_iter()
            .map(|a| model::Entry::Artist(map_artist_ref(a)))
            .collect();
        let albums: Vec<_> = or_empty("albums", albums)
            .into_iter()
            .map(|a| model::Entry::Album(map_album_ref(a)))
            .collect();

        let mut items = Vec::with_capacity(songs.len() + artists.len() + albums.len());
        items.extend(artists.into_iter().take(1));
        items.extend(albums.into_iter().take(1));
        let mut rest: Vec<model::Entry> = songs;
        items.append(&mut rest);
        Ok(model::SearchResults {
            items,
            ..Default::default()
        })
    }

    /// An artist page, parsed by us so the per-song covers survive.
    ///
    /// `ytmapi_rs::parse::ArtistSong` has no `thumbnails` field, so the typed path throws away the
    /// cover YouTube sends for every top song and the page fell back to the artist's photograph on
    /// every row. Falls back to the typed query if our parse cannot find a name.
    pub async fn get_artist(&self, id: &str) -> Result<model::Artist> {
        let client = self.client_ref().await?;
        let aid = id.to_owned();
        let json = match &client {
            ClientRef::NoAuth(yt) => run_artist_json(yt, &aid).await,
            ClientRef::Browser(yt) => run_artist_json(yt, &aid).await,
        };
        match json {
            Ok(json) => {
                dump("REFRYT_DUMP_ARTIST", &json);
                if let Some(artist) = innertube::parse_artist(&json) {
                    return Ok(artist);
                }
                tracing::warn!(artist = %id, "artist page parsed to nothing; using the typed query");
            }
            Err(e) => {
                tracing::warn!(artist = %id, error = %format!("{e:#}"), "raw artist fetch failed");
            }
        }
        let artist = match client {
            ClientRef::NoAuth(yt) => run_artist(&yt, id).await?,
            ClientRef::Browser(yt) => run_artist(&yt, id).await?,
        };
        Ok(map_artist(artist))
    }

    pub async fn get_album(&self, id: &str) -> Result<model::Album> {
        // The typed query parses the tracks, but its library state comes from the toggle's
        // default icon — which does not flip when the album is saved (verified live: a saved
        // album still renders `BOOKMARK_BORDER`). The raw browse of the same id reads
        // `isToggled` instead. Concurrent, so the truth costs no latency; a raw failure
        // keeps the typed state rather than losing the page.
        let raw = BrowseQuery::feed_owned(id.to_owned());
        let (album, header) = tokio::join!(
            async {
                match self.client_ref().await? {
                    ClientRef::NoAuth(yt) => run_album(&yt, id).await,
                    ClientRef::Browser(yt) => run_album(&yt, id).await,
                }
            },
            self.run_browse_owned(raw, id),
        );
        let mut out = map_album(id, album?);
        match header {
            Ok(json) => {
                // A raw album browse renders the responsive header; the scan stays inside
                // it, because the track rows below carry their own bookmark menu items.
                if let Some(toggle) =
                    innertube::find_header_toggle(&json, "musicResponsiveHeaderRenderer")
                {
                    out.in_library = toggle.in_library;
                }
                // And those rows are the only place the tracks' own ratings live: the typed
                // parser has no field for a like button, so every album track used to arrive
                // unrated and a song you had liked did not look liked once it was playing.
                let states = innertube::track_states(&json);
                merge_track_states(&mut out.tracks, &states);
                // The rows' own credits, where a track's differ from the album's.
                for track in &mut out.tracks {
                    if let Some(state) = states.get(&track.video_id)
                        && !state.artists.is_empty()
                    {
                        track.artists = state.artists.clone();
                    }
                }
            }
            Err(e) => tracing::debug!(%id, error = %format!("{e:#}"),
                "raw album browse failed; keeping the typed library state"),
        }
        Ok(out)
    }

    /// The signed-in user's library, in four sections fetched concurrently.
    ///
    /// Parsed here rather than through `ytmapi-rs` because the sections do not share a shape:
    /// liked songs and followed artists come back as `musicShelfRenderer` lists, while albums and
    /// playlists come back as **`gridRenderer`** — which its parser does not read. An empty
    /// section is an empty list, not an error: a signed-out or genuinely empty library renders a
    /// `messageRenderer`, and surfacing that as a missing-key error is what put a JSON path in
    /// front of the user.
    pub async fn library(&self, sort: LibrarySortParam) -> Result<model::Library> {
        if !self.is_signed_in().await {
            anyhow::bail!("not signed in");
        }
        // Only songs take a sort order; the other sections have none to give.
        let (songs, artists, albums, playlists) = tokio::join!(
            self.library_page(LIBRARY_SONGS, sort.params(), None),
            self.library_page(LIBRARY_ARTISTS, None, None),
            self.library_page(LIBRARY_ALBUMS, None, None),
            self.library_page(LIBRARY_PLAYLISTS, None, None),
        );

        Ok(model::Library {
            songs: page_of(songs?, |e| e.track().cloned()),
            artists: page_of(artists?, |e| match e {
                model::Entry::Artist(a) => Some(a),
                _ => None,
            }),
            albums: page_of(albums?, |e| match e {
                model::Entry::Album(a) => Some(a),
                _ => None,
            }),
            playlists: page_of(playlists?, |e| match e {
                model::Entry::Playlist(p) => Some(p),
                _ => None,
            }),
        })
    }

    /// The first page of your playlists, for the sidebar.
    ///
    /// The rail lists your playlists and must not wait for the Library page to be opened once —
    /// but it needs one of the four sections, not all four. Fetching the whole library on
    /// sign-in put 20 MB on the idle memory figure for three sections nothing was displaying.
    pub async fn library_playlists(&self) -> Result<innertube::Page> {
        if !self.is_signed_in().await {
            anyhow::bail!("not signed in");
        }
        // The sidebar wants the whole collection, not just page one — a playlist sorting past
        // the first page used to simply never show up there, even though the full Library page
        // (which pages on demand via `library_more`) had it all along.
        let mut page = self.library_page(LIBRARY_PLAYLISTS, None, None).await?;
        let mut token = page.next.take();
        for _ in 0..SIDEBAR_PLAYLIST_PAGES {
            let Some(next) = token.take() else { break };
            let mut more = self
                .library_page(LIBRARY_PLAYLISTS, None, Some(next))
                .await?;
            if more.items.is_empty() {
                break;
            }
            page.items.append(&mut more.items);
            token = more.next;
        }
        Ok(page)
    }

    /// The next page of one library section.
    pub async fn library_more(
        &self,
        section: LibrarySection,
        token: String,
    ) -> Result<innertube::Page> {
        self.library_page(section.browse_id(), None, Some(token))
            .await
    }

    /// One page of one library browse id, with a single re-import if the session has gone stale.
    async fn library_page(
        &self,
        browse_id: &'static str,
        sort: Option<String>,
        continuation: Option<String>,
    ) -> Result<innertube::Page> {
        let query = match &continuation {
            Some(token) => BrowseQuery::next_page(token.clone()),
            None => BrowseQuery::sorted(browse_id, sort.clone()),
        };
        let json = self.run_browse(query, browse_id).await?;
        let page = innertube::parse_library_page(&json);
        if !page.items.is_empty() || !innertube::looks_signed_out(&json) {
            return Ok(page);
        }
        // Empty *and* carrying YouTube's signed-out prompt: the cookie has expired under us.
        if self.refresh_from_source().await {
            let query = match &continuation {
                Some(token) => BrowseQuery::next_page(token.clone()),
                None => BrowseQuery::sorted(browse_id, sort),
            };
            let json = self.run_browse(query, browse_id).await?;
            return Ok(innertube::parse_library_page(&json));
        }
        Ok(page)
    }

    /// A full playlist: details plus tracks, concurrently. Public playlists work signed out.
    /// A full playlist: header and the first page of tracks, in one request.
    ///
    /// Parsed here rather than through `ytmapi-rs`, which returns only part of a long playlist and
    /// cannot ask for the rest: for a 5,042-track "Liked music" it hands back 46, which is what
    /// made a playlist page look like it stopped scrolling. The raw response gives 100 rows a page
    /// with a continuation. Falls back to the typed queries if our parse finds no header.
    pub async fn playlist(&self, id: &str) -> Result<model::Playlist> {
        let browse_id = browse_id_for_playlist(id);
        let mut states = std::collections::HashMap::new();
        match self
            .run_browse_owned(BrowseQuery::feed_owned(browse_id.clone()), id)
            .await
        {
            Ok(json) => {
                if let Some(p) = innertube::parse_playlist(&json) {
                    return Ok(Self::playlist_from_page(id, p));
                }
                // Two shapes mean the request was answered as if nobody were signed in: an
                // empty shell — `contents: {}` — for a private playlist, and YouTube's
                // sign-in prompt page for a personal feed like VLLM. When we believe we *have*
                // a session, the cookie most likely rotated under us: `verify_session`
                // re-imports from the browser once, and a renewed session gets one retry
                // before anything surfaces.
                if Self::no_session_page(&json)
                    && self.is_signed_in().await
                    && self.verify_session().await.ok().flatten().is_some()
                {
                    let retry = self
                        .run_browse_owned(BrowseQuery::feed_owned(browse_id), id)
                        .await?;
                    if let Some(p) = innertube::parse_playlist(&retry) {
                        return Ok(Self::playlist_from_page(id, p));
                    }
                }
                if Self::no_session_page(&json) {
                    // The typed queries cannot parse either shape — their two-column key
                    // error is noise. Say what it is instead.
                    anyhow::bail!(
                        "playlist {id} came back empty: private or removed, or the session expired"
                    );
                }
                tracing::warn!(%id, "playlist parsed to nothing; using the typed queries");
                // The header is what failed, not the rows: whatever ratings and bookmark
                // tokens the raw response did carry are still worth keeping, because the
                // typed parser has no field for either and would hand back a playlist where
                // nothing you have liked looks liked.
                states = innertube::track_states(&json);
            }
            Err(e) => tracing::warn!(%id, error = %format!("{e:#}"), "raw playlist fetch failed"),
        }
        self.playlist_typed(id, &states).await
    }

    /// Whether a browse answered with an empty shell — `contents: {}` or nothing at all.
    /// A private item fetched without a recognised session looks exactly like this.
    fn contents_empty(json: &serde_json::Value) -> bool {
        json.pointer("/contents")
            .is_none_or(|c| c.as_object().is_some_and(|m| m.is_empty()))
    }

    /// Whether a browse answered as if the request were anonymous: the empty shell above, or
    /// YouTube's sign-in prompt page. Personal feeds answer with the latter signed out, and it
    /// is not an empty shell, so the two have to be tested together.
    fn no_session_page(json: &serde_json::Value) -> bool {
        Self::contents_empty(json) || innertube::sign_in_prompt(json)
    }

    /// A raw playlist page into the model: subtitle fields split the way YouTube writes them —
    /// "Playlist • Private • 2025" and "56 tracks • 2 hours, 52 minutes".
    fn playlist_from_page(id: &str, p: innertube::PlaylistPage) -> model::Playlist {
        // "Playlist • Private • 2025" and "56 tracks • 2 hours, 52 minutes" — but the second
        // line's fields are ordered by YouTube, not by kind: a public radio can read
        // "24 views • 8 tracks • 7 days". Classify by what each field says.
        let second: Vec<&str> = p
            .second_subtitle
            .split('•')
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .collect();
        let says = |f: &&str, needles: &[&str]| {
            let lower = f.to_lowercase();
            needles.iter().any(|n| lower.contains(n))
        };
        let count = second
            .iter()
            .find(|f| says(f, &["track", "song"]))
            .map(|f| (*f).to_owned());
        let duration = second
            .iter()
            .find(|f| says(f, &["minute", "hour", "second"]))
            .map(|f| (*f).to_owned());
        let year = p
            .subtitle
            .split('•')
            .map(str::trim)
            .find(|f| f.len() == 4 && f.chars().all(|c| c.is_ascii_digit()))
            .map(str::to_owned);
        model::Playlist {
            id: id.to_owned(),
            title: p.title,
            description: p.description,
            // The owner is shown in the header's facepile rather than as text; the raw parse
            // reads the name from it, and the typed fallback fills it when the raw path falls
            // through.
            author: p.author.unwrap_or_default(),
            author_id: p.author_id,
            year,
            track_count: count,
            duration_text: duration,
            thumbnail: p.thumbnail,
            editable: p.editable,
            in_library: p.library.as_ref().is_some_and(|t| t.in_library),
            library_add: p.library.as_ref().and_then(|t| t.add.clone()),
            library_remove: p.library.as_ref().and_then(|t| t.remove.clone()),
            saveable: p.library.is_some(),
            tracks: model::Paged::new(
                p.page
                    .items
                    .into_iter()
                    .filter_map(|e| e.track().cloned())
                    .collect(),
                p.page.next,
            ),
            source: model::PlaylistSource::YouTube,
            extras: p.extras,
        }
    }

    /// The Suggestions below one of your playlists, from its [`model::Playlist::extras`] token or
    /// the shelf's own Refresh token. `None` when the response held no such shelf.
    pub async fn playlist_suggestions(
        &self,
        token: String,
    ) -> Result<Option<innertube::Suggestions>> {
        let json = self
            .run_browse_owned(BrowseQuery::next_page(token), "playlist suggestions")
            .await?;
        Ok(innertube::parse_playlist_suggestions(&json))
    }

    /// The next page of a playlist's tracks.
    pub async fn playlist_more(&self, token: String) -> Result<innertube::Page> {
        let json = self
            .run_browse_owned(BrowseQuery::next_page(token), "playlist continuation")
            .await?;
        Ok(innertube::parse_playlist_tracks(&json))
    }

    /// The pre-existing path, kept as the fallback.
    ///
    /// `states` is whatever the raw browse managed to read before its header parse failed;
    /// it is empty when the request itself failed.
    async fn playlist_typed(
        &self,
        id: &str,
        states: &std::collections::HashMap<String, innertube::TrackState>,
    ) -> Result<model::Playlist> {
        let client = self.client_ref().await?;
        let pid = id.to_owned();
        let (details, tracks) = match client {
            ClientRef::NoAuth(yt) => {
                tokio::join!(
                    yt.get_playlist_details(PlaylistID::from_raw(pid.clone())),
                    yt.get_playlist_tracks(PlaylistID::from_raw(pid.clone())),
                )
            }
            ClientRef::Browser(yt) => {
                tokio::join!(
                    yt.get_playlist_details(PlaylistID::from_raw(pid.clone())),
                    yt.get_playlist_tracks(PlaylistID::from_raw(pid.clone())),
                )
            }
        };
        let d = details.with_context(|| format!("could not load playlist {id}"))?;
        let mut tracks: Vec<Track> = tracks
            .with_context(|| format!("could not load tracks for playlist {id}"))?
            .into_iter()
            .filter_map(|item| match item {
                PlaylistItem::Song(s) => Some(map_playlist_song(s)),
                _ => None,
            })
            .collect();
        merge_track_states(&mut tracks, states);
        Ok(model::Playlist {
            id: id.to_owned(),
            title: d.title,
            description: d.description,
            author: d.author,
            // The typed response carries no owner link.
            author_id: None,
            year: (!d.year.is_empty()).then_some(d.year),
            duration_text: (!d.duration.is_empty()).then_some(d.duration),
            track_count: (!d.track_count_text.is_empty()).then_some(d.track_count_text),
            thumbnail: last_thumb(&d.thumbnails),
            // The typed response cannot tell owned from subscribed; false hides "Remove
            // from playlist" rather than risking a write to someone else's list. Same
            // conservatism for the header save control.
            editable: false,
            in_library: false,
            library_add: None,
            library_remove: None,
            saveable: false,
            tracks: model::Paged::new(tracks, None),
            source: model::PlaylistSource::YouTube,
            extras: None,
        })
    }

    /// Expand one of an artist's shelf previews into the full list, for "Show all". `browse_id`
    /// and `params` are a matched pair from that shelf's own "more" button (`Shelf.more_browse_id`
    /// / `Shelf.more_params`) — passing the artist's channel id here when the shelf carried its
    /// own different one is what used to make this return an unrelated, unfiltered page.
    ///
    /// `wanted_title` is the shelf's own heading. The response is frequently more than the
    /// shelf — the captured "From your library" expansion was the *whole artist page* — so its
    /// titled sections are searched for that heading and only its items returned when one
    /// matches. A response with no matching heading falls back to the flat, tolerant parse, as
    /// before.
    pub async fn artist_albums(
        &self,
        browse_id: &str,
        params: &str,
        wanted_title: &str,
    ) -> Result<Vec<model::Entry>> {
        let (aid, p) = (browse_id.to_owned(), params.to_owned());
        // Raw first: the typed parser assumes one fixed grid pointer and errors on any other
        // shape, which is what made "Show all" fail while the entries were right there in the
        // same document.
        let raw = match self.client_ref().await? {
            ClientRef::NoAuth(yt) => run_artist_albums_json(&yt, &aid, &p).await,
            ClientRef::Browser(yt) => run_artist_albums_json(&yt, &aid, &p).await,
        };
        match &raw {
            Ok(json) => {
                dump("REFRYT_DUMP_ARTIST_ALBUMS", json);
                let mut sections = innertube::parse_artist_sections(json);
                let asked = wanted_title.trim();
                if let Some(i) = sections.iter().position(|s| {
                    s.title
                        .as_deref()
                        .is_some_and(|t| t.trim().eq_ignore_ascii_case(asked))
                }) {
                    return Ok(sections.swap_remove(i).items);
                }
                let items: Vec<model::Entry> = sections.drain(..).flat_map(|s| s.items).collect();
                if !items.is_empty() {
                    return Ok(items);
                }
            }
            Err(e) => tracing::warn!(
                %browse_id,
                error = %format!("{e:#}"),
                "raw artist albums fetch failed",
            ),
        }
        // Nothing usable in the raw read: the typed query is never worse than the old path, and
        // for a response the tolerant parser cannot read it is the only answer left.
        let albums = match self.client_ref().await? {
            ClientRef::NoAuth(yt) => run_artist_albums(&yt, &aid, &p).await?,
            ClientRef::Browser(yt) => run_artist_albums(&yt, &aid, &p).await?,
        };
        Ok(albums
            .into_iter()
            .map(|a| {
                model::Entry::Album(model::AlbumRef {
                    id: a.browse_id.get_raw().to_owned(),
                    title: a.title,
                    artist: String::new(),
                    year: a.year,
                    kind: a.category,
                    thumbnail: last_thumb(&a.thumbnails),
                })
            })
            .collect())
    }

    /// The home feed YouTube would actually serve.
    ///
    /// Signed in this is the personalised one — Listen again, Mixed for you, Quick picks. Signed
    /// out YouTube returns only two shelves, so Explore's shelves are appended to fill the page.
    /// The top-up only fires when actually signed out, not merely on a thin shelf count — a
    /// signed-in feed that happens to come back thin (a hiccup, a fresh account, pagination
    /// stalling early) should stay as-is rather than get Explore's shelves mixed in.
    /// The home feed through an optional mood-bar filter, with its chips.
    ///
    /// One browse serves both: the chips ride the response header, so no second request. A
    /// filter skips the Explore top-up — padding "Relax" with New releases would be nonsense —
    /// while continuations are still followed, since the token identifies the filtered feed
    /// server-side.
    pub async fn home(
        &self,
        filter: Option<&str>,
        fetch: &model::HomeFetch,
    ) -> Result<model::HomeFeed> {
        let started = std::time::Instant::now();
        let json = match filter {
            Some(params) => {
                self.run_browse(BrowseQuery::filtered(HOME, params.to_owned()), HOME)
                    .await?
            }
            None => self.browse(HOME).await?,
        };
        let mut shelves = innertube::parse_browse_shelves(&json);
        let chips = innertube::parse_home_chips(&json);

        // Home pages its shelves in too. Following a few is what turns a signed-in feed from three
        // shelves into something that looks like YouTube's; bounded, because it does not end.
        //
        // How many is the load's policy: a few for the ordinary one; for a strict one, until the
        // sections the listener ranked highest have all appeared, or the clock or the page cap
        // says enough. A strict load never waits on one slow page past its budget, and what it
        // leaves unread stays in the token, so scrolling carries on from there.
        let mut token = innertube::continuation_token(&json);
        let mut pages = 0;
        let stop = loop {
            if token.is_none() {
                break model::HomeStop::FeedEnded;
            }
            let seen: Vec<_> = shelves.iter().map(model::Shelf::home_section).collect();
            if let Some(why) = model::home_fetch_stop(fetch, pages, &seen, started.elapsed()) {
                break why;
            }
            let next = token.clone().unwrap_or_default();
            let request = self.run_browse(BrowseQuery::next_page(next), HOME);
            let page = match fetch {
                model::HomeFetch::Strict { budget, .. } => {
                    let left = budget.saturating_sub(started.elapsed());
                    match tokio::time::timeout(left, request).await {
                        Ok(page) => page,
                        Err(_) => break model::HomeStop::Budget,
                    }
                }
                model::HomeFetch::Quick => request.await,
            };
            pages += 1;
            // A page that fails or comes back empty ends the feed, as it always has.
            let more = match page {
                Ok(json) => {
                    let more = innertube::parse_browse_shelves(&json);
                    if more.is_empty() {
                        token = None;
                        break model::HomeStop::FeedEnded;
                    }
                    token = innertube::continuation_token(&json);
                    more
                }
                Err(_) => {
                    token = None;
                    break model::HomeStop::FeedEnded;
                }
            };
            shelves.extend(more);
        };
        if let model::HomeFetch::Strict { wanted, .. } = fetch {
            let seen: Vec<_> = shelves.iter().map(model::Shelf::home_section).collect();
            let missing: Vec<_> = wanted.iter().filter(|w| !seen.contains(w)).collect();
            tracing::info!(
                pages,
                shelves = shelves.len(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                stopped = ?stop,
                ?missing,
                "strict home fetch"
            );
        }

        if filter.is_none() && shelves.len() < THIN_HOME && !self.is_signed_in().await {
            // This only ever runs for a genuinely signed-out response — if Explore-like shelves
            // still show up on Home while signed in, this line's absence from the log (with
            // `RUST_LOG=refryt=debug`) proves it: that content came from YouTube's own
            // personalised feed, not from this top-up.
            tracing::debug!(
                shelves = shelves.len(),
                "home feed thin and signed out — topping up with Explore"
            );
            match self.browse(EXPLORE).await {
                Ok(json) => shelves.extend(innertube::parse_browse_shelves(&json)),
                Err(e) => tracing::warn!(error = %format!("{e:#}"), "explore feed unavailable"),
            }
        }
        if shelves.is_empty() {
            anyhow::bail!("YouTube returned an empty home feed");
        }
        Ok(model::HomeFeed {
            shelves,
            chips,
            next: token,
        })
    }

    /// One more page of the home feed, from the token the last page ended on — what loads as the
    /// listener scrolls toward the end. The token already names the feed (and its filter), so
    /// nothing else is needed. Returns the shelves and where the feed continues after them.
    pub async fn home_more(&self, token: String) -> Result<(Vec<model::Shelf>, Option<String>)> {
        let json = self.run_browse(BrowseQuery::next_page(token), HOME).await?;
        Ok((
            innertube::parse_browse_shelves(&json),
            innertube::continuation_token(&json),
        ))
    }

    /// The Explore landing: new albums & singles, mood chips, new music videos.
    ///
    /// Read by the same `parse_browse_shelves` as home — the shelves are the same shape, which is
    /// why home has been able to borrow them to pad a thin signed-out feed all along. The top
    /// buttons ride along: their browse ids and params come from the feed, not from code.
    pub async fn explore(&self) -> Result<model::ExploreFeed> {
        let json = self.browse(EXPLORE).await?;
        let shelves = innertube::parse_browse_shelves(&json);
        if shelves.is_empty() {
            anyhow::bail!("YouTube returned an empty Explore feed");
        }
        let links = innertube::parse_explore_links(&json);
        Ok(model::ExploreFeed { shelves, links })
    }

    /// New releases, for Explore's first button. The same shelf shape as every other feed.
    pub async fn new_releases(&self) -> Result<Vec<model::Shelf>> {
        let json = self.browse(NEW_RELEASES).await?;
        let shelves = innertube::parse_browse_shelves(&json);
        if shelves.is_empty() {
            anyhow::bail!("YouTube returned an empty New releases feed");
        }
        Ok(shelves)
    }

    /// Charts, for Explore's second button: ranked sections plus card shelves. The feed's own
    /// params ride along where Explore sends them.
    pub async fn charts(&self, params: Option<String>) -> Result<model::ChartsFeed> {
        let json = self
            .run_browse_owned(BrowseQuery::sorted(CHARTS, params), "charts")
            .await?;
        let feed = innertube::parse_charts(&json);
        if feed.ranked.is_empty() && feed.shelves.is_empty() {
            anyhow::bail!("YouTube returned an empty Charts feed");
        }
        Ok(feed)
    }

    /// Whether the signed-in account holds YouTube Premium, read off its "Paid memberships"
    /// page — see [`innertube::parse_premium`]. Signed out is `Ok(false)`: there is no account
    /// to hold anything.
    pub async fn is_premium(&self) -> Result<bool> {
        if !self.is_signed_in().await {
            return Ok(false);
        }
        let json = self.browse(MEMBERSHIPS).await?;
        dump("REFRYT_DUMP_MEMBERSHIPS", &json);
        Ok(innertube::parse_premium(&json))
    }

    /// What you played, in dated shelves. Same shape as every other shelf feed, so the page
    /// reuses the ordinary shelf renderer — playing, right-clicking and rating work for free.
    ///
    /// Unlike New Releases/Charts (never legitimately empty, so an empty parse there means the
    /// parser broke), History genuinely can be empty — a fresh account, paused watch history,
    /// or just nothing played yet — so it is not treated as an error on its own. Only a
    /// signed-out answer is: that one really does mean something went wrong (an expired
    /// session slipping through as if it were anonymous), and `looks_signed_out` tells it from
    /// an empty history by YouTube's own `logged_in` flag.
    pub async fn history(&self) -> Result<Vec<model::Shelf>> {
        let json = self.browse(HISTORY).await?;
        if innertube::looks_signed_out(&json) {
            // The cookie may have gone stale under us, same as `library_page` already guards
            // against — retry once after a fresh browser re-import before giving up.
            if self.refresh_from_source().await {
                let json = self.browse(HISTORY).await?;
                if !innertube::looks_signed_out(&json) {
                    return Ok(innertube::parse_browse_shelves(&json));
                }
            }
            anyhow::bail!("YouTube returned an empty history feed");
        }
        Ok(innertube::parse_browse_shelves(&json))
    }

    /// Follow or unfollow an artist. Signed in only.
    ///
    /// `params` is the button's own token from the page it was clicked on, when there was one:
    /// YouTube's web client sends it with the call, and some channels refuse the write without it.
    pub async fn set_subscribed(
        &self,
        channel_id: &str,
        subscribe: bool,
        params: Option<&str>,
    ) -> Result<()> {
        let query = SubscribeQuery {
            channel_id: channel_id.to_owned(),
            subscribe,
            params: params.map(str::to_owned),
        };
        let json = match self.client_ref().await? {
            ClientRef::NoAuth(_) => anyhow::bail!("sign in to subscribe"),
            ClientRef::Browser(yt) => yt.json_query(query).await,
        };
        if let Err(e) = &json {
            note_write_failure("subscribe", channel_id, e);
        }
        json.map(|_| ())
            .with_context(|| format!("could not update the subscription to {channel_id}"))
    }

    /// A watch queue for anything YouTube handed us a target for — an artist's shuffle or mix.
    ///
    /// The same `next` call the song mix uses; only the ids differ, and they come from the
    /// response rather than being built.
    pub async fn watch(&self, target: &model::WatchTarget) -> Result<innertube::WatchQueue> {
        let query = NextQuery::target(target);
        let json = match self.client_ref().await? {
            ClientRef::NoAuth(yt) => run_next_json(&yt, query).await,
            ClientRef::Browser(yt) => run_next_json(&yt, query).await,
        }?;
        dump("REFRYT_DUMP_NEXT", &json);
        let queue = innertube::parse_watch_queue(&json);
        if queue.tracks.is_empty() {
            anyhow::bail!("YouTube returned an empty watch queue");
        }
        Ok(queue)
    }

    /// Create a playlist. Signed in only; private, as YouTube's own default is.
    pub async fn create_playlist(&self, title: &str) -> Result<()> {
        let query = CreatePlaylistQuery {
            title: title.to_owned(),
        };
        let json = match self.client_ref().await? {
            ClientRef::NoAuth(_) => anyhow::bail!("sign in to make playlists"),
            ClientRef::Browser(yt) => yt.json_query(query).await,
        };
        json.map(|_| ())
            .with_context(|| format!("could not create the playlist {title}"))
    }

    /// Run a raw browse against whichever client is current.
    pub(crate) async fn browse(&self, browse_id: &'static str) -> Result<serde_json::Value> {
        self.run_browse(BrowseQuery::feed(browse_id), browse_id)
            .await
    }

    /// A browse with a sort order, or the next page of one already open.
    async fn run_browse(&self, query: BrowseQuery, what: &str) -> Result<serde_json::Value> {
        self.run_browse_owned(query, what).await
    }

    async fn run_browse_owned(&self, query: BrowseQuery, what: &str) -> Result<serde_json::Value> {
        let browse_id = query
            .browse_id
            .clone()
            .unwrap_or_else(|| "continuation".to_owned());
        let json = match self.client_ref().await? {
            ClientRef::NoAuth(yt) => yt.json_query(query).await,
            ClientRef::Browser(yt) => yt.json_query(query).await,
        };
        let json = json
            .map(ytmapi_rs::json::Json::into_inner)
            .with_context(|| format!("could not load {what}"))?;
        // One file per feed: a single path would be overwritten by whichever browse finished last,
        // which is never the one you are trying to look at.
        if let Some(dir) = std::env::var_os("REFRYT_DUMP_BROWSE") {
            let path = std::path::Path::new(&dir).with_extension(format!("{browse_id}.json"));
            let _ = std::fs::write(path, serde_json::to_vec_pretty(&json).unwrap_or_default());
        }
        Ok(json)
    }

    /// "Moods & genres" sections for the home page. Works signed out.
    pub async fn moods(&self) -> Result<Vec<model::MoodSection>> {
        let client = self.client_ref().await?;
        let sections = match client {
            ClientRef::NoAuth(yt) => yt.get_mood_categories().await?,
            ClientRef::Browser(yt) => yt.get_mood_categories().await?,
        };
        Ok(sections
            .into_iter()
            .map(|s: MoodCategorySection| model::MoodSection {
                name: s.section_name,
                moods: s
                    .mood_categories
                    .into_iter()
                    .map(|m| model::MoodRef {
                        title: m.title,
                        params: m.params.get_raw().to_owned(),
                        // The typed categories carry no stripe colour.
                        colour: None,
                    })
                    .collect(),
            })
            .collect())
    }

    /// The playlists behind one mood chip. Works signed out.
    ///
    /// Parsed by us first: `ytmapi_rs::get_mood_playlists`'s typed parser only recognises two
    /// renderer shapes at this path (a grid or a carousel of `musicTwoRowItemRenderer`s) and
    /// YouTube now sends something else, so the typed call throws on every mood/genre chip.
    /// Falls back to it only if our own parse cannot find anything, same as `get_artist`.
    pub async fn mood_playlists(&self, params: &str) -> Result<model::MoodPlaylists> {
        let client = self.client_ref().await?;
        let json = match &client {
            ClientRef::NoAuth(yt) => run_mood_playlists_json(yt, params).await,
            ClientRef::Browser(yt) => run_mood_playlists_json(yt, params).await,
        };
        match json {
            Ok(json) => {
                dump("REFRYT_DUMP_MOOD_PLAYLISTS", &json);
                if let Some(playlists) = innertube::parse_mood_playlists(&json) {
                    return Ok(playlists);
                }
                tracing::warn!("mood playlists parsed to nothing; using the typed query");
            }
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "raw mood playlists fetch failed");
            }
        }
        let p = params.to_owned();
        let categories = match client {
            ClientRef::NoAuth(yt) => {
                yt.get_mood_playlists(ytmapi_rs::common::MoodCategoryParams::from_raw(p))
                    .await?
            }
            ClientRef::Browser(yt) => {
                yt.get_mood_playlists(ytmapi_rs::common::MoodCategoryParams::from_raw(p))
                    .await?
            }
        };
        let title = categories
            .first()
            .map(|c: &MoodPlaylistCategory| c.category_name.clone())
            .unwrap_or_else(|| "Playlists".to_owned());
        Ok(model::MoodPlaylists {
            title,
            playlists: categories
                .into_iter()
                .flat_map(|c| c.playlists)
                .map(|pl| model::PlaylistRef {
                    id: pl.playlist_id.get_raw().to_owned(),
                    title: pl.title,
                    author: pl.author,
                    author_id: None,
                    track_count: None,
                    thumbnail: last_thumb(&pl.thumbnails),
                    // A mood page lists YouTube's playlists, never yours.
                    editable: false,
                    source: model::PlaylistSource::YouTube,
                })
                .collect(),
        })
    }

    /// Radio for a song: the `next` endpoint's watch queue, parsed by us — `ytmapi-rs` has no
    /// query type for it. The mix id is derivable (`RDAMVM` + video id); the returned token
    /// extends the river past its first page (M14.9). Works signed out.
    pub async fn mix(&self, video_id: &str) -> Result<innertube::WatchQueue> {
        let query = NextQuery::mix_for(video_id);
        let json = match self.client_ref().await? {
            ClientRef::NoAuth(yt) => run_next_json(&yt, query).await,
            ClientRef::Browser(yt) => run_next_json(&yt, query).await,
        }?;
        dump("REFRYT_DUMP_NEXT", &json);
        let queue = innertube::parse_watch_queue(&json);
        if queue.tracks.is_empty() {
            anyhow::bail!("YouTube returned an empty watch queue for {video_id}");
        }
        Ok(queue)
    }

    /// Radio for a playlist or album — the playlist-based counterpart of [`mix`]. Same `next`
    /// call and same queue shape; only the id prefix differs (`RDAMPL`). Works signed out.
    pub async fn mix_playlist(&self, playlist_id: &str) -> Result<innertube::WatchQueue> {
        let query = NextQuery::playlist_radio(playlist_id);
        let json = match self.client_ref().await? {
            ClientRef::NoAuth(yt) => run_next_json(&yt, query).await,
            ClientRef::Browser(yt) => run_next_json(&yt, query).await,
        }?;
        dump("REFRYT_DUMP_NEXT", &json);
        let queue = innertube::parse_watch_queue(&json);
        if queue.tracks.is_empty() {
            anyhow::bail!("YouTube returned an empty watch queue for {playlist_id}");
        }
        Ok(queue)
    }

    /// The next ~50 tracks of a radio river, following a `nextRadioContinuationData` token.
    /// Works signed out.
    pub async fn mix_more(&self, token: &str) -> Result<innertube::WatchQueue> {
        let query = NextQuery::continuation(token);
        let json = match self.client_ref().await? {
            ClientRef::NoAuth(yt) => run_next_json(&yt, query).await,
            ClientRef::Browser(yt) => run_next_json(&yt, query).await,
        }?;
        dump("REFRYT_DUMP_NEXT_MORE", &json);
        let queue = innertube::parse_watch_queue(&json);
        if queue.tracks.is_empty() {
            anyhow::bail!("YouTube returned an empty watch continuation");
        }
        Ok(queue)
    }

    /// Plain lyrics from InnerTube, if any. `ytmapi-rs` wraps the two calls: the `next`
    /// response names the lyrics browse id, and the browse returns the text.
    pub async fn lyrics(&self, video_id: &str) -> Result<Option<model::Lyrics>> {
        let vid = VideoID::from_raw(video_id.to_owned());
        let id = match self.client_ref().await? {
            ClientRef::NoAuth(yt) => yt.get_lyrics_id(vid).await,
            ClientRef::Browser(yt) => yt.get_lyrics_id(vid).await,
        };
        // No lyrics is a normal answer, not an error worth surfacing.
        let Ok(id) = id else { return Ok(None) };
        let lyrics = match self.client_ref().await? {
            ClientRef::NoAuth(yt) => yt.get_lyrics(id).await,
            ClientRef::Browser(yt) => yt.get_lyrics(id).await,
        }
        .context("could not load lyrics")?;
        Ok(Some(model::Lyrics {
            lines: lyrics
                .lyrics
                .lines()
                .map(|l| model::LyricLine {
                    at_ms: 0,
                    text: l.trim_end().to_owned(),
                })
                .collect(),
            synced: false,
            source: lyrics.source,
        }))
    }

    /// Rate a song. Signed in only — there is no anonymous meaning, so this bails instead of
    /// sending a request YouTube would refuse. Either session type works; what matters is that
    /// one exists.
    pub async fn rate(&self, video_id: &str, status: model::LikeStatus) -> Result<()> {
        let query = LikeQuery {
            target: LikeTarget::Video(video_id.to_owned()),
            status,
        };
        let json = match self.client_ref().await? {
            ClientRef::NoAuth(_) => anyhow::bail!("sign in to like songs"),
            ClientRef::Browser(yt) => yt.json_query(query).await,
        };
        if let Err(e) = &json {
            note_write_failure("rate", video_id, e);
        }
        json.map(|_| ())
            .with_context(|| format!("could not rate {video_id}"))
    }

    /// Save an album to the library, or remove it. The album header carries no feedback
    /// token — saving is a like on the album's like-playlist id (the play button's), so
    /// this rides the like endpoint with a playlist target. Signed in only.
    pub async fn rate_playlist(&self, playlist_id: &str, status: model::LikeStatus) -> Result<()> {
        let query = LikeQuery {
            target: LikeTarget::Playlist(playlist_id.to_owned()),
            status,
        };
        let json = match self.client_ref().await? {
            ClientRef::NoAuth(_) => anyhow::bail!("sign in to save albums"),
            ClientRef::Browser(yt) => yt.json_query(query).await,
        };
        if let Err(e) = &json {
            note_write_failure("save", playlist_id, e);
        }
        let json = json
            .map(ytmapi_rs::json::Json::into_inner)
            .with_context(|| format!("could not update the playlist {playlist_id}"))?;
        dump("REFRYT_DUMP_RATE", &json);
        Ok(())
    }

    /// Save a song to your library, or remove it. Signed in only. The token is the row's own
    /// bookmark token — `library_add` to save, `library_remove` to remove — so the call never
    /// carries anything the page did not hand it.
    pub async fn feedback(&self, token: &str) -> Result<()> {
        let query = FeedbackQuery {
            token: token.to_owned(),
        };
        let json = match self.client_ref().await? {
            ClientRef::NoAuth(_) => anyhow::bail!("sign in to save songs"),
            ClientRef::Browser(yt) => yt.json_query(query).await,
        };
        if let Err(e) = &json {
            note_write_failure("library", token, e);
        }
        json.map(|_| ())
            .with_context(|| "could not update the library".to_owned())
    }

    /// Add a video to one of your playlists, or remove it by its playlist item id. Signed in
    /// only. The edit endpoint wants the bare playlist id; the library hands out the `VL`
    /// browse form, so it is stripped here.
    ///
    /// A refusal in the reply's own status is an error here too — it used to pass as success,
    /// which is how a song already in the playlist was toasted as saved. An add goes through
    /// [`Self::add_to_playlist`] instead, which tells a duplicate apart.
    pub async fn edit_playlist(&self, playlist_id: &str, edit: PlaylistEdit) -> Result<()> {
        match self.edit_playlist_reply(playlist_id, edit).await? {
            innertube::EditReply::Done => Ok(()),
            innertube::EditReply::Duplicate(m) | innertube::EditReply::Refused(m) => {
                anyhow::bail!("YouTube refused the edit to {playlist_id}: {m}")
            }
        }
    }

    /// Add a song to one of your playlists. A song already there is not added, and the reply
    /// says so — unless `allow_duplicate`, the dialog's "Add anyway", skips that check.
    pub async fn add_to_playlist(
        &self,
        playlist_id: &str,
        video_id: &str,
        allow_duplicate: bool,
    ) -> Result<innertube::EditReply> {
        let edit = PlaylistEdit::AddVideo {
            video_id: video_id.to_owned(),
            allow_duplicate,
        };
        self.edit_playlist_reply(playlist_id, edit).await
    }

    async fn edit_playlist_reply(
        &self,
        playlist_id: &str,
        edit: PlaylistEdit,
    ) -> Result<innertube::EditReply> {
        let query = EditPlaylistQuery {
            playlist_id: playlist_id.to_owned(),
            edit,
        };
        let json = match self.client_ref().await? {
            ClientRef::NoAuth(_) => anyhow::bail!("sign in to edit playlists"),
            ClientRef::Browser(yt) => yt.json_query(query).await,
        };
        let json = json
            .map(ytmapi_rs::json::Json::into_inner)
            .with_context(|| format!("could not edit playlist {playlist_id}"))?;
        dump("REFRYT_DUMP_EDIT", &json);
        Ok(innertube::parse_edit_reply(&json))
    }

    /// Delete one of your own playlists. Signed in only. The path wants the bare id.
    pub async fn delete_playlist(&self, playlist_id: &str) -> Result<()> {
        let query = DeletePlaylistQuery {
            playlist_id: playlist_id.to_owned(),
        };
        let json = match self.client_ref().await? {
            ClientRef::NoAuth(_) => anyhow::bail!("sign in to delete playlists"),
            ClientRef::Browser(yt) => yt.json_query(query).await,
        };
        json.map(|_| ())
            .with_context(|| format!("could not delete playlist {playlist_id}"))
    }

    /// Where this play reports itself. Signed in only — an anonymous play has no account to
    /// count on, so this is `None` rather than a beacon YouTube would ignore. Either tracking
    /// URL missing is the same answer: an unplayable shape, not half a tracker.
    pub async fn playback_tracking(
        &self,
        video_id: &str,
    ) -> Result<Option<model::PlaybackTracking>> {
        let query = GetSongTrackingUrlQuery::new(VideoID::from_raw(video_id))
            .context("could not build the player request")?;
        let json = match self.client_ref().await? {
            ClientRef::NoAuth(_) => return Ok(None),
            ClientRef::Browser(yt) => yt.json_query(query).await,
        }
        .map(ytmapi_rs::json::Json::into_inner)
        .with_context(|| format!("could not load the player for {video_id}"))?;
        dump("REFRYT_DUMP_PLAYER", &json);
        Ok(innertube::parse_playback_tracking(&json))
    }

    /// Fire the playback-start beacon: a GET on the tracking URL is what writes the history
    /// item. Signed in only — anonymous callers never get a URL to fire.
    pub async fn report_playback(&self, playback_url: &str) -> Result<()> {
        let query = AddHistoryItemQuery::new(SongTrackingUrl::from_raw(playback_url));
        match self.client_ref().await? {
            ClientRef::NoAuth(_) => anyhow::bail!("sign in to scrobble plays"),
            ClientRef::Browser(yt) => yt.query(query).await,
        }
        .with_context(|| "could not report the play".to_owned())
    }

    /// The stored browser cookie, for requests that ride outside `ytmapi-rs` — the watchtime
    /// pings are plain GETs, not InnerTube calls, so they carry the cookie themselves.
    /// The slot yt-dlp reads the session from; see [`refryt_resolve::YtDlpResolver::with_session`].
    pub fn ytdlp_session(&self) -> refryt_resolve::SessionCookie {
        std::sync::Arc::clone(&self.ytdlp_session)
    }

    fn share_with_ytdlp(&self, cookie: Option<&str>) {
        if let Ok(mut slot) = self.ytdlp_session.write() {
            *slot = cookie.map(str::to_owned);
        }
    }

    pub async fn session_cookie(&self) -> Option<String> {
        self.session.lock().await.clone().map(|s| s.cookie)
    }
}

// ---------------------------------------------------------------------------------------------
// The query calls, generic over the token so both client variants share one implementation.
// ---------------------------------------------------------------------------------------------

/// A raw browse of a fixed feed id, for the endpoints `ytmapi-rs` has no query type for.
///
/// The crate documents implementing `Query` from outside (see its `query` module), so this reuses
/// its client, auth, request signing and consent cookie rather than opening a second HTTP path to
/// YouTube. `Output` is never constructed: `json_query` returns the response before parsing, and
/// [`crate::innertube`] reads it.
struct BrowseQuery {
    /// The feed to open. `None` for a continuation, which identifies itself by token alone.
    browse_id: Option<String>,
    /// Sort order, or a home-feed filter — both ride the same body key, and both are runtime
    /// values (the mood bar's params arrive in the feed itself).
    params: Option<String>,
    /// The next page of a feed already opened.
    continuation: Option<String>,
}

impl BrowseQuery {
    fn feed(browse_id: &'static str) -> Self {
        Self::feed_owned(browse_id.to_owned())
    }

    fn feed_owned(browse_id: String) -> Self {
        Self {
            browse_id: Some(browse_id),
            params: None,
            continuation: None,
        }
    }

    fn sorted(browse_id: &'static str, params: Option<String>) -> Self {
        Self {
            browse_id: Some(browse_id.to_owned()),
            params,
            continuation: None,
        }
    }

    /// The home feed through one of its mood-bar chips.
    fn filtered(browse_id: &'static str, params: String) -> Self {
        Self {
            browse_id: Some(browse_id.to_owned()),
            params: Some(params),
            continuation: None,
        }
    }

    fn next_page(token: String) -> Self {
        Self {
            browse_id: None,
            params: None,
            continuation: Some(token),
        }
    }
}

#[derive(Debug)]
struct RawBrowse;

/// The account switcher's list of channels this Google account can browse as.
struct AccountsListQuery;

impl ytmapi_rs::parse::ParseFrom<AccountsListQuery> for RawBrowse {
    fn parse_from(
        _: ytmapi_rs::parse::ProcessedResult<AccountsListQuery>,
    ) -> ytmapi_rs::Result<Self> {
        Ok(RawBrowse)
    }
}

impl<A: AuthToken> ytmapi_rs::query::Query<A> for AccountsListQuery {
    type Output = RawBrowse;
    type Method = ytmapi_rs::query::PostMethod;
}

impl ytmapi_rs::query::PostQuery for AccountsListQuery {
    fn header(&self) -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::new()
    }

    fn params(&self) -> Vec<(&str, std::borrow::Cow<'_, str>)> {
        vec![]
    }

    fn path(&self) -> &str {
        "account/accounts_list"
    }
}

impl ytmapi_rs::parse::ParseFrom<BrowseQuery> for RawBrowse {
    fn parse_from(_: ytmapi_rs::parse::ProcessedResult<BrowseQuery>) -> ytmapi_rs::Result<Self> {
        Ok(RawBrowse)
    }
}

impl<A: AuthToken> ytmapi_rs::query::Query<A> for BrowseQuery {
    type Output = RawBrowse;
    type Method = ytmapi_rs::query::PostMethod;
}

impl ytmapi_rs::query::PostQuery for BrowseQuery {
    fn header(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut body = serde_json::Map::new();
        if let Some(id) = &self.browse_id {
            body.insert("browseId".to_owned(), serde_json::json!(id));
        }
        if let Some(params) = &self.params {
            body.insert("params".to_owned(), serde_json::json!(params));
        }
        if let Some(token) = &self.continuation {
            body.insert("continuation".to_owned(), serde_json::json!(token));
        }
        body
    }

    fn params(&self) -> Vec<(&str, std::borrow::Cow<'_, str>)> {
        vec![]
    }

    fn path(&self) -> &str {
        "browse"
    }
}

/// A raw call to an InnerTube endpoint that takes no arguments beyond the client context.
///
/// `account/account_menu` is the one that matters: it is the only cheap way to tell a real session
/// from a cookie YouTube is quietly ignoring.
struct AccountMenuQuery;

impl ytmapi_rs::parse::ParseFrom<AccountMenuQuery> for RawBrowse {
    fn parse_from(
        _: ytmapi_rs::parse::ProcessedResult<AccountMenuQuery>,
    ) -> ytmapi_rs::Result<Self> {
        Ok(RawBrowse)
    }
}

impl<A: AuthToken> ytmapi_rs::query::Query<A> for AccountMenuQuery {
    type Output = RawBrowse;
    type Method = ytmapi_rs::query::PostMethod;
}

impl ytmapi_rs::query::PostQuery for AccountMenuQuery {
    fn header(&self) -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::new()
    }

    fn params(&self) -> Vec<(&str, std::borrow::Cow<'_, str>)> {
        vec![]
    }

    fn path(&self) -> &str {
        "account/account_menu"
    }
}

/// A raw call to the `next` endpoint: the watch queue behind autoplay and Start Mix.
///
/// `video_id` with the `RDAMVM`-derived playlist id and `wAEB` params opens radio for a song;
/// `continuation` follows a `nextRadioContinuationData` token (M14.9). Follows the
/// `AccountsListQuery` pattern: `Output` is never built, `json_query` hands back the raw
/// response for [`crate::innertube`] to read.
struct NextQuery {
    video_id: Option<String>,
    playlist_id: Option<String>,
    params: Option<String>,
    continuation: Option<String>,
}

impl NextQuery {
    /// Radio for a song. The mix id is derivable: `RDAMVM` + the video id.
    fn mix_for(video_id: &str) -> Self {
        Self {
            video_id: Some(video_id.to_owned()),
            playlist_id: Some(format!("RDAMVM{video_id}")),
            params: Some("wAEB".to_owned()),
            continuation: None,
        }
    }

    /// Radio for a playlist or album: the playlist-based counterpart of `mix_for`, `RDAMPL` +
    /// the bare playlist id, with `wAEB` asking `next` to randomise it (the same convention
    /// YouTube Music's own "Start radio" follows, and what ytmusicapi calls radio). The id must
    /// be bare — no `VL` — and for an album it is the play button's `audioPlaylistId`, not the
    /// `MPREb_` browse id.
    fn playlist_radio(playlist_id: &str) -> Self {
        Self {
            video_id: None,
            playlist_id: Some(format!(
                "RDAMPL{}",
                crate::model::bare_playlist_id(playlist_id)
            )),
            params: Some("wAEB".to_owned()),
            continuation: None,
        }
    }

    /// Whatever YouTube handed over as something to play — an artist's shuffle or radio.
    fn target(t: &model::WatchTarget) -> Self {
        Self {
            video_id: t.video_id.clone(),
            playlist_id: Some(t.playlist_id.clone()),
            params: t.params.clone(),
            continuation: None,
        }
    }

    /// The next page of an open river. The token identifies itself — measured 2026-09-08, a
    /// bare continuation returns 49 more tracks — so the other fields stay `None`. They remain
    /// sendable because the shape is YouTube's to change.
    fn continuation(token: &str) -> Self {
        Self {
            video_id: None,
            playlist_id: None,
            params: None,
            continuation: Some(token.to_owned()),
        }
    }
}

impl ytmapi_rs::parse::ParseFrom<NextQuery> for RawBrowse {
    fn parse_from(_: ytmapi_rs::parse::ProcessedResult<NextQuery>) -> ytmapi_rs::Result<Self> {
        Ok(RawBrowse)
    }
}

impl<A: AuthToken> ytmapi_rs::query::Query<A> for NextQuery {
    type Output = RawBrowse;
    type Method = ytmapi_rs::query::PostMethod;
}

impl ytmapi_rs::query::PostQuery for NextQuery {
    fn header(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut body = serde_json::Map::new();
        if let Some(video_id) = &self.video_id {
            body.insert("videoId".to_owned(), serde_json::json!(video_id));
        }
        if let Some(playlist_id) = &self.playlist_id {
            body.insert("playlistId".to_owned(), serde_json::json!(playlist_id));
        }
        if let Some(params) = &self.params {
            body.insert("params".to_owned(), serde_json::json!(params));
        }
        if let Some(token) = &self.continuation {
            body.insert("continuation".to_owned(), serde_json::json!(token));
        }
        body.insert("isAudioOnly".to_owned(), serde_json::json!(true));
        body
    }

    fn params(&self) -> Vec<(&str, std::borrow::Cow<'_, str>)> {
        vec![]
    }

    fn path(&self) -> &str {
        "next"
    }
}

/// A rating write, taken from the `likeEndpoint` in a signed-in dump: `like/like`,
/// `like/dislike` or `like/removelike` with `{"target": {"videoId"}}`.
struct LikeQuery {
    target: LikeTarget,
    status: model::LikeStatus,
}

/// What a like endpoint rates: a video, or an album by its like-playlist id.
enum LikeTarget {
    Video(String),
    Playlist(String),
}

impl ytmapi_rs::parse::ParseFrom<LikeQuery> for RawBrowse {
    fn parse_from(_: ytmapi_rs::parse::ProcessedResult<LikeQuery>) -> ytmapi_rs::Result<Self> {
        Ok(RawBrowse)
    }
}

impl<A: AuthToken> ytmapi_rs::query::Query<A> for LikeQuery {
    type Output = RawBrowse;
    type Method = ytmapi_rs::query::PostMethod;
}

impl ytmapi_rs::query::PostQuery for LikeQuery {
    fn header(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut body = serde_json::Map::new();
        let target = match &self.target {
            LikeTarget::Video(id) => serde_json::json!({ "videoId": id }),
            LikeTarget::Playlist(id) => serde_json::json!({ "playlistId": id }),
        };
        body.insert("target".to_owned(), target);
        body
    }

    fn params(&self) -> Vec<(&str, std::borrow::Cow<'_, str>)> {
        vec![]
    }

    fn path(&self) -> &str {
        match self.status {
            model::LikeStatus::Like => "like/like",
            model::LikeStatus::Dislike => "like/dislike",
            model::LikeStatus::Indifferent => "like/removelike",
        }
    }
}

/// A library write: `feedback` with one feedback token. The token comes from the row's own
/// bookmark toggle, so saving and removing are the same call with different tokens.
struct FeedbackQuery {
    token: String,
}

impl ytmapi_rs::parse::ParseFrom<FeedbackQuery> for RawBrowse {
    fn parse_from(_: ytmapi_rs::parse::ProcessedResult<FeedbackQuery>) -> ytmapi_rs::Result<Self> {
        Ok(RawBrowse)
    }
}

impl<A: AuthToken> ytmapi_rs::query::Query<A> for FeedbackQuery {
    type Output = RawBrowse;
    type Method = ytmapi_rs::query::PostMethod;
}

impl ytmapi_rs::query::PostQuery for FeedbackQuery {
    fn header(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut body = serde_json::Map::new();
        body.insert("feedbackTokens".to_owned(), serde_json::json!([self.token]));
        body
    }

    fn params(&self) -> Vec<(&str, std::borrow::Cow<'_, str>)> {
        vec![]
    }

    fn path(&self) -> &str {
        "feedback"
    }
}

/// A playlist edit: adding a video, removing one by its playlist item id, or renaming.
pub enum PlaylistEdit {
    AddVideo {
        video_id: String,
        /// Skip YouTube's already-in-the-playlist check: the dialog's "Add anyway".
        allow_duplicate: bool,
    },
    RemoveVideo {
        video_id: String,
        set_video_id: String,
    },
    /// Move one track to sit immediately before another. Both are playlist item ids
    /// (`setVideoId`), not video ids — a song can appear in the same playlist twice, and only
    /// the item id says which occurrence moved.
    MoveVideo {
        set_video_id: String,
        before_set_video_id: String,
    },
    /// Rename, and/or set the description. Either may be absent, but not both.
    SetMeta {
        title: Option<String>,
        description: Option<String>,
    },
}

struct EditPlaylistQuery {
    playlist_id: String,
    edit: PlaylistEdit,
}

impl ytmapi_rs::parse::ParseFrom<EditPlaylistQuery> for RawBrowse {
    fn parse_from(
        _: ytmapi_rs::parse::ProcessedResult<EditPlaylistQuery>,
    ) -> ytmapi_rs::Result<Self> {
        Ok(RawBrowse)
    }
}

impl<A: AuthToken> ytmapi_rs::query::Query<A> for EditPlaylistQuery {
    type Output = RawBrowse;
    type Method = ytmapi_rs::query::PostMethod;
}

impl ytmapi_rs::query::PostQuery for EditPlaylistQuery {
    fn header(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut body = serde_json::Map::new();
        body.insert(
            "playlistId".to_owned(),
            serde_json::json!(model::bare_playlist_id(&self.playlist_id)),
        );
        let action = match &self.edit {
            PlaylistEdit::AddVideo {
                video_id,
                allow_duplicate,
            } => {
                let mut add = serde_json::json!({
                    "action": "ACTION_ADD_VIDEO",
                    "addedVideoId": video_id,
                });
                if *allow_duplicate {
                    add["dedupeOption"] = serde_json::json!("DEDUPE_OPTION_SKIP");
                }
                add
            }
            PlaylistEdit::RemoveVideo {
                video_id,
                set_video_id,
            } => serde_json::json!({
                "action": "ACTION_REMOVE_VIDEO",
                "removedVideoId": video_id,
                "setVideoId": set_video_id,
            }),
            PlaylistEdit::MoveVideo {
                set_video_id,
                before_set_video_id,
            } => serde_json::json!({
                "action": "ACTION_MOVE_VIDEO_BEFORE",
                "setVideoId": set_video_id,
                "movedSetVideoIdSuccessor": before_set_video_id,
            }),
            PlaylistEdit::SetMeta { title, description } => {
                let mut actions = Vec::new();
                if let Some(title) = title {
                    actions.push(serde_json::json!({
                        "action": "ACTION_SET_PLAYLIST_NAME",
                        "playlistName": title,
                    }));
                }
                if let Some(description) = description {
                    actions.push(serde_json::json!({
                        "action": "ACTION_SET_PLAYLIST_DESCRIPTION",
                        "playlistDescription": description,
                    }));
                }
                body.insert("actions".to_owned(), serde_json::json!(actions));
                return body;
            }
        };
        body.insert("actions".to_owned(), serde_json::json!([action]));
        body
    }

    fn params(&self) -> Vec<(&str, std::borrow::Cow<'_, str>)> {
        vec![]
    }

    fn path(&self) -> &str {
        "browse/edit_playlist"
    }
}

/// Deleting one of your own playlists: `POST playlist/delete` with the bare id.
struct DeletePlaylistQuery {
    playlist_id: String,
}

impl ytmapi_rs::parse::ParseFrom<DeletePlaylistQuery> for RawBrowse {
    fn parse_from(
        _: ytmapi_rs::parse::ProcessedResult<DeletePlaylistQuery>,
    ) -> ytmapi_rs::Result<Self> {
        Ok(RawBrowse)
    }
}

impl<A: AuthToken> ytmapi_rs::query::Query<A> for DeletePlaylistQuery {
    type Output = RawBrowse;
    type Method = ytmapi_rs::query::PostMethod;
}

impl ytmapi_rs::query::PostQuery for DeletePlaylistQuery {
    fn header(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut body = serde_json::Map::new();
        body.insert(
            "playlistId".to_owned(),
            serde_json::json!(model::bare_playlist_id(&self.playlist_id)),
        );
        body
    }

    fn params(&self) -> Vec<(&str, std::borrow::Cow<'_, str>)> {
        vec![]
    }

    fn path(&self) -> &str {
        "playlist/delete"
    }
}

/// Follow or unfollow, which are the same call at two paths.
struct SubscribeQuery {
    channel_id: String,
    subscribe: bool,
    /// The button's own `params` token, when the page carried one. The web client always sends
    /// it; without it YouTube answers some channels with a bare 400 "invalid argument".
    params: Option<String>,
}

impl ytmapi_rs::parse::ParseFrom<SubscribeQuery> for RawBrowse {
    fn parse_from(_: ytmapi_rs::parse::ProcessedResult<SubscribeQuery>) -> ytmapi_rs::Result<Self> {
        Ok(RawBrowse)
    }
}

impl<A: AuthToken> ytmapi_rs::query::Query<A> for SubscribeQuery {
    type Output = RawBrowse;
    type Method = ytmapi_rs::query::PostMethod;
}

impl ytmapi_rs::query::PostQuery for SubscribeQuery {
    fn header(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut body = serde_json::Map::new();
        body.insert(
            "channelIds".to_owned(),
            serde_json::json!([self.channel_id]),
        );
        if let Some(params) = &self.params {
            body.insert("params".to_owned(), serde_json::json!(params));
        }
        body
    }

    fn params(&self) -> Vec<(&str, std::borrow::Cow<'_, str>)> {
        vec![]
    }

    fn path(&self) -> &str {
        if self.subscribe {
            "subscription/subscribe"
        } else {
            "subscription/unsubscribe"
        }
    }
}

/// A new playlist. `privacyStatus` is required and YouTube's own default is private.
struct CreatePlaylistQuery {
    title: String,
}

impl ytmapi_rs::parse::ParseFrom<CreatePlaylistQuery> for RawBrowse {
    fn parse_from(
        _: ytmapi_rs::parse::ProcessedResult<CreatePlaylistQuery>,
    ) -> ytmapi_rs::Result<Self> {
        Ok(RawBrowse)
    }
}

impl<A: AuthToken> ytmapi_rs::query::Query<A> for CreatePlaylistQuery {
    type Output = RawBrowse;
    type Method = ytmapi_rs::query::PostMethod;
}

impl ytmapi_rs::query::PostQuery for CreatePlaylistQuery {
    fn header(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut body = serde_json::Map::new();
        body.insert("title".to_owned(), serde_json::json!(self.title));
        body.insert("privacyStatus".to_owned(), serde_json::json!("PRIVATE"));
        body
    }

    fn params(&self) -> Vec<(&str, std::borrow::Cow<'_, str>)> {
        vec![]
    }

    fn path(&self) -> &str {
        "playlist/create"
    }
}

/// Ask YouTube whose session this is.
///
/// `Ok(Some(name))` is a confirmed session and `Ok(None)` is a confirmed anonymous one. `Err`
/// means we could not ask — a different thing entirely, and conflating the two is how a network
/// blip deleted a perfectly good stored session.
async fn account_name<A: AuthToken>(yt: &YtMusic<A>) -> Result<Option<String>> {
    let json = yt
        .json_query(AccountMenuQuery)
        .await
        .map(ytmapi_rs::json::Json::into_inner)
        .context("could not reach YouTube Music")?;
    dump("REFRYT_DUMP_ACCOUNT", &json);
    Ok(innertube::account_name(&json))
}

/// YouTube Music's own home feed.
const HOME: &str = "FEmusic_home";
/// New albums & singles, Moods & genres, New music videos. Used to fill a thin anonymous home.
const EXPLORE: &str = "FEmusic_explore";
/// Explore's first button, as the feed itself names it.
const NEW_RELEASES: &str = "FEmusic_new_releases";
/// Explore's second button. The feed's params ride along where it sends them.
const CHARTS: &str = "FEmusic_charts";
/// Dated shelves — Today, This week, and so on. Answers "Sign in to view your history" signed
/// out, which is why the app gates the page rather than the call.
const HISTORY: &str = "FEmusic_history";
/// The site's "Paid memberships" page, linked from the account menu.
const MEMBERSHIPS: &str = "FEmemberships_and_purchases";
/// Below this many shelves, the feed is the signed-out one and is worth supplementing.
const THIN_HOME: usize = 4;

/// How many extra pages of library playlists the sidebar will follow to get the whole
/// collection, not just the first page. Bounded for the same reason the ordinary Home load is (`model::QUICK_HOME_PAGES`).
const SIDEBAR_PLAYLIST_PAGES: usize = 20;

/// Which library section a continuation belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibrarySection {
    Songs,
    Artists,
    Albums,
    Playlists,
}

impl LibrarySection {
    fn browse_id(self) -> &'static str {
        match self {
            LibrarySection::Songs => LIBRARY_SONGS,
            LibrarySection::Artists => LIBRARY_ARTISTS,
            LibrarySection::Albums => LIBRARY_ALBUMS,
            LibrarySection::Playlists => LIBRARY_PLAYLISTS,
        }
    }
}

/// A library sort order, as YouTube's own params.
///
/// Only the orders YouTube can perform are offered. Sorting by artist or album was possible only
/// over the loaded page, which meant sorting 25 rows of a 689-row library and calling it a sort.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LibrarySortParam {
    /// YouTube's own default for liked songs, and what "Latest" means.
    #[default]
    RecentlySaved,
    NameAsc,
    NameDesc,
}

impl LibrarySortParam {
    /// Always explicit, including for "Latest".
    ///
    /// Sending no params does *not* mean recently-saved: measured on 2026-09-07, a bare request
    /// returns a third order of its own, and YouTube then remembers whichever order was last asked
    /// for explicitly. Leaving the param off to save nothing is what made "Latest" show an order
    /// nobody chose.
    fn params(self) -> Option<String> {
        Some(
            match self {
                LibrarySortParam::RecentlySaved => "ggMGKgQIABAB",
                LibrarySortParam::NameAsc => "ggMGKgQIARAA",
                LibrarySortParam::NameDesc => "ggMGKgQIARAB",
            }
            .to_owned(),
        )
    }
}

/// Narrow a page of mixed entries to one kind, keeping its cursor.
fn page_of<T>(page: innertube::Page, pick: impl Fn(model::Entry) -> Option<T>) -> model::Paged<T> {
    model::Paged::new(page.items.into_iter().filter_map(pick).collect(), page.next)
}

/// A playlist's browse id. YouTube addresses playlists as `VL` + the playlist id; a bare id is
/// answered with `400 Request contains an invalid argument`.
fn browse_id_for_playlist(id: &str) -> String {
    if id.starts_with("VL") {
        id.to_owned()
    } else {
        format!("VL{id}")
    }
}

/// The library browse ids. Each returns its own layout; see [`Api::library`].
const LIBRARY_SONGS: &str = "FEmusic_liked_videos";
const LIBRARY_ALBUMS: &str = "FEmusic_liked_albums";
const LIBRARY_PLAYLISTS: &str = "FEmusic_liked_playlists";
const LIBRARY_ARTISTS: &str = "FEmusic_library_corpus_artists";

/// Diagnostic hatch: `REFRYT_DUMP_SEARCH` / `REFRYT_DUMP_ARTIST` name a file to write the raw response
/// to. Settling a parsing disagreement needs the actual bytes YouTube sent *to this client* —
/// reproducing the request by hand gave a different layout and cost an hour of wrong guesses.
fn dump(var: &str, json: &serde_json::Value) {
    if let Some(path) = std::env::var_os(var) {
        let _ = std::fs::write(path, serde_json::to_vec_pretty(json).unwrap_or_default());
    }
}

/// Whether an error reads as "no connection" rather than as YouTube refusing something: the
/// request never got an answer. Read from the text because the errors arrive as strings in the
/// app's events, and each layer — reqwest, `ytmapi-rs`, yt-dlp — words it its own way.
/// What music.youtube.com sent instead of its own page, when `BrowserToken::from_str` refused a
/// session with its bare "Error parsing header" — which it says alike for a cookie it could not
/// read and for a page with no client version in it, so a report of it said nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    /// Google's own pages: sign in again, or confirm it is you.
    Google,
    /// YouTube's "unusual traffic" check.
    Robot,
    /// The cookie-consent page.
    Consent,
    /// The music page itself, or something unrecognised.
    Other,
}

impl Refusal {
    fn from_page(url: &str, page: &str) -> Self {
        let host = url
            .split("://")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .unwrap_or("");
        if host.ends_with("accounts.google.com") {
            Refusal::Google
        } else if host.starts_with("consent.") {
            Refusal::Consent
        } else if url.contains("/sorry/") || page.contains("unusual traffic") {
            Refusal::Robot
        } else {
            Refusal::Other
        }
    }

    fn advice(self) -> &'static str {
        match self {
            Refusal::Google => {
                "Google wants this account signed in again or confirmed. Open \
                 music.youtube.com in your browser, finish what it asks, then sign in again here."
            }
            Refusal::Robot => {
                "YouTube is asking to confirm you are not a robot. Open music.youtube.com in \
                 your browser, pass the check, then try again."
            }
            Refusal::Consent => {
                "YouTube is asking about cookies first. Open music.youtube.com in your browser, \
                 answer it, then sign in again here."
            }
            Refusal::Other => "YouTube Music did not accept this session. Sign in again.",
        }
    }
}

/// Look at what music.youtube.com answers this cookie with, after `from_str` refused it, and log
/// it: where the request ended up (scheme, host and path, never the query), the status, the
/// page's title and whether it carried the client version `from_str` looks for. No cookie value
/// is logged. `None` for any other error, and when the page could not be fetched either.
async fn refused_page(error: &ytmapi_rs::Error, cookie: &str) -> Option<Refusal> {
    if !error.to_string().contains("parsing header") {
        return None;
    }
    // The user agent `ytmapi-rs` signs in with, so the page is the one it was shown.
    const AGENT: &str =
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:88.0) Gecko/20100101 Firefox/88.0";
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .gzip(true)
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .ok()?;
    let response = client
        .get("https://music.youtube.com")
        .header(reqwest::header::USER_AGENT, AGENT)
        .header(reqwest::header::COOKIE, auth::for_token(cookie))
        .send()
        .await
        .inspect_err(|e| tracing::warn!(error = %e, "could not look at the refused page"))
        .ok()?;
    let status = response.status().as_u16();
    let url = response.url();
    let landed = format!(
        "{}://{}{}",
        url.scheme(),
        url.host_str().unwrap_or(""),
        url.path()
    );
    let page = response.text().await.unwrap_or_default();
    let title = page
        .split_once("<title>")
        .and_then(|(_, rest)| rest.split_once("</title>"))
        .map(|(t, _)| t.trim().chars().take(80).collect::<String>())
        .unwrap_or_default();
    let refusal = Refusal::from_page(&landed, &page);
    tracing::warn!(
        status,
        %landed,
        title,
        client_version = page.contains("INNERTUBE_CLIENT_VERSION"),
        sapisid = cookie.contains("SAPISID="),
        ?refusal,
        "YouTube Music refused the session"
    );
    Some(refusal)
}

pub fn looks_offline(error: &str) -> bool {
    const SIGNS: &[&str] = &[
        "error sending request",
        "dns error",
        "failed to lookup address",
        "name resolution",
        "connection refused",
        "network is unreachable",
        "no route to host",
        "tcp connect error",
        "could not reach youtube music",
    ];
    let error = error.to_ascii_lowercase();
    SIGNS.iter().any(|sign| error.contains(sign))
}

/// Record a refused write, so the server's own words survive.
///
/// The writes — rate, save-to-library, save-to-playlist, subscribe — are the paths that fail
/// with only a generic toast, and the reason (a rotated session, an endpoint YouTube no longer
/// accepts, a wrong id) lives in `ytmapi-rs`'s error, which carries InnerTube's `code` and
/// `message`. This prints the whole chain at `error` (no `RUST_LOG` tuning needed) and, when
/// `REFRYT_DUMP_WRITE` names a directory, writes one JSON file per operation to hand back.
fn note_write_failure(op: &str, id: &str, error: &(dyn std::error::Error + 'static)) {
    let mut chain = Vec::new();
    let mut current = Some(error);
    while let Some(e) = current {
        chain.push(e.to_string());
        current = e.source();
    }
    let full = chain.join(": ");
    tracing::error!(op, id, error = %full, "write refused by YouTube");
    let Ok(dir) = std::env::var("REFRYT_DUMP_WRITE") else {
        return;
    };
    let payload = serde_json::json!({ "op": op, "id": id, "error": full, "causes": chain });
    let path = std::path::Path::new(&dir).join(format!("refryt-write-{op}.json"));
    match serde_json::to_vec_pretty(&payload) {
        Ok(bytes) => {
            if let Err(e) = std::fs::write(&path, bytes) {
                tracing::warn!(error = %e, path = %path.display(), "could not write the failure dump");
            }
        }
        Err(e) => tracing::warn!(error = %e, "could not encode the failure dump"),
    }
}

/// The raw JSON for a search, so [`crate::innertube`] can read the shape `ytmapi-rs` cannot.
async fn run_search_json<A: AuthToken>(yt: &YtMusic<A>, query: &str) -> Result<serde_json::Value> {
    let sq: SearchQuery<'_, BasicSearch> = query.into();
    yt.json_query(sq)
        .await
        .map(ytmapi_rs::json::Json::into_inner)
        .with_context(|| format!("search failed for {query:?}"))
}

/// The raw JSON for a search under one of the result filters (Albums, Songs, …), for when the
/// typed parse of that filter fails.
async fn run_filtered_json<A, F>(yt: &YtMusic<A>, query: &str) -> Result<serde_json::Value>
where
    A: AuthToken,
    F: FilteredSearchType,
    for<'a> SearchQuery<'a, FilteredSearch<F>>: ytmapi_rs::query::Query<A> + From<&'a str>,
{
    let sq: SearchQuery<'_, FilteredSearch<F>> = query.into();
    yt.json_query(sq)
        .await
        .map(ytmapi_rs::json::Json::into_inner)
        .with_context(|| format!("search failed for {query:?}"))
}

/// The raw JSON for an artist page.
async fn run_artist_json<A: AuthToken>(yt: &YtMusic<A>, id: &str) -> Result<serde_json::Value> {
    yt.json_query(GetArtistQuery::new(ArtistChannelID::from_raw(
        id.to_owned(),
    )))
    .await
    .map(ytmapi_rs::json::Json::into_inner)
    .with_context(|| format!("could not load artist {id}"))
}

/// The raw JSON for a mood/genre chip's playlists.
async fn run_mood_playlists_json<A: AuthToken>(
    yt: &YtMusic<A>,
    params: &str,
) -> Result<serde_json::Value> {
    yt.json_query(ytmapi_rs::query::GetMoodPlaylistsQuery::new(
        ytmapi_rs::common::MoodCategoryParams::from_raw(params.to_owned()),
    ))
    .await
    .map(ytmapi_rs::json::Json::into_inner)
    .with_context(|| "could not load mood playlists".to_owned())
}

async fn run_artist<A: AuthToken>(yt: &YtMusic<A>, id: &str) -> Result<GetArtist> {
    yt.get_artist(ArtistChannelID::from_raw(id.to_owned()))
        .await
        .with_context(|| format!("could not load artist {id}"))
}

async fn run_album<A: AuthToken>(yt: &YtMusic<A>, id: &str) -> Result<GetAlbum> {
    yt.get_album(AlbumID::from_raw(id.to_owned()))
        .await
        .with_context(|| format!("could not load album {id}"))
}

/// The raw JSON for an artist expansion ("Show all"), so [`crate::innertube`] can read the
/// layouts `ytmapi-rs` walks past.
async fn run_artist_albums_json<A: AuthToken>(
    yt: &YtMusic<A>,
    artist_id: &str,
    params: &str,
) -> Result<serde_json::Value> {
    yt.json_query(GetArtistAlbumsQuery::new(
        ArtistChannelID::from_raw(artist_id.to_owned()),
        BrowseParams::from_raw(params.to_owned()),
    ))
    .await
    .map(ytmapi_rs::json::Json::into_inner)
    .with_context(|| format!("could not load albums for artist {artist_id}"))
}

async fn run_artist_albums<A: AuthToken>(
    yt: &YtMusic<A>,
    artist_id: &str,
    params: &str,
) -> Result<Vec<GetArtistAlbumsAlbum>> {
    yt.get_artist_albums(
        ArtistChannelID::from_raw(artist_id.to_owned()),
        BrowseParams::from_raw(params.to_owned()),
    )
    .await
    .with_context(|| format!("could not load albums for artist {artist_id}"))
}

/// The raw JSON for a watch queue (`next`), so [`crate::innertube`] can read the
/// `playlistPanelRenderer` the typed API has no query for.
async fn run_next_json<A: AuthToken>(
    yt: &YtMusic<A>,
    query: NextQuery,
) -> Result<serde_json::Value> {
    yt.json_query(query)
        .await
        .map(ytmapi_rs::json::Json::into_inner)
        .with_context(|| "could not load the watch queue".to_owned())
}

// ---------------------------------------------------------------------------------------------
// Mapping ytmapi-rs types into ours.
// ---------------------------------------------------------------------------------------------

fn last_thumb(thumbnails: &[ytmapi_rs::common::Thumbnail]) -> Option<Arc<str>> {
    thumbnails.last().map(|t| Arc::from(t.url.as_str()))
}

/// Fold the ratings and library tokens from a raw browse onto typed tracks, by video id.
///
/// Only fills gaps: a typed track that somehow arrived rated keeps what it had, so this can
/// never downgrade a known state to `Indifferent`.
fn merge_track_states(
    tracks: &mut [model::Track],
    states: &std::collections::HashMap<String, innertube::TrackState>,
) {
    for track in tracks {
        let Some(state) = states.get(&track.video_id) else {
            continue;
        };
        if track.like == model::LikeStatus::Indifferent {
            track.like = state.like;
        }
        track.in_library |= state.in_library;
        track.library_add = track
            .library_add
            .take()
            .or_else(|| state.library_add.clone());
        track.library_remove = track
            .library_remove
            .take()
            .or_else(|| state.library_remove.clone());
        track.set_video_id = track
            .set_video_id
            .take()
            .or_else(|| state.set_video_id.clone());
        // The typed album parser has no field for this at all, so it never has anything to
        // downgrade from — only ever learned here, never unlearned.
        track.wide |= state.wide;
    }
}

/// Every artist, each keeping its own browse id.
///
/// The typed parser already carries a name *and* an id per artist, so the fallback path gets
/// individually clickable artists for free — it used to join the names and keep only the first id.
fn artist_links(artists: &[ytmapi_rs::parse::ParsedSongArtist]) -> Vec<model::ArtistLink> {
    artists
        .iter()
        .map(|a| model::ArtistLink {
            name: a.name.clone(),
            id: a.id.as_ref().map(|id| id.get_raw().to_owned()),
        })
        .collect()
}

fn is_explicit(e: &Explicit) -> bool {
    matches!(e, Explicit::IsExplicit)
}

/// `"Album"`, `"Single"` or `"EP"`, as YouTube labels it.
fn album_kind(t: &AlbumType) -> &'static str {
    match t {
        AlbumType::Single => "Single",
        AlbumType::EP => "EP",
        _ => "Album",
    }
}

fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

fn map_song(s: SearchResultSong) -> Track {
    Track {
        video_id: s.video_id.get_raw().to_owned(),
        title: s.title,
        artists: vec![model::ArtistLink::plain(s.artist)],
        album: s.album.as_ref().map(|a| a.name.clone()),
        // The typed album carries its browse id too, so even this fallback path's title links to
        // the album.
        album_id: s.album.as_ref().map(|a| a.id.get_raw().to_owned()),
        duration: parse_duration(&s.duration),
        plays: non_empty(s.plays),
        explicit: is_explicit(&s.explicit),
        // The typed responses never carry the like button.
        like: model::LikeStatus::Indifferent,
        in_library: false,
        library_add: None,
        library_remove: None,
        set_video_id: None,
        // The typed parsers carry no video kind, so a typed track is never wide.
        wide: false,
        // The typed parsers carry no rank column either.
        rank: None,
        trend: crate::model::ChartTrend::Unknown,
        thumbnail: last_thumb(&s.thumbnails),
        local_path: None,
    }
}

fn map_artist_ref(a: ytmapi_rs::parse::SearchResultArtist) -> model::ArtistRef {
    model::ArtistRef {
        id: a.browse_id.get_raw().to_owned(),
        name: a.artist,
        subscribers: a.subscribers,
        actions: model::ArtistActions::default(),
        thumbnail: last_thumb(&a.thumbnails),
    }
}

fn map_album_ref(a: ytmapi_rs::parse::SearchResultAlbum) -> model::AlbumRef {
    model::AlbumRef {
        id: a.album_id.get_raw().to_owned(),
        title: a.title,
        artist: a.artist,
        year: (!a.year.is_empty()).then_some(a.year),
        kind: Some(album_kind(&a.album_type).to_owned()),
        thumbnail: last_thumb(&a.thumbnails),
    }
}

/// `None` for a `VideoEpisode` — a podcast episode's search result, which this app has no page
/// for (the same reason [`innertube::parse_search_row`] drops one off the ranked list).
fn map_video_ref(v: ytmapi_rs::parse::SearchResultVideo) -> Option<Track> {
    let ytmapi_rs::parse::SearchResultVideo::Video {
        title,
        channel_name,
        video_id,
        views,
        length,
        thumbnails,
        ..
    } = v
    else {
        return None;
    };
    Some(Track {
        video_id: video_id.get_raw().to_owned(),
        title,
        artists: vec![model::ArtistLink::plain(channel_name)],
        album: None,
        album_id: None,
        duration: parse_duration(&length),
        plays: non_empty(views),
        explicit: false,
        like: model::LikeStatus::Indifferent,
        in_library: false,
        library_add: None,
        library_remove: None,
        set_video_id: None,
        // A dedicated video search result is always a real video, never the plain
        // album-audio kind — there is no "song" filter mixed into this one.
        wide: true,
        rank: None,
        trend: crate::model::ChartTrend::Unknown,
        thumbnail: last_thumb(&thumbnails),
        local_path: None,
    })
}

/// `None` for a `Podcast` — not a playlist this app has a page for.
fn map_playlist_ref(p: ytmapi_rs::parse::SearchResultPlaylist) -> Option<model::PlaylistRef> {
    match p {
        ytmapi_rs::parse::SearchResultPlaylist::Featured(f) => Some(model::PlaylistRef {
            id: f.playlist_id.get_raw().to_owned(),
            title: f.title,
            author: f.author,
            author_id: None,
            track_count: non_empty(f.songs),
            thumbnail: last_thumb(&f.thumbnails),
            editable: false,
            source: model::PlaylistSource::YouTube,
        }),
        ytmapi_rs::parse::SearchResultPlaylist::Community(c) => Some(model::PlaylistRef {
            id: c.playlist_id.get_raw().to_owned(),
            title: c.title,
            author: c.author,
            author_id: None,
            // `views`, not a song count — the community-playlist result carries no track total.
            track_count: None,
            thumbnail: last_thumb(&c.thumbnails),
            editable: false,
            source: model::PlaylistSource::YouTube,
        }),
        ytmapi_rs::parse::SearchResultPlaylist::Podcast(_) => None,
        // Non-exhaustive upstream enum; no other variant exists as of ytmapi-rs 0.3.3.
        _ => None,
    }
}

fn map_artist(a: GetArtist) -> model::Artist {
    let artist_thumb = last_thumb(&a.thumbnails);
    let top_songs = a
        .top_releases
        .songs
        .map(|s| {
            s.results
                .into_iter()
                .map(|song| Track {
                    video_id: song.video_id.get_raw().to_owned(),
                    title: song.title,
                    artists: artist_links(&song.artists),
                    // ArtistSong carries no duration; none is better than a fake one.
                    duration: None,
                    album: Some(song.album.name),
                    album_id: Some(song.album.id.get_raw().to_owned()),
                    plays: non_empty(song.plays),
                    explicit: is_explicit(&song.explicit),
                    like: model::LikeStatus::Indifferent,
                    in_library: false,
                    library_add: None,
                    library_remove: None,
                    set_video_id: None,
                    // The typed parsers carry no video kind, so a typed track is never wide.
                    wide: false,
                    rank: None,
                    trend: crate::model::ChartTrend::Unknown,
                    thumbnail: artist_thumb.clone(),
                    local_path: None,
                })
                .collect()
        })
        .unwrap_or_default();

    // The typed fallback knows only these two shelves; the raw parser keeps all of YouTube's.
    let mut shelves = Vec::new();
    for (title, section) in [
        ("Albums", a.top_releases.albums),
        ("Singles & EPs", a.top_releases.singles),
    ] {
        let Some(section) = section else { continue };
        let more_params = section.params.as_ref().map(|p| p.get_raw().to_owned());
        let items: Vec<model::Entry> = section
            .results
            .into_iter()
            .map(|al| {
                model::Entry::Album(model::AlbumRef {
                    id: al.album_id.get_raw().to_owned(),
                    title: al.title,
                    artist: a.name.clone(),
                    year: (!al.year.is_empty()).then_some(al.year),
                    // The preview's own type label is deprecated upstream; the full-list
                    // expansion carries the category string instead.
                    kind: None,
                    thumbnail: last_thumb(&al.thumbnails),
                })
            })
            .collect();
        if !items.is_empty() {
            shelves.push(model::Shelf {
                strapline: None,
                title: title.to_owned(),
                items,
                more_params,
                // The typed API exposes no browse id for the continuation, only `params`;
                // `expand_artist_shelf` falls back to the artist's own id when this is `None`.
                more_browse_id: None,
                expanded: false,
            });
        }
    }

    model::Artist {
        id: a.channel_id.get_raw().to_owned(),
        name: a.name,
        subscribers: a.subscribers.clone(),
        // The typed fallback carries one count and no way to tell short from long; it serves as
        // the sentence when the raw parser never ran, and the button loses its count rather than
        // risk repeating it (see `ui::artist`).
        subscribers_long: a.subscribers,
        // The typed fallback carries neither, so the header simply shows less rather than
        // inventing something.
        monthly_listeners: None,
        actions: model::ArtistActions::default(),
        views: a.views,
        description: a.description,
        thumbnail: artist_thumb,
        // The typed fallback's thumbnails are square portraits, never the wide banner.
        hero: None,
        top_songs,
        // The typed fallback has no shelf JSON to read either the title or the "Show all"
        // target from; the page falls back to its own "Popular" heading.
        top_songs_title: None,
        top_songs_playlist: None,
        shelves: std::sync::Arc::new(shelves),
    }
}

fn map_album(id: &str, a: GetAlbum) -> model::Album {
    let links = artist_links(&a.artists);
    let artist = links
        .iter()
        .map(|l| l.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let artist_id = links.iter().find_map(|l| l.id.clone());
    let thumb = last_thumb(&a.thumbnails);
    let tracks = a
        .tracks
        .into_iter()
        .map(|t| Track {
            video_id: t.video_id.get_raw().to_owned(),
            title: t.title,
            // The album's artists, each linked; the page's own rows refine this per track.
            artists: links.clone(),
            album: Some(a.title.clone()),
            // Every track on an album page belongs to the album being shown.
            album_id: Some(id.to_owned()),
            duration: parse_duration(&t.duration),
            plays: non_empty(t.plays),
            explicit: is_explicit(&t.explicit),
            like: model::LikeStatus::Indifferent,
            in_library: false,
            library_add: None,
            library_remove: None,
            set_video_id: None,
            // The typed parser carries no video kind at all — `get_album`'s raw-browse pass
            // corrects this afterward via `merge_track_states`, so this default only survives
            // for a track that pass couldn't resolve.
            wide: false,
            rank: None,
            trend: crate::model::ChartTrend::Unknown,
            thumbnail: thumb.clone(),
            local_path: None,
        })
        .collect();

    model::Album {
        id: id.to_owned(),
        title: a.title,
        artist,
        artist_id,
        artists: links,
        year: (!a.year.is_empty()).then_some(a.year),
        kind: album_kind(&a.category).to_owned(),
        description: a.description,
        thumbnail: thumb,
        duration_text: (!a.duration.is_empty()).then_some(a.duration),
        // The typed query keeps the header toggle's state icon but drops its endpoints; the
        // flip rides the like endpoint against the play button's playlist id instead.
        in_library: matches!(
            a.library_status,
            ytmapi_rs::common::LibraryStatus::InLibrary
        ),
        like_id: a.audio_playlist_id.map(|p| p.get_raw().to_owned()),
        tracks,
    }
}

fn map_playlist_song(s: PlaylistSong) -> Track {
    Track {
        video_id: s.video_id.get_raw().to_owned(),
        title: s.title,
        artists: artist_links(&s.artists),
        album: Some(s.album.name),
        // The typed album carries its browse id, so a playlist row's title links too.
        album_id: Some(s.album.id.get_raw().to_owned()),
        duration: parse_duration(&s.duration),
        plays: None,
        explicit: is_explicit(&s.explicit),
        like: model::LikeStatus::Indifferent,
        in_library: false,
        library_add: None,
        library_remove: None,
        set_video_id: None,
        // The typed parsers carry no video kind, so a typed track is never wide.
        wide: false,
        // The typed parsers carry no rank column either.
        rank: None,
        trend: crate::model::ChartTrend::Unknown,
        thumbnail: last_thumb(&s.thumbnails),
        local_path: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A refused sign-in names what YouTube showed instead of its music page.
    #[test]
    fn a_refused_session_is_told_by_the_page_it_landed_on() {
        let refusal = |url, page| Refusal::from_page(url, page);
        assert_eq!(
            refusal("https://accounts.google.com/ServiceLogin", ""),
            Refusal::Google
        );
        assert_eq!(
            refusal("https://consent.youtube.com/m", ""),
            Refusal::Consent
        );
        assert_eq!(
            refusal("https://www.google.com/sorry/index", ""),
            Refusal::Robot
        );
        assert_eq!(
            refusal(
                "https://music.youtube.com/",
                "Our systems have detected unusual traffic"
            ),
            Refusal::Robot
        );
        assert_eq!(
            refusal(
                "https://music.youtube.com/",
                "ytcfg INNERTUBE_CLIENT_VERSION"
            ),
            Refusal::Other
        );
    }

    /// An empty shell — what a private playlist looks like without a recognised session —
    /// is detectable, so it gets a plain error instead of the typed parser's key error.
    #[test]
    fn an_empty_shell_counts_as_empty() {
        assert!(Api::contents_empty(&serde_json::json!({"contents": {}})));
        assert!(Api::contents_empty(
            &serde_json::json!({"responseContext": {}})
        ));
        assert!(!Api::contents_empty(
            &serde_json::json!({"contents": {"twoColumnBrowseResultsRenderer": {}}})
        ));
    }

    /// The typed parsers carry no `likeButtonRenderer`, so an album or playlist track arrives
    /// unrated and a song you had liked did not look liked once it was playing. The raw browse
    /// of the same id does carry it, and this is the fold — which must never overwrite a state
    /// that is already known, or a fresh optimistic rating would be undone by a stale merge.
    #[test]
    fn merging_fills_gaps_and_downgrades_nothing() {
        let mut tracks = vec![
            Track {
                video_id: "unrated".into(),
                ..sample_track()
            },
            Track {
                video_id: "already-liked".into(),
                like: model::LikeStatus::Like,
                ..sample_track()
            },
            Track {
                video_id: "not-in-the-rows".into(),
                ..sample_track()
            },
        ];
        let states = std::collections::HashMap::from([
            (
                "unrated".to_owned(),
                innertube::TrackState {
                    like: model::LikeStatus::Like,
                    in_library: true,
                    library_remove: Some("token".into()),
                    ..Default::default()
                },
            ),
            (
                "already-liked".to_owned(),
                innertube::TrackState {
                    like: model::LikeStatus::Indifferent,
                    ..Default::default()
                },
            ),
        ]);

        merge_track_states(&mut tracks, &states);

        assert_eq!(tracks[0].like, model::LikeStatus::Like);
        assert!(tracks[0].in_library);
        assert_eq!(tracks[0].library_remove.as_deref(), Some("token"));
        assert_eq!(tracks[1].like, model::LikeStatus::Like);
        assert_eq!(tracks[2].like, model::LikeStatus::Indifferent);
    }

    fn sample_track() -> Track {
        Track {
            video_id: String::new(),
            title: String::new(),
            artists: Vec::new(),
            album: None,
            album_id: None,
            duration: None,
            plays: None,
            explicit: false,
            like: model::LikeStatus::Indifferent,
            in_library: false,
            library_add: None,
            library_remove: None,
            set_video_id: None,
            thumbnail: None,
            wide: false,
            rank: None,
            trend: crate::model::ChartTrend::Unknown,
            local_path: None,
        }
    }

    /// `X-Goog-PageId` on its own does not select a brand channel — it de-authenticates the
    /// request, so the library comes back as its signed-out empty state and the app concludes the
    /// cookie is dead. The two headers must always travel together.
    #[test]
    fn a_brand_account_sends_both_headers() {
        let headers = default_headers(Some("104195520750316828690"));
        assert_eq!(headers["x-goog-pageid"], "104195520750316828690");
        assert_eq!(headers["x-goog-authuser"], "0");
    }

    /// The personal channel is the absence of both, not `AuthUser` on its own — that is the
    /// request shape that has always worked and it stays byte-identical.
    #[test]
    fn the_personal_account_sends_neither() {
        for page_id in [None, Some("")] {
            let headers = default_headers(page_id);
            assert!(headers.get("x-goog-pageid").is_none());
            assert!(headers.get("x-goog-authuser").is_none());
        }
    }

    /// A page "Start mix" seeds from the playlist, so its id is `RDAMPL` + the *bare* id (never
    /// the `VL`-prefixed browse id) with `wAEB` asking `next` to randomise it — the same shape
    /// `mix_for` uses for a song, and the convention YouTube Music's own radio follows.
    #[test]
    fn playlist_radio_uses_the_bare_id_and_wants_a_random_mix() {
        let vl = NextQuery::playlist_radio("VLPLabc");
        let body = ytmapi_rs::query::PostQuery::header(&vl);
        assert_eq!(
            body.get("playlistId").and_then(|v| v.as_str()),
            Some("RDAMPLPLabc")
        );
        assert_eq!(body.get("params").and_then(|v| v.as_str()), Some("wAEB"));
        assert!(body.get("videoId").is_none());

        // An album is seeded by its audio playlist id, which passes through untouched.
        let album = NextQuery::playlist_radio("OLAK5uy_x");
        let body = ytmapi_rs::query::PostQuery::header(&album);
        assert_eq!(
            body.get("playlistId").and_then(|v| v.as_str()),
            Some("RDAMPLOLAK5uy_x")
        );
    }

    /// The subscribe body is the channel id plus the page button's own token when there was one
    /// — the token is what makes some channels accept the write — and nothing else. Without a
    /// token it stays the bare body it always sent, so a page that carried none is no worse off.
    #[test]
    fn the_subscribe_body_carries_the_buttons_params_when_there_is_one() {
        let with = SubscribeQuery {
            channel_id: "UC1".into(),
            subscribe: true,
            params: Some("EgIIGxgA".into()),
        };
        let body = ytmapi_rs::query::PostQuery::header(&with);
        assert_eq!(
            body.get("channelIds"),
            Some(&serde_json::json!(["UC1"])),
            "the channel id went missing"
        );
        assert_eq!(
            body.get("params").and_then(|v| v.as_str()),
            Some("EgIIGxgA")
        );
        assert_eq!(body.len(), 2, "unexpected fields in the body");

        let without = SubscribeQuery {
            channel_id: "UC1".into(),
            subscribe: false,
            params: None,
        };
        let body = ytmapi_rs::query::PostQuery::header(&without);
        assert_eq!(
            body.len(),
            1,
            "no token means the bare body, exactly as before"
        );
    }
}
