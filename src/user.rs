use std::collections::HashMap;

use actix_web::{get, post, services, web, HttpResponse, Scope};
use askama::Template;
use foxlib::jobs::FaktoryProducer;
use futures::TryStreamExt;
use rand::Rng;
use serde::Deserialize;
use serde_with::{serde_as, NoneAsEmptyString};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;

use crate::{
    auth::{self, FuzzySearchSessionToken},
    common,
    jobs::{self, JobInitiatorExt},
    models::{self, setting, Site, UserSettingItem},
    routes::*,
    AddFlash, ClientIpAddr, Error, FlashStyle, WrappedTemplate,
};

#[derive(Template)]
#[template(path = "user/index.html")]
struct Home<'a> {
    user: &'a models::User,

    item_count: i64,
    total_content_size: i64,

    recent_media: Vec<models::OwnedMediaItem>,
    monitored_accounts: Vec<models::LinkedAccount>,
}

#[get("/home")]
async fn home(
    request: actix_web::HttpRequest,
    conn: web::Data<sqlx::PgPool>,
    user: models::User,
) -> Result<HttpResponse, Error> {
    let user_item_count = models::OwnedMediaItem::user_item_count(&conn, user.id);
    let recent_media = models::OwnedMediaItem::recent_media(&conn, user.id, None);
    let monitored_accounts = models::LinkedAccount::owned_by_user(&conn, user.id);

    let ((item_count, total_content_size), recent_media, monitored_accounts) =
        futures::try_join!(user_item_count, recent_media, monitored_accounts)?;

    let body = Home {
        user: &user,
        item_count,
        total_content_size,
        recent_media,
        monitored_accounts,
    }
    .wrap(&request, Some(&user))
    .await
    .render()?;

    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[derive(Template)]
#[template(path = "user/settings.html")]
struct Settings<'a> {
    telegram_login: &'a auth::TelegramLoginConfig,

    user: &'a models::User,
    saved_message: Option<(bool, &'a str)>,

    email_frequency: String,
    telegram_notifications: setting::TelegramNotifications,
}

#[get("/settings")]
async fn settings_get(
    request: actix_web::HttpRequest,
    telegram_login: web::Data<auth::TelegramLoginConfig>,
    conn: web::Data<sqlx::PgPool>,
    user: models::User,
) -> Result<HttpResponse, Error> {
    let email_frequency = models::UserSetting::get::<setting::EmailFrequency>(&conn, user.id)
        .await?
        .unwrap_or_default();

    let telegram_notifications = models::UserSetting::get(&conn, user.id)
        .await?
        .unwrap_or_default();

    let body = Settings {
        telegram_login: &telegram_login,

        user: &user,
        saved_message: None,

        email_frequency: serde_plain::to_string(&email_frequency.0)?,
        telegram_notifications,
    }
    .wrap(&request, Some(&user))
    .await
    .render()?;

    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[post("/settings")]
async fn settings_post(
    request: actix_web::HttpRequest,
    telegram_login: web::Data<auth::TelegramLoginConfig>,
    conn: web::Data<sqlx::PgPool>,
    faktory: web::Data<FaktoryProducer>,
    user: models::User,
    form: web::Form<HashMap<String, String>>,
) -> Result<HttpResponse, Error> {
    tracing::trace!("got settings: {:?}", form);

    let email_frequency = form
        .get("email-frequency")
        .and_then(|value| {
            serde_plain::from_str(value)
                .ok()
                .map(models::setting::EmailFrequency)
        })
        .unwrap_or_else(models::UserSettingItem::off_value);

    let telegram_notifications = form
        .get("telegram-notifications")
        .map(|value| setting::TelegramNotifications(value == "on"))
        .unwrap_or_else(models::UserSettingItem::off_value);

    let display_name = match form.get("display_name") {
        Some(display_name) if !display_name.is_empty() => Some(display_name.as_str()),
        _ => None,
    };

    futures::try_join!(
        models::UserSetting::set(&conn, user.id, &email_frequency),
        models::UserSetting::set(&conn, user.id, &telegram_notifications),
        models::User::update_display_name(&conn, user.id, display_name)
    )?;

    let email_address = match form.get("email") {
        None => None,
        Some(email) if email.is_empty() => None,
        Some(email) => Some(email),
    };

    let email_was_updated = email_address != user.email.as_ref().filter(|s| !s.is_empty());

    let error_saving_email = match email_address {
        Some(email_address) if email_was_updated => {
            if !models::User::email_exists(&conn, email_address).await? {
                models::User::set_email(&conn, user.id, email_address).await?;

                faktory
                    .enqueue_job(
                        jobs::EmailVerificationJob { user_id: user.id }
                            .initiated_by(jobs::JobInitiator::user(user.id)),
                    )
                    .await?;

                false
            } else {
                true
            }
        }
        _ => false,
    };

    let user = models::User::lookup_by_id(&conn, user.id)
        .await?
        .ok_or(Error::Missing)?;

    let (successful, saved_message) = if error_saving_email {
        (false, "Your email address could not be saved.")
    } else if email_was_updated {
        if email_address.is_some() {
            (
                true,
                "Your email address was updated, please verify your new email address.",
            )
        } else {
            (true, "Your email address was removed.")
        }
    } else {
        (true, "Your settings were saved.")
    };

    let body = Settings {
        telegram_login: &telegram_login,

        user: &user,
        saved_message: Some((successful, saved_message)),

        email_frequency: serde_plain::to_string(&email_frequency.0)?,
        telegram_notifications,
    }
    .wrap(&request, Some(&user))
    .await
    .render()?;

    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[post("/delete")]
async fn delete(
    conn: web::Data<sqlx::PgPool>,
    session: actix_session::Session,
    user: models::User,
) -> Result<HttpResponse, Error> {
    models::User::delete(&conn, user.id).await?;
    session.purge();

    return Ok(HttpResponse::Found()
        .insert_header(("Location", "/"))
        .finish());
}

#[get("/events")]
async fn events(
    conn: web::Data<sqlx::PgPool>,
    user: models::User,
) -> Result<web::Json<Vec<models::UserEvent>>, Error> {
    let events = models::UserEvent::recent_events(&conn, user.id).await?;

    Ok(web::Json(events))
}

#[post("/single")]
async fn single(
    conn: web::Data<sqlx::PgPool>,
    redis: web::Data<redis::aio::ConnectionManager>,
    s3: web::Data<rusoto_s3::S3Client>,
    faktory: web::Data<FaktoryProducer>,
    config: web::Data<crate::Config>,
    session: actix_session::Session,
    user: models::User,
    form: actix_multipart::Multipart,
) -> Result<HttpResponse, Error> {
    let _ids =
        common::handle_multipart_upload(&conn, &redis, &s3, &faktory, &config, &user, form).await?;

    session.add_flash(FlashStyle::Success, "Uploaded image.");

    Ok(HttpResponse::Found()
        .insert_header(("Location", USER_HOME))
        .finish())
}

#[derive(Template)]
#[template(path = "user/check_form.html")]
struct CheckForm;

#[get("/check")]
async fn check_get(
    request: actix_web::HttpRequest,
    user: models::User,
) -> Result<HttpResponse, Error> {
    let body = CheckForm.wrap(&request, Some(&user)).await.render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

struct CheckLink {
    url: String,
    site: models::Site,
    username: String,
    posted_at: Option<chrono::DateTime<chrono::Utc>>,
}

struct CheckResult {
    /// Base-64 encoded image preview
    photo_preview: String,
    links: Vec<CheckLink>,
}

#[derive(Template)]
#[template(path = "user/check_results.html")]
struct CheckResults {
    results: Vec<CheckResult>,
}

#[post("/check")]
async fn check_post(
    conn: web::Data<sqlx::PgPool>,
    request: actix_web::HttpRequest,
    user: models::User,
    mut form: actix_multipart::Multipart,
) -> Result<HttpResponse, Error> {
    let mut results = Vec::new();

    while let Ok(Some(mut field)) = form.try_next().await {
        tracing::trace!("checking multipart field: {:?}", field);

        if !matches!(field.content_disposition().get_name(), Some("image")) {
            continue;
        }

        let mut file = tokio::task::spawn_blocking(move || -> Result<_, String> {
            let file = tempfile::tempfile().map_err(|err| err.to_string())?;
            Ok(tokio::fs::File::from_std(file))
        })
        .await
        .map_err(Error::from_displayable)?
        .map_err(|err| Error::UnknownMessage(err.into()))?;

        let mut size = 0;
        while let Ok(Some(chunk)) = field.try_next().await {
            if size > 25_000_000 {
                return Err(Error::TooLarge(size));
            }

            file.write_all(&chunk).await?;
            size += chunk.len();
        }

        if size == 0 {
            continue;
        }

        tracing::info!(size, "received complete file from client");

        file.seek(std::io::SeekFrom::Start(0)).await?;

        let file = file.into_std().await;
        let (perceptual_hash, thumbnail) = tokio::task::spawn_blocking(
            move || -> Result<([u8; 8], Vec<u8>), image::ImageError> {
                let reader = std::io::BufReader::new(file);
                let reader = image::io::Reader::new(reader).with_guessed_format()?;
                let im = reader.decode()?;

                let hasher = fuzzysearch_common::get_hasher();
                let hash = hasher.hash_image(&im);
                let hash: [u8; 8] = hash
                    .as_bytes()
                    .try_into()
                    .expect("perceptual hash returned wrong bytes");

                let thumbnail = im.thumbnail(128, 128);
                let mut buf = Vec::new();
                thumbnail.write_to(&mut buf, image::ImageOutputFormat::Png)?;

                Ok((hash, buf))
            },
        )
        .await
        .map_err(|_err| Error::unknown_message("join error"))??;

        let accounts: HashMap<Uuid, models::LinkedAccount> =
            models::LinkedAccount::owned_by_user(&conn, user.id)
                .await?
                .into_iter()
                .map(|account| (account.id, account))
                .collect();

        let links = models::OwnedMediaItem::find_similar_with_owner(
            &conn,
            user.id,
            i64::from_be_bytes(perceptual_hash),
        )
        .await?
        .into_iter()
        .filter_map(|media| {
            let account = accounts.get(&media.account_id?)?;

            Some(CheckLink {
                url: media.link?,
                site: account.source_site,
                username: account.username.clone(),
                posted_at: media.posted_at,
            })
        })
        .collect();

        results.push(CheckResult {
            photo_preview: base64::encode(thumbnail),
            links,
        });
    }

    let body = CheckResults { results }
        .wrap(&request, Some(&user))
        .await
        .render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[derive(Template)]
#[template(path = "user/account/link.html")]
struct AccountLink;

#[get("/link")]
async fn account_link_get(
    request: actix_web::HttpRequest,
    user: models::User,
) -> Result<HttpResponse, Error> {
    if !user.has_verified_account() {
        return Err(Error::user_error(
            "You must verify your email address first.",
        ));
    }

    let body = AccountLink.wrap(&request, Some(&user)).await.render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[derive(Deserialize)]
struct AccountLinkForm {
    site: models::Site,
    username: Option<String>,
}

#[post("/link")]
async fn account_link_post(
    config: web::Data<crate::Config>,
    conn: web::Data<sqlx::PgPool>,
    session: actix_session::Session,
    user: models::User,
    form: web::Form<AccountLinkForm>,
) -> Result<HttpResponse, Error> {
    if !user.has_verified_account() {
        return Err(Error::user_error(
            "You must verify your email address first.",
        ));
    }

    if let Ok(Some(collected_site)) = form.site.collected_site(&config).await {
        if let Some(location) = collected_site.oauth_page() {
            return Ok(HttpResponse::Found()
                .insert_header(("Location", location))
                .finish());
        }
    }

    let username = form.username.as_deref().ok_or(Error::Missing)?;

    let token: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(12)
        .map(char::from)
        .collect();

    let data = match form.site {
        Site::FurAffinity => Some(serde_json::json!({ "verification_key": token })),
        Site::Weasyl => Some(serde_json::json!({ "verification_key": token })),
        _ => None,
    };

    let account = models::LinkedAccount::create(&conn, user.id, form.site, username, data).await?;

    let (style, message) = if account.verification_key().is_some() {
        (
            FlashStyle::Warning,
            "Added account, please verify it to import submissions.",
        )
    } else {
        (
            FlashStyle::Success,
            "Added account, now importing submissions.",
        )
    };

    session.add_flash(style, message);

    Ok(HttpResponse::Found()
        .insert_header(("Location", format!("/user/account/{}", account.id)))
        .finish())
}

#[derive(Deserialize)]
struct AccountIdForm {
    account_id: Uuid,
}

#[post("/remove")]
async fn account_remove(
    conn: web::Data<sqlx::PgPool>,
    session: actix_session::Session,
    user: models::User,
    form: web::Form<AccountIdForm>,
) -> Result<HttpResponse, Error> {
    models::LinkedAccount::remove(&conn, user.id, form.account_id).await?;

    session.add_flash(
        FlashStyle::Success,
        "Removed account and associated submissions.",
    );

    Ok(HttpResponse::Found()
        .insert_header(("Location", USER_HOME))
        .finish())
}

#[derive(Template)]
#[template(path = "user/account/view.html")]
struct AccountView {
    account: models::LinkedAccount,

    item_count: i64,
    total_content_size: i64,

    recent_media: Vec<models::OwnedMediaItem>,
}

#[get("/{account_id}")]
async fn account_view(
    request: actix_web::HttpRequest,
    conn: web::Data<sqlx::PgPool>,
    path: web::Path<(Uuid,)>,
    user: models::User,
) -> Result<HttpResponse, Error> {
    let account_id: Uuid = path.into_inner().0;

    let account = models::LinkedAccount::lookup_by_id(&conn, account_id)
        .await?
        .ok_or(Error::Missing)?;

    if account.owner_id != user.id {
        return Err(Error::Missing);
    }

    let (item_count, total_content_size) =
        models::LinkedAccount::items(&conn, user.id, account_id).await?;

    let recent_media =
        models::OwnedMediaItem::recent_media(&conn, user.id, Some(account_id)).await?;

    let body = AccountView {
        account,
        item_count,
        total_content_size,
        recent_media,
    }
    .wrap(&request, Some(&user))
    .await
    .render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[post("/verify")]
async fn account_verify(
    conn: web::Data<sqlx::PgPool>,
    faktory: web::Data<FaktoryProducer>,
    user: models::User,
    form: web::Form<AccountIdForm>,
) -> Result<HttpResponse, Error> {
    let account = models::LinkedAccount::lookup_by_id(&conn, form.account_id)
        .await?
        .ok_or(Error::Missing)?;

    if account.owner_id != user.id {
        return Err(Error::Missing);
    }

    faktory
        .enqueue_job(
            jobs::VerifyAccountJob {
                user_id: user.id,
                account_id: account.id,
            }
            .initiated_by(jobs::JobInitiator::user(user.id)),
        )
        .await?;

    Ok(HttpResponse::Found()
        .insert_header(("Location", format!("/user/account/{}", account.id)))
        .finish())
}

#[derive(Template)]
#[template(path = "user/media/view.html")]
struct MediaView<'a> {
    url: String,
    media: models::OwnedMediaItem,
    similar_image_events: &'a [(chrono::DateTime<chrono::Utc>, models::SimilarImage)],
    other_events: &'a [models::UserEvent],
    allowlisted_users: HashMap<(Site, String), Uuid>,
}

#[get("/view/{media_id}")]
async fn media_view(
    request: actix_web::HttpRequest,
    conn: web::Data<sqlx::PgPool>,
    path: web::Path<(Uuid,)>,
    user: models::User,
) -> Result<HttpResponse, Error> {
    let media = models::OwnedMediaItem::get_by_id(&conn, path.0, user.id)
        .await?
        .ok_or(Error::Missing)?;

    let recent_events =
        models::UserEvent::recent_events_for_media(&conn, user.id, media.id).await?;

    let (similar_events, other_events): (Vec<_>, Vec<_>) = recent_events
        .into_iter()
        .partition(|event| matches!(event.data, Some(models::UserEventData::SimilarImage(_))));

    let similar_events: Vec<_> = similar_events
        .into_iter()
        .filter_map(|event| match event.data {
            Some(models::UserEventData::SimilarImage(similar)) => Some((event.created_at, similar)),
            _ => None,
        })
        .collect();

    let site_user_pairs = similar_events.iter().flat_map(|(_time, similar)| {
        similar
            .posted_by
            .as_deref()
            .map(|posted_by| (similar.site, posted_by))
    });

    let allowlisted_users =
        models::UserAllowlist::lookup_many(&conn, user.id, site_user_pairs).await?;

    let body = MediaView {
        url: request.uri().to_string(),
        media,
        similar_image_events: &similar_events,
        other_events: &other_events,
        allowlisted_users,
    }
    .wrap(&request, Some(&user))
    .await
    .render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[derive(Deserialize)]
struct MediaRemoveForm {
    media_id: Uuid,
}

#[post("/remove")]
async fn media_remove(
    conn: web::Data<sqlx::PgPool>,
    session: actix_session::Session,
    user: models::User,
    form: web::Form<MediaRemoveForm>,
) -> Result<HttpResponse, Error> {
    models::OwnedMediaItem::remove(&conn, user.id, form.media_id).await?;

    session.add_flash(FlashStyle::Success, "Removed media.");

    Ok(HttpResponse::Found()
        .insert_header(("Location", USER_HOME))
        .finish())
}

#[derive(Debug, Deserialize)]
struct MediaListQuery {
    account_id: Option<Uuid>,
    page: Option<u32>,
    sort: Option<models::MediaListSort>,
}

fn decode_query(query: &str) -> HashMap<&str, &str> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .collect()
}

fn encode_query(params: &HashMap<&str, &str>) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&")
}

pub struct PaginationData<'a> {
    uri: &'a actix_web::http::Uri,
    params: HashMap<&'a str, &'a str>,

    pub page_size: u32,
    pub item_count: u32,

    pub current_page: u32,
}

impl<'a> PaginationData<'a> {
    pub fn new(
        uri: &'a actix_web::http::Uri,
        page_size: u32,
        item_count: u32,
        current_page: u32,
    ) -> Self {
        let params = decode_query(uri.query().unwrap_or_default());

        Self {
            uri,
            params,

            page_size,
            item_count,
            current_page,
        }
    }

    pub fn url<P: std::fmt::Display>(&self, page: P) -> String {
        let page = page.to_string();

        let mut params = self.params.clone();
        params.insert("page", &page);

        format!("{}?{}", self.uri.path(), encode_query(&params))
    }

    pub fn last_page(&self) -> u32 {
        self.item_count.saturating_sub(1) / self.page_size
    }

    pub fn previous_page(&self) -> Option<u32> {
        if self.current_page == 0 {
            None
        } else {
            Some(self.current_page - 1)
        }
    }

    pub fn next_page(&self) -> Option<u32> {
        if (self.current_page + 1) * 25 >= self.item_count {
            None
        } else {
            Some(self.current_page + 1)
        }
    }

    pub fn display_lower(&self) -> u32 {
        std::cmp::max(1, self.current_page.saturating_sub(2))
    }

    pub fn display_upper(&self) -> u32 {
        std::cmp::min(self.last_page(), self.current_page + 3)
    }
}

#[derive(Template)]
#[template(path = "user/media/list.html")]
struct MediaList<'a> {
    media: &'a [models::OwnedMediaItem],
    sort: &'a str,
    pagination_data: PaginationData<'a>,
    account: Option<models::LinkedAccount>,
}

#[get("/list")]
async fn media_list(
    request: actix_web::HttpRequest,
    conn: web::Data<sqlx::PgPool>,
    user: models::User,
    query: web::Query<MediaListQuery>,
) -> Result<HttpResponse, Error> {
    let page = query.page.unwrap_or(0);
    let sort = query.sort.unwrap_or(models::MediaListSort::Recent);

    let media =
        models::OwnedMediaItem::media_page(&conn, user.id, page, sort, query.account_id).await?;

    let account = if let Some(account_id) = query.account_id {
        match models::LinkedAccount::lookup_by_id(&conn, account_id).await? {
            Some(account) if account.owner_id == user.id => Some(account),
            _ => return Err(Error::Missing),
        }
    } else {
        None
    };

    let count = models::OwnedMediaItem::count(&conn, user.id).await?;
    let pagination_data = PaginationData::new(request.uri(), 25, count as u32, page);

    let body = MediaList {
        media: &media,
        sort: sort.name(),
        pagination_data,
        account,
    }
    .wrap(&request, Some(&user))
    .await
    .render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[serde_as]
#[derive(Debug, Deserialize)]
struct FeedQuery {
    page: Option<u32>,

    #[serde_as(as = "NoneAsEmptyString")]
    #[serde(default)]
    site: Option<models::Site>,
    #[serde(default, deserialize_with = "deserialize_checkbox")]
    filter_allowlisted: bool,
}

fn deserialize_checkbox<'de, D: serde::de::Deserializer<'de>>(
    deserializer: D,
) -> Result<bool, D::Error> {
    let s: &str = serde::de::Deserialize::deserialize(deserializer)?;

    match s {
        "on" => Ok(true),
        _ => Err(serde::de::Error::unknown_variant(s, &["on"])),
    }
}

#[derive(Template)]
#[template(path = "user/feed.html")]
struct Feed<'a> {
    entries: &'a [models::EventAndRelatedMedia],

    visible_sites: &'a [String],
    site: Option<&'a str>,
    filtering_allowlisted: bool,

    pagination_data: PaginationData<'a>,
}

#[get("/feed")]
async fn feed(
    request: actix_web::HttpRequest,
    conn: web::Data<sqlx::PgPool>,
    user: models::User,
    query: web::Query<FeedQuery>,
) -> Result<HttpResponse, Error> {
    let page = query.page.unwrap_or(0);
    let site = query.site.map(|site| site.to_string());

    let visible_sites = models::Site::visible_sites();

    let count = models::UserEvent::count(
        &conn,
        user.id,
        Some("similar_image"),
        query.site,
        query.filter_allowlisted,
    )
    .await?;
    let pagination_data = PaginationData::new(request.uri(), 25, count as u32, page);

    let entries = models::UserEvent::feed(
        &conn,
        user.id,
        "similar_image",
        query.site,
        query.filter_allowlisted,
        page,
    )
    .await?;

    let body = Feed {
        entries: &entries,
        pagination_data,
        visible_sites: &visible_sites,
        site: site.as_deref(),
        filtering_allowlisted: query.filter_allowlisted,
    }
    .wrap(&request, Some(&user))
    .await
    .render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[derive(Template)]
#[template(path = "user/email/add.html")]
struct AddEmail {
    error_message: Option<String>,
}

#[get("/add")]
async fn email_add(
    request: actix_web::HttpRequest,
    user: models::User,
) -> Result<HttpResponse, Error> {
    let body = AddEmail {
        error_message: None,
    }
    .wrap(&request, Some(&user))
    .await
    .render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[derive(Deserialize)]
struct AddEmailForm {
    email: lettre::Address,
}

#[post("/add")]
async fn email_add_post(
    conn: web::Data<sqlx::PgPool>,
    faktory: web::Data<FaktoryProducer>,
    user: models::User,
    form: web::Form<AddEmailForm>,
) -> Result<HttpResponse, Error> {
    let email = form.email.to_string();

    if email.len() > 120 {
        return Err(Error::UserError(
            "Email must be less than 120 characters.".into(),
        ));
    }

    if models::User::email_exists(&conn, &email).await? {
        return Err(Error::UserError("Email is already in use.".into()));
    }

    models::User::set_email(&conn, user.id, &email).await?;

    faktory
        .enqueue_job(
            jobs::EmailVerificationJob { user_id: user.id }
                .initiated_by(jobs::JobInitiator::user(user.id)),
        )
        .await?;

    Ok(HttpResponse::Found()
        .insert_header(("Location", USER_HOME))
        .finish())
}

#[derive(Template)]
#[template(path = "user/verify.html")]
struct EmailVerify<'a> {
    user: &'a models::User,
    verifier: Uuid,
}

#[derive(Deserialize)]
struct EmailVerifyQuery {
    #[serde(rename = "u")]
    user_id: Uuid,
    #[serde(rename = "v")]
    verifier: Uuid,
}

#[get("/verify")]
async fn verify_email_get(
    request: actix_web::HttpRequest,
    conn: web::Data<sqlx::PgPool>,
    query: web::Query<EmailVerifyQuery>,
) -> Result<HttpResponse, Error> {
    if !models::User::check_email_verifier_token(&conn, query.user_id, query.verifier).await? {
        return Err(Error::user_error(
            "Account has already been verified or an invalid token was provided.",
        ));
    }

    let user = models::User::lookup_by_id(&conn, query.user_id)
        .await?
        .ok_or(Error::Missing)?;

    let body = EmailVerify {
        user: &user,
        verifier: query.verifier,
    }
    .wrap(&request, Some(&user))
    .await
    .render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[post("/verify")]
async fn verify_email_post(
    client_ip: ClientIpAddr,
    conn: web::Data<sqlx::PgPool>,
    form: web::Form<EmailVerifyQuery>,
    user: Option<models::User>,
    session: actix_session::Session,
) -> Result<HttpResponse, Error> {
    if !models::User::verify_email(&conn, form.user_id, form.verifier).await? {
        if models::User::lookup_by_id(&conn, form.user_id)
            .await?
            .is_some()
        {
            return Err(Error::user_error(
                "Account has already been verified or an invalid token was provided.",
            ));
        } else {
            return Err(Error::Missing);
        }
    };

    if user.is_none() {
        let session_id = models::UserSession::create(
            &conn,
            form.user_id,
            models::UserSessionSource::EmailVerification,
            client_ip.ip_addr.as_deref(),
        )
        .await?;
        session.set_session_token(form.user_id, session_id)?;
    }

    session.add_flash(FlashStyle::Success, "Email address verified.");

    Ok(HttpResponse::Found()
        .insert_header(("Location", USER_HOME))
        .finish())
}

#[derive(Debug, Deserialize)]
struct AllowlistForm {
    redirect_url: String,
    site: Site,
    site_username: String,
}

#[post("/add")]
async fn allowlist_add(
    conn: web::Data<sqlx::PgPool>,
    form: web::Form<AllowlistForm>,
    session: actix_session::Session,
    user: models::User,
) -> Result<HttpResponse, Error> {
    models::UserAllowlist::add(&conn, user.id, form.site, &form.site_username).await?;

    session.add_flash(FlashStyle::Success, "Account added to allowlist.");

    Ok(HttpResponse::Found()
        .insert_header(("Location", form.redirect_url.to_owned()))
        .finish())
}

#[post("/remove")]
async fn allowlist_remove(
    conn: web::Data<sqlx::PgPool>,
    form: web::Form<AllowlistForm>,
    session: actix_session::Session,
    user: models::User,
) -> Result<HttpResponse, Error> {
    models::UserAllowlist::remove(&conn, user.id, form.site, &form.site_username).await?;

    session.add_flash(FlashStyle::Success, "Account removed from allowlist.");

    Ok(HttpResponse::Found()
        .insert_header(("Location", form.redirect_url.to_owned()))
        .finish())
}

#[derive(Template)]
#[template(path = "user/unsubscribe.html")]
struct Unsubscribe<'a> {
    user: &'a models::User,
    verifier: Uuid,
}

#[derive(Deserialize)]
struct UnsubscribeQuery {
    #[serde(rename = "u")]
    user_id: Uuid,
    #[serde(rename = "t")]
    verifier: Uuid,
}

#[get("/unsubscribe")]
async fn unsubscribe_get(
    request: actix_web::HttpRequest,
    conn: web::Data<sqlx::PgPool>,
    query: web::Query<UnsubscribeQuery>,
) -> Result<HttpResponse, Error> {
    let user = models::User::lookup_by_id(&conn, query.user_id)
        .await?
        .ok_or(Error::Missing)?;

    if user.unsubscribe_token != query.verifier {
        return Err(Error::Missing);
    }

    let body = Unsubscribe {
        user: &user,
        verifier: query.verifier,
    }
    .wrap(&request, Some(&user))
    .await
    .render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[post("/unsubscribe")]
async fn unsubscribe_post(
    conn: web::Data<sqlx::PgPool>,
    form: web::Form<UnsubscribeQuery>,
) -> Result<HttpResponse, Error> {
    let user = models::User::lookup_by_id(&conn, form.user_id)
        .await?
        .ok_or(Error::Missing)?;

    if user.unsubscribe_token != form.verifier {
        return Err(Error::Missing);
    }

    models::UserSetting::set(
        &conn,
        user.id,
        &models::setting::EmailNotifications::off_value(),
    )
    .await?;

    Ok(HttpResponse::Found()
        .insert_header(("Location", USER_HOME))
        .finish())
}

#[derive(Template)]
#[template(path = "user/feed_item.html")]
struct FeedItem<'a> {
    posted_by: &'a str,
    site: models::Site,
    content_url: Option<&'a str>,
    media_url: &'a str,
    at_url: &'a str,
}

#[derive(Deserialize)]
struct RssQuery {
    #[serde(rename = "u")]
    user_id: Uuid,
    #[serde(rename = "t")]
    token: Uuid,
}

#[get("/feed/rss")]
async fn rss_feed(
    config: web::Data<crate::Config>,
    conn: web::Data<sqlx::PgPool>,
    query: web::Query<RssQuery>,
) -> Result<HttpResponse, Error> {
    let user = models::User::lookup_by_rss_token(&conn, query.user_id, query.token)
        .await?
        .ok_or(Error::Missing)?;

    let entries = models::UserEvent::feed(&conn, user.id, "similar_image", None, false, 0).await?;

    let items = entries.into_iter().filter_map(|entry| {
        let media = entry.media?;

        let similar_image = match entry.event.data.as_ref() {
            Some(models::UserEventData::SimilarImage(similar_image)) => similar_image,
            _ => return None,
        };

        let media_url = format!("{}/user/media/view/{}", config.host_url, media.id);

        let content = FeedItem {
            posted_by: similar_image
                .posted_by
                .as_deref()
                .unwrap_or("an unknown user"),
            site: similar_image.site,
            content_url: media.content_url.as_deref(),
            media_url: &media_url,
            at_url: similar_image
                .page_url
                .as_deref()
                .unwrap_or(&similar_image.content_url),
        }
        .render()
        .ok()?;

        let item = rss::ItemBuilder::default()
            .guid(
                rss::GuidBuilder::default()
                    .value(entry.event.id.to_string())
                    .permalink(false)
                    .build(),
            )
            .pub_date(entry.event.last_updated.to_rfc2822())
            .title("Image match found".to_string())
            .description(entry.event.display())
            .content(content)
            .link(media_url)
            .build();

        Some(item)
    });

    let channel = rss::ChannelBuilder::default()
        .title("FuzzySearch OwO Feed")
        .link(config.host_url.clone())
        .description("Image upload event feed.")
        .items(items.collect::<Vec<_>>())
        .build();

    Ok(HttpResponse::Ok()
        .content_type("application/rss+xml")
        .body(channel.to_string()))
}

pub fn service() -> Scope {
    web::scope("/user")
        .service(services![
            home,
            settings_get,
            settings_post,
            delete,
            events,
            single,
            unsubscribe_get,
            unsubscribe_post,
            feed,
            rss_feed,
            check_get,
            check_post,
        ])
        .service(web::scope("/account").service(services![
            account_link_get,
            account_link_post,
            account_remove,
            account_view,
            account_verify,
        ]))
        .service(web::scope("/media").service(services![media_list, media_view, media_remove]))
        .service(web::scope("/email").service(services![
            email_add,
            email_add_post,
            verify_email_get,
            verify_email_post
        ]))
        .service(web::scope("/allowlist").service(services![allowlist_add, allowlist_remove]))
}
