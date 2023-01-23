use std::{
    collections::{HashMap, HashSet},
    fmt::{Debug, Display},
    str::FromStr,
};

use argonautica::{Hasher, Verifier};
use image::GenericImageView;
use redis::AsyncCommands;
use rusoto_s3::S3;
use serde::{Deserialize, Serialize};
use serde_with::{DeserializeFromStr, SerializeDisplay};
use sha2::Digest;
use uuid::Uuid;

use crate::site::SiteFromConfig;
use crate::{api, site, Error};

pub struct User {
    pub id: Uuid,
    pub username: Option<String>,
    pub email: Option<String>,
    pub email_verifier: Option<Uuid>,
    pub telegram_id: Option<i64>,
    pub telegram_name: Option<String>,
    pub is_admin: bool,
    pub display_name: Option<String>,
    pub unsubscribe_token: Uuid,
    pub rss_token: Uuid,
    pub api_token: Uuid,
    pub reset_token: Option<String>,

    hashed_password: Option<String>,
}

impl Debug for User {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("User")
            .field("id", &self.id)
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

impl User {
    pub fn has_verified_account(&self) -> bool {
        self.has_verified_email() || self.has_telegram_account()
    }

    pub fn has_verified_email(&self) -> bool {
        self.email.is_some() && self.email_verifier.is_none()
    }

    pub fn has_unverified_email(&self) -> bool {
        matches!(self.email, Some(ref email) if !email.is_empty()) && self.email_verifier.is_some()
    }

    pub fn has_telegram_account(&self) -> bool {
        self.telegram_id.is_some()
    }

    pub fn display_name(&self) -> &str {
        if let Some(display_name) = &self.display_name {
            display_name
        } else if let Some(username) = &self.username {
            username
        } else if let Some(telegram_name) = &self.telegram_name {
            telegram_name
        } else {
            unreachable!("user must always have display name")
        }
    }

    pub async fn lookup_by_id(conn: &sqlx::PgPool, id: Uuid) -> Result<Option<User>, Error> {
        let user = sqlx::query_file_as!(User, "queries/user/lookup_id.sql", id)
            .fetch_optional(conn)
            .await?;

        Ok(user)
    }

    pub async fn lookup_by_login(
        conn: &sqlx::PgPool,
        username: &str,
        password: &str,
    ) -> Result<Option<User>, Error> {
        let user = sqlx::query_file_as!(User, "queries/user/lookup_login.sql", username)
            .fetch_optional(conn)
            .await?;

        let user = match user {
            Some(user) => user,
            None => return Ok(None),
        };

        if !user.verify_password(password) {
            return Ok(None);
        }

        Ok(Some(user))
    }

    pub async fn lookup_by_email(conn: &sqlx::PgPool, email: &str) -> Result<Option<User>, Error> {
        let user = sqlx::query_file_as!(User, "queries/user/lookup_email.sql", email)
            .fetch_optional(conn)
            .await?;

        Ok(user)
    }

    pub async fn lookup_by_telegram_id(
        conn: &sqlx::PgPool,
        telegram_id: i64,
    ) -> Result<Option<User>, Error> {
        let user =
            sqlx::query_file_as!(User, "queries/user/lookup_by_telegram_id.sql", telegram_id)
                .fetch_optional(conn)
                .await?;

        Ok(user)
    }

    pub async fn lookup_by_rss_token(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        rss_token: Uuid,
    ) -> Result<Option<User>, Error> {
        let user = sqlx::query_file_as!(
            User,
            "queries/user/lookup_by_rss_token.sql",
            user_id,
            rss_token
        )
        .fetch_optional(conn)
        .await?;

        Ok(user)
    }

    pub async fn lookup_by_api_token(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        api_token: Uuid,
    ) -> Result<Option<User>, Error> {
        let user = sqlx::query_file_as!(
            User,
            "queries/user/lookup_by_api_token.sql",
            user_id,
            api_token
        )
        .fetch_optional(conn)
        .await?;

        Ok(user)
    }

    pub async fn create(
        conn: &sqlx::PgPool,
        username: &str,
        password: &str,
    ) -> Result<Uuid, Error> {
        let password = Self::hash_password(password)?;

        let id = sqlx::query_file_scalar!("queries/user/create.sql", username, password)
            .fetch_one(conn)
            .await?;

        Ok(id)
    }

    pub async fn create_telegram(
        conn: &sqlx::PgPool,
        telegram_id: i64,
        telegram_name: &str,
    ) -> Result<Uuid, Error> {
        let id = sqlx::query_file_scalar!(
            "queries/user/create_telegram.sql",
            telegram_id,
            telegram_name
        )
        .fetch_one(conn)
        .await?;

        Ok(id)
    }

    pub async fn update_password(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        password: &str,
    ) -> Result<(), Error> {
        let password = Self::hash_password(password)?;

        sqlx::query_file!("queries/user/update_password.sql", user_id, password)
            .execute(conn)
            .await?;

        Ok(())
    }

    pub async fn username_exists(conn: &sqlx::PgPool, username: &str) -> Result<bool, Error> {
        let exists = sqlx::query_file_scalar!("queries/user/username_exists.sql", username)
            .fetch_one(conn)
            .await?;

        Ok(exists)
    }

    pub async fn email_exists(conn: &sqlx::PgPool, email: &str) -> Result<bool, Error> {
        let exists = sqlx::query_file_scalar!("queries/user/email_exists.sql", email)
            .fetch_one(conn)
            .await?;

        Ok(exists)
    }

    fn verify_password(&self, input: &str) -> bool {
        let hashed_password = match &self.hashed_password {
            Some(hashed_password) => hashed_password,
            None => return false,
        };

        let mut verifier = Verifier::default();

        verifier
            .with_hash(hashed_password)
            .with_password(input)
            .verify()
            .unwrap_or(false)
    }

    fn hash_password(password: &str) -> Result<String, Error> {
        let mut hasher = Hasher::default();
        hasher.opt_out_of_secret_key(true);

        let hash = hasher
            .with_password(password)
            .hash()
            .map_err(Error::from_displayable)?;
        Ok(hash)
    }

    pub async fn set_email(conn: &sqlx::PgPool, user_id: Uuid, email: &str) -> Result<(), Error> {
        sqlx::query_file!("queries/user/set_email.sql", user_id, email)
            .execute(conn)
            .await?;

        Ok(())
    }

    pub async fn check_email_verifier_token(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        verifier: Uuid,
    ) -> Result<bool, Error> {
        let valid_token = sqlx::query_file_scalar!(
            "queries/user/check_email_verifier_token.sql",
            user_id,
            verifier
        )
        .fetch_one(conn)
        .await?;

        Ok(valid_token.unwrap_or(false))
    }

    pub async fn verify_email(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        verifier: Uuid,
    ) -> Result<bool, Error> {
        let updated_user_id =
            sqlx::query_file_scalar!("queries/user/verify_email.sql", user_id, verifier)
                .fetch_optional(conn)
                .await?;

        Ok(updated_user_id.is_some())
    }

    pub async fn associate_telegram(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        telegram_id: i64,
        telegram_name: &str,
    ) -> Result<(), Error> {
        sqlx::query_file!(
            "queries/user/associate_telegram.sql",
            user_id,
            telegram_id,
            telegram_name
        )
        .execute(conn)
        .await?;

        Ok(())
    }

    pub async fn delete(conn: &sqlx::PgPool, user_id: Uuid) -> Result<(), Error> {
        sqlx::query_file!("queries/user/delete.sql", user_id)
            .execute(conn)
            .await?;

        Ok(())
    }

    pub async fn update_display_name(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        display_name: Option<&str>,
    ) -> Result<(), Error> {
        sqlx::query_file!(
            "queries/user/update_display_name.sql",
            user_id,
            display_name
        )
        .execute(conn)
        .await?;

        Ok(())
    }

    pub async fn set_reset_token(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        token: &str,
    ) -> Result<(), Error> {
        sqlx::query_file!("queries/user/set_reset_token.sql", user_id, token)
            .execute(conn)
            .await?;

        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "source", content = "source_data")]
pub enum UserSessionSource {
    Unknown,
    Registration,
    Login,
    EmailVerification,
    Telegram,
}

impl UserSessionSource {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Registration => "registration",
            Self::Login => "login",
            Self::EmailVerification => "email verification",
            Self::Telegram => "Telegram",
        }
    }
}

pub struct UserSession {
    pub id: Uuid,
    pub user_id: Uuid,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_used: chrono::DateTime<chrono::Utc>,
    pub source: UserSessionSource,
    pub ip_addr: Option<std::net::IpAddr>,
}

impl UserSession {
    pub fn display_ip_addr(&self) -> String {
        match self.ip_addr {
            Some(addr) => addr.to_string(),
            None => "unknown".to_string(),
        }
    }
    pub async fn create(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        source: UserSessionSource,
        ip_addr: Option<&str>,
    ) -> Result<Uuid, Error> {
        let id = sqlx::query_file_scalar!(
            "queries/user_session/create.sql",
            user_id,
            serde_json::to_value(source)?,
            ip_addr
                .and_then(|ip| ip.parse::<std::net::IpAddr>().ok())
                .map(ipnetwork::IpNetwork::from),
        )
        .fetch_one(conn)
        .await?;

        Ok(id)
    }

    pub async fn check(
        conn: &sqlx::PgPool,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<User>, Error> {
        let user_id = match sqlx::query_file_scalar!("queries/user_session/check.sql", user_id, id)
            .fetch_optional(conn)
            .await?
        {
            Some(user_id) => user_id,
            None => return Ok(None),
        };

        User::lookup_by_id(conn, user_id).await
    }

    pub async fn destroy(
        conn: &sqlx::PgPool,
        redis: &redis::aio::ConnectionManager,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<(), Error> {
        sqlx::query_file!("queries/user_session/destroy.sql", user_id, id)
            .execute(conn)
            .await?;

        let mut redis = redis.clone();
        redis
            .publish(
                format!("user-events:{}", user_id),
                serde_json::to_string(&api::EventMessage::SessionEnded { session_id: id })?,
            )
            .await?;

        Ok(())
    }

    pub async fn list(conn: &sqlx::PgPool, user_id: Uuid) -> Result<Vec<Self>, Error> {
        let sessions = sqlx::query_file!("queries/user_session/list.sql", user_id)
            .map(|row| Self {
                id: row.id,
                user_id: row.user_id,
                created_at: row.created_at,
                last_used: row.last_used,
                source: serde_json::from_value(row.source).unwrap_or(UserSessionSource::Unknown),
                ip_addr: row.creation_ip.map(|ip| ip.ip()),
            })
            .fetch_all(conn)
            .await?;

        Ok(sessions)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Sha256Hash([u8; 32]);

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for Sha256Hash {
    fn decode(
        value: <sqlx::Postgres as sqlx::database::HasValueRef<'r>>::ValueRef,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let value = <Vec<u8> as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        let data = value
            .try_into()
            .map_err(|_err| anyhow::anyhow!("data could not be converted"))?;

        Ok(Self(data))
    }
}

impl std::ops::Deref for Sha256Hash {
    type Target = [u8; 32];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OwnedMediaItem {
    pub id: Uuid,
    pub owner_id: Uuid,

    pub account_id: Option<Uuid>,
    pub source_id: Option<String>,

    pub perceptual_hash: Option<i64>,
    pub sha256_hash: Sha256Hash,

    pub link: Option<String>,
    pub title: Option<String>,
    pub posted_at: Option<chrono::DateTime<chrono::Utc>>,

    pub last_modified: chrono::DateTime<chrono::Utc>,

    pub content_url: Option<String>,
    pub content_size: Option<i64>,
    pub thumb_url: Option<String>,

    pub event_count: i32,
    pub last_event: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaListSort {
    Added,
    Events,
    Recent,
}

impl MediaListSort {
    pub fn name(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Events => "events",
            Self::Recent => "recent",
        }
    }
}

impl OwnedMediaItem {
    pub async fn get_by_id(
        conn: &sqlx::PgPool,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<Self>, Error> {
        let item = sqlx::query_file_as!(Self, "queries/owned_media/get_by_id.sql", id, user_id)
            .fetch_optional(conn)
            .await?;

        Ok(item)
    }

    pub async fn lookup_by_site_id<S: ToString>(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        site: Site,
        site_id: S,
    ) -> Result<Option<Self>, Error> {
        let item = sqlx::query_file_as!(
            Self,
            "queries/owned_media/lookup_by_site_id.sql",
            user_id,
            site.to_string(),
            site_id.to_string()
        )
        .fetch_optional(conn)
        .await?;

        Ok(item)
    }

    pub async fn user_item_count(conn: &sqlx::PgPool, user_id: Uuid) -> Result<(i64, i64), Error> {
        let stats = sqlx::query_file!("queries/owned_media/user_item_count.sql", user_id)
            .fetch_one(conn)
            .await?;

        Ok((stats.count, stats.total_content_size))
    }

    pub async fn add_manual_item(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        perceptual_hash: i64,
        sha256_hash: [u8; 32],
        title: Option<&str>,
    ) -> Result<Uuid, Error> {
        let item_id = sqlx::query_file_scalar!(
            "queries/owned_media/add_manual_item.sql",
            user_id,
            if perceptual_hash != 0 {
                Some(perceptual_hash)
            } else {
                None
            },
            sha256_hash.to_vec(),
            title,
        )
        .fetch_one(conn)
        .await?;

        Ok(item_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn add_item<ID: ToString>(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        account_id: Uuid,
        source_id: ID,
        perceptual_hash: Option<i64>,
        sha256_hash: [u8; 32],
        link: Option<String>,
        title: Option<String>,
        posted_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Uuid, Error> {
        let item = sqlx::query_file_scalar!(
            "queries/owned_media/add_item.sql",
            user_id,
            account_id,
            source_id.to_string(),
            perceptual_hash.filter(|hash| *hash != 0),
            sha256_hash.to_vec(),
            link,
            title,
            posted_at
        )
        .fetch_one(conn)
        .await?;

        Ok(item)
    }

    pub async fn update_media(
        conn: &sqlx::PgPool,
        s3: &rusoto_s3::S3Client,
        config: &crate::Config,
        id: Uuid,
        full_size: image::DynamicImage,
    ) -> Result<(), Error> {
        let (width, height) = full_size.dimensions();
        let content = if width > 2000 || height > 2000 {
            tracing::trace!(width, height, "resizing content");
            full_size.resize(2000, 2000, image::imageops::FilterType::Lanczos3)
        } else {
            full_size.clone()
        };
        let (content_url, content_size) = upload_image(s3, config, content).await?;

        let thumb = if width > 300 || height > 300 {
            tracing::trace!(width, height, "resizing for thumbnail");
            full_size.resize(300, 300, image::imageops::FilterType::Lanczos3)
        } else {
            full_size
        };
        let (thumb_url, _thumb_size) = upload_image(s3, config, thumb).await?;

        sqlx::query_file!(
            "queries/owned_media/update_media.sql",
            id,
            content_url,
            content_size as i64,
            thumb_url
        )
        .execute(conn)
        .await?;

        Ok(())
    }

    pub async fn recent_media(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        account_id: Option<Uuid>,
    ) -> Result<Vec<Self>, Error> {
        let items = sqlx::query_file_as!(
            Self,
            "queries/owned_media/recent_media.sql",
            user_id,
            account_id
        )
        .fetch_all(conn)
        .await?;

        Ok(items)
    }

    pub fn alt_text(&self) -> String {
        match (self.title.as_ref(), self.posted_at) {
            (Some(title), Some(posted_at)) => {
                format!("{} posted {}", title, posted_at.to_rfc2822())
            }
            (Some(title), None) => title.to_string(),
            (None, Some(posted_at)) => format!("Item posted {}", posted_at.to_rfc2822()),
            (None, None) => "Unknown".to_string(),
        }
    }

    pub async fn find_similar(
        conn: &sqlx::PgPool,
        perceptual_hash: i64,
    ) -> Result<Vec<Self>, Error> {
        let items = sqlx::query_file_as!(
            Self,
            "queries/owned_media/find_similar.sql",
            perceptual_hash,
            3
        )
        .fetch_all(conn)
        .await?;

        Ok(items)
    }

    pub async fn find_similar_with_owner(
        conn: &sqlx::PgPool,
        owner_id: Uuid,
        perceptual_hash: i64,
    ) -> Result<Vec<Self>, Error> {
        let items = sqlx::query_file_as!(
            Self,
            "queries/owned_media/find_similar_with_owner.sql",
            owner_id,
            perceptual_hash,
            3
        )
        .fetch_all(conn)
        .await?;

        Ok(items)
    }

    pub async fn remove(conn: &sqlx::PgPool, user_id: Uuid, media_id: Uuid) -> Result<(), Error> {
        sqlx::query_file!("queries/owned_media/remove.sql", user_id, media_id)
            .execute(conn)
            .await?;

        Ok(())
    }

    pub async fn media_page(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        page: u32,
        sort: MediaListSort,
        account_id: Option<Uuid>,
    ) -> Result<Vec<Self>, Error> {
        const ITEMS_PER_PAGE: i32 = 25;

        let media = match sort {
            MediaListSort::Added => {
                sqlx::query_file_as!(
                    Self,
                    "queries/owned_media/media_before_added.sql",
                    user_id,
                    ITEMS_PER_PAGE,
                    page as i64,
                    account_id,
                )
                .fetch_all(conn)
                .await?
            }
            MediaListSort::Events => {
                sqlx::query_file_as!(
                    Self,
                    "queries/owned_media/media_before_events.sql",
                    user_id,
                    ITEMS_PER_PAGE,
                    page as i64,
                    account_id,
                )
                .fetch_all(conn)
                .await?
            }
            MediaListSort::Recent => {
                sqlx::query_file_as!(
                    Self,
                    "queries/owned_media/media_before_recent.sql",
                    user_id,
                    ITEMS_PER_PAGE,
                    page as i64,
                    account_id,
                )
                .fetch_all(conn)
                .await?
            }
        };

        Ok(media)
    }

    pub async fn count(conn: &sqlx::PgPool, user_id: Uuid) -> Result<i64, Error> {
        let count = sqlx::query_file_scalar!("queries/owned_media/count.sql", user_id)
            .fetch_one(conn)
            .await?
            .unwrap_or_default();

        Ok(count)
    }

    pub async fn resolve(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        media_ids: impl Iterator<Item = Uuid>,
    ) -> Result<HashMap<Uuid, Self>, Error> {
        let items = sqlx::query_file_as!(
            Self,
            "queries/owned_media/resolve_media.sql",
            &media_ids.collect::<Vec<Uuid>>(),
            user_id
        )
        .fetch_all(conn)
        .await?;

        Ok(items.into_iter().map(|item| (item.id, item)).collect())
    }
}

async fn upload_image(
    s3: &rusoto_s3::S3Client,
    config: &crate::Config,
    im: image::DynamicImage,
) -> Result<(String, usize), Error> {
    tracing::debug!("uploading image to s3");

    // Remove transparency before attempting to save as JPEG.
    let im = image::DynamicImage::ImageRgb8(im.to_rgb8());

    let mut buf = Vec::new();
    im.write_to(&mut buf, image::ImageOutputFormat::Jpeg(80))?;
    let size = buf.len();

    let mut hasher = sha2::Sha256::default();
    hasher.update(&buf);
    let hash: [u8; 32] = hasher
        .finalize()
        .try_into()
        .expect("sha256 hash had wrong length");
    let hash = hex::encode(hash);
    let path = format!("{}/{}/{}.jpg", &hash[0..2], &hash[2..4], &hash);

    let put = rusoto_s3::PutObjectRequest {
        acl: Some("download".to_string()),
        bucket: config.s3_bucket.clone(),
        content_type: Some("image/jpeg".to_string()),
        key: path.clone(),
        content_length: Some(buf.len() as i64),
        body: Some(rusoto_core::ByteStream::from(buf)),
        content_disposition: Some("inline".to_string()),
        ..Default::default()
    };

    s3.put_object(put)
        .await
        .map_err(|err| Error::S3(err.to_string()))?;

    Ok((
        format!(
            "{}/{}/{}",
            config.s3_region_endpoint, config.s3_bucket, path
        ),
        size,
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadingState {
    Unknown,
    DiscoveringItems,
    LoadingItems { known: i32 },
    Custom { message: String },
    Complete,
}

impl LoadingState {
    pub fn message(&self) -> String {
        match self {
            LoadingState::Unknown => "Starting Loading".to_string(),
            LoadingState::DiscoveringItems => "Discovering Items".to_string(),
            LoadingState::LoadingItems { known } => format!("Processing {} Items", known),
            LoadingState::Custom { message } => message.clone(),
            LoadingState::Complete => "Loading Complete".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkedAccount {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub source_site: Site,
    pub username: String,
    pub last_update: Option<chrono::DateTime<chrono::Utc>>,
    pub loading_state: Option<LoadingState>,
    pub data: Option<serde_json::Value>,
}

impl LinkedAccount {
    pub async fn create(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        source_site: Site,
        username: &str,
        data: Option<serde_json::Value>,
    ) -> Result<Self, Error> {
        let id = sqlx::query_file!(
            "queries/linked_account/create.sql",
            user_id,
            source_site.to_string(),
            username,
            data
        )
        .map(|row| LinkedAccount {
            id: row.id,
            owner_id: row.owner_id,
            source_site: row.source_site.parse().expect("unknown site in database"),
            username: row.username,
            last_update: row.last_update,
            loading_state: row
                .loading_state
                .and_then(|loading_state| serde_json::from_value(loading_state).ok()),
            data: row.data,
        })
        .fetch_one(conn)
        .await?;

        Ok(id)
    }

    pub async fn lookup_by_id(conn: &sqlx::PgPool, id: Uuid) -> Result<Option<Self>, Error> {
        let account = sqlx::query_file!("queries/linked_account/lookup_by_id.sql", id)
            .map(|row| LinkedAccount {
                id: row.id,
                owner_id: row.owner_id,
                source_site: row.source_site.parse().expect("unknown site in database"),
                username: row.username,
                last_update: row.last_update,
                loading_state: row
                    .loading_state
                    .and_then(|loading_state| serde_json::from_value(loading_state).ok()),
                data: row.data,
            })
            .fetch_optional(conn)
            .await?;

        Ok(account)
    }

    pub async fn lookup_by_site_id(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        site: Site,
        site_id: &str,
    ) -> Result<Option<Self>, Error> {
        let account = sqlx::query_file!(
            "queries/linked_account/lookup_by_site_id.sql",
            user_id,
            site.to_string(),
            site_id
        )
        .map(|row| LinkedAccount {
            id: row.id,
            owner_id: row.owner_id,
            source_site: row.source_site.parse().expect("unknown site in database"),
            username: row.username,
            last_update: row.last_update,
            loading_state: row
                .loading_state
                .and_then(|loading_state| serde_json::from_value(loading_state).ok()),
            data: row.data,
        })
        .fetch_optional(conn)
        .await?;

        Ok(account)
    }

    pub async fn owned_by_user(conn: &sqlx::PgPool, user_id: Uuid) -> Result<Vec<Self>, Error> {
        let accounts = sqlx::query_file!("queries/linked_account/owned_by_user.sql", user_id)
            .map(|row| LinkedAccount {
                id: row.id,
                owner_id: row.owner_id,
                source_site: row.source_site.parse().expect("unknown site in database"),
                username: row.username,
                last_update: row.last_update,
                loading_state: row
                    .loading_state
                    .and_then(|loading_state| serde_json::from_value(loading_state).ok()),
                data: row.data,
            })
            .fetch_all(conn)
            .await?;

        Ok(accounts)
    }

    pub fn loading_state(&self) -> String {
        self.loading_state
            .as_ref()
            .unwrap_or(&LoadingState::Unknown)
            .message()
    }

    pub fn is_loading(&self) -> bool {
        !matches!(
            self.loading_state
                .as_ref()
                .unwrap_or(&LoadingState::Unknown),
            LoadingState::Complete
        )
    }

    pub async fn update_loading_state(
        conn: &sqlx::PgPool,
        redis: &redis::aio::ConnectionManager,
        user_id: Uuid,
        account_id: Uuid,
        loading_state: LoadingState,
    ) -> Result<(), Error> {
        sqlx::query_file!(
            "queries/linked_account/update_loading_state.sql",
            user_id,
            account_id,
            serde_json::to_value(&loading_state)?,
        )
        .execute(conn)
        .await?;

        let mut redis = redis.clone();
        redis
            .publish(
                format!("user-events:{}", user_id),
                serde_json::to_string(&api::EventMessage::LoadingStateChange {
                    account_id,
                    loading_state: loading_state.message(),
                })?,
            )
            .await?;

        Ok(())
    }

    pub async fn update_data(
        conn: &sqlx::PgPool,
        account_id: Uuid,
        data: Option<serde_json::Value>,
    ) -> Result<(), Error> {
        sqlx::query_file!("queries/linked_account/update_data.sql", account_id, data)
            .execute(conn)
            .await?;

        Ok(())
    }

    pub async fn remove(conn: &sqlx::PgPool, user_id: Uuid, account_id: Uuid) -> Result<(), Error> {
        sqlx::query_file!("queries/linked_account/remove.sql", user_id, account_id)
            .execute(conn)
            .await?;

        Ok(())
    }

    pub async fn items(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        account_id: Uuid,
    ) -> Result<(i64, i64), Error> {
        let stats = sqlx::query_file!("queries/linked_account/items.sql", user_id, account_id)
            .fetch_one(conn)
            .await?;

        Ok((stats.count, stats.total_content_size))
    }

    pub async fn search_site_account(
        conn: &sqlx::PgPool,
        site_name: &str,
        username: &str,
    ) -> Result<Vec<(Uuid, Uuid)>, Error> {
        let account = sqlx::query_file!(
            "queries/linked_account/search_site_account.sql",
            site_name,
            username
        )
        .map(|row| (row.id, row.owner_id))
        .fetch_all(conn)
        .await?;

        Ok(account)
    }

    pub async fn all_site_accounts(conn: &sqlx::PgPool, site: Site) -> Result<Vec<Uuid>, Error> {
        let accounts = sqlx::query_file_scalar!(
            "queries/linked_account/all_site_accounts.sql",
            site.to_string()
        )
        .fetch_all(conn)
        .await?;

        Ok(accounts)
    }

    pub fn verification_key(&self) -> Option<&str> {
        self.data
            .as_ref()
            .and_then(|data| data.as_object())
            .and_then(|obj| obj.get("verification_key"))
            .and_then(|key| key.as_str())
    }

    pub fn show_twitter_archive_import(&self) -> bool {
        if self.source_site != Site::Twitter {
            return false;
        }

        !self
            .data
            .as_ref()
            .and_then(|data| data.as_object())
            .and_then(|obj| obj.get("has_imported_archive"))
            .and_then(|has| has.as_bool())
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Copy, DeserializeFromStr, SerializeDisplay, PartialEq, Eq, Hash)]
pub enum Site {
    FurAffinity,
    E621,
    Weasyl,
    Twitter,
    Patreon,
    FList,
    DeviantArt,
    Reddit,
    InternalTesting,
}

impl Site {
    pub fn visible_sites() -> Vec<String> {
        [
            Self::FurAffinity,
            Self::E621,
            Self::Weasyl,
            Self::Twitter,
            Self::FList,
            Self::Reddit,
        ]
        .map(|site| site.to_string())
        .to_vec()
    }

    pub async fn collected_site(
        &self,
        config: &crate::Config,
    ) -> Result<Option<Box<dyn crate::site::CollectedSite>>, Error> {
        let site: Option<Box<dyn crate::site::CollectedSite>> = match self {
            Site::FurAffinity => Some(Box::new(site::FurAffinity::site_from_config(config).await?)),
            Site::DeviantArt => Some(Box::new(site::DeviantArt::site_from_config(config).await?)),
            Site::Patreon => Some(Box::new(site::Patreon::site_from_config(config).await?)),
            Site::Weasyl => Some(Box::new(site::Weasyl::site_from_config(config).await?)),
            Site::Twitter => Some(Box::new(site::Twitter::site_from_config(config).await?)),
            _ => None,
        };

        Ok(site)
    }
}

impl Display for Site {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Site::FurAffinity => write!(f, "FurAffinity"),
            Site::E621 => write!(f, "e621"),
            Site::Weasyl => write!(f, "Weasyl"),
            Site::Twitter => write!(f, "Twitter"),
            Site::Patreon => write!(f, "Patreon"),
            Site::FList => write!(f, "F-list"),
            Site::DeviantArt => write!(f, "DeviantArt"),
            Site::Reddit => write!(f, "Reddit"),
            Site::InternalTesting => write!(f, "Internal Testing"),
        }
    }
}

impl FromStr for Site {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let site = match value {
            "FurAffinity" => Site::FurAffinity,
            "e621" => Site::E621,
            "Weasyl" => Site::Weasyl,
            "Twitter" => Site::Twitter,
            "Patreon" => Site::Patreon,
            "F-list" => Site::FList,
            "DeviantArt" => Site::DeviantArt,
            "Reddit" => Site::Reddit,
            "Internal Testing" => Site::InternalTesting,
            _ => {
                tracing::warn!(value, "had unknown site");
                return Err("unknown source site");
            }
        };

        Ok(site)
    }
}

impl From<fuzzysearch::SiteInfo> for Site {
    fn from(site: fuzzysearch::SiteInfo) -> Self {
        match site {
            fuzzysearch::SiteInfo::FurAffinity(_) => Site::FurAffinity,
            fuzzysearch::SiteInfo::E621(_) => Site::E621,
            fuzzysearch::SiteInfo::Weasyl => Site::Weasyl,
            fuzzysearch::SiteInfo::Twitter => Site::Twitter,
        }
    }
}

impl From<fuzzysearch_common::types::Site> for Site {
    fn from(site: fuzzysearch_common::types::Site) -> Self {
        match site {
            fuzzysearch_common::types::Site::FurAffinity => Site::FurAffinity,
            fuzzysearch_common::types::Site::E621 => Site::E621,
            fuzzysearch_common::types::Site::Weasyl => Site::Weasyl,
            fuzzysearch_common::types::Site::Twitter => Site::Twitter,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimilarImage {
    pub site: Site,
    pub posted_by: Option<String>,
    pub page_url: Option<String>,
    pub content_url: String,
}

impl SimilarImage {
    pub fn best_link(&self) -> &str {
        match self.page_url.as_deref() {
            Some(url) => url,
            None => &self.content_url,
        }
    }

    pub fn display(&self) -> String {
        match (self.posted_by.as_ref(), self.page_url.as_ref()) {
            (Some(posted_by), Some(page_url)) => format!(
                "Image posted by {} was found on {}: {}",
                posted_by, self.site, page_url
            ),
            (Some(posted_by), None) => format!(
                "Image posted by {} was found on {}: {}",
                posted_by, self.site, self.content_url
            ),
            (None, Some(page_url)) => {
                format!("Image was found on {}: {}", self.site, page_url)
            }
            (None, None) => format!("Image was found on {}: {}", self.site, self.content_url),
        }
    }

    pub fn poster_pair(&self) -> Option<(Site, String)> {
        self.posted_by
            .as_deref()
            .map(|posted_by| (self.site, posted_by.to_lowercase()))
    }
}

impl From<SimilarImage> for UserEventData {
    fn from(similar: SimilarImage) -> Self {
        UserEventData::SimilarImage(similar)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UserEventData {
    SimilarImage(SimilarImage),
}

impl UserEventData {
    pub fn event_name(&self) -> String {
        match self {
            UserEventData::SimilarImage(_) => "similar_image".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserEvent {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub related_to_media_item_id: Option<Uuid>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_updated: chrono::DateTime<chrono::Utc>,

    pub message: String,
    pub data: Option<UserEventData>,
}

pub struct EventAndRelatedMedia {
    pub event: UserEvent,
    pub media: Option<OwnedMediaItem>,
}

impl UserEvent {
    pub async fn notify<M: AsRef<str>>(
        conn: &sqlx::PgPool,
        redis: &redis::aio::ConnectionManager,
        user_id: Uuid,
        message: M,
    ) -> Result<Uuid, Error> {
        let notification_id = sqlx::query_file_scalar!(
            "queries/user_event/notify.sql",
            user_id,
            message.as_ref(),
            "user"
        )
        .fetch_one(conn)
        .await?;

        let mut redis = redis.clone();
        redis
            .publish(
                format!("user-events:{}", user_id),
                serde_json::to_string(&api::EventMessage::SimpleMessage {
                    id: notification_id,
                    message: message.as_ref().to_string(),
                })?,
            )
            .await?;

        Ok(notification_id)
    }

    pub async fn similar_found(
        conn: &sqlx::PgPool,
        redis: &redis::aio::ConnectionManager,
        user_id: Uuid,
        media_id: Uuid,
        similar_image: SimilarImage,
        created_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Uuid, Error> {
        let link = similar_image.best_link().to_owned();
        let data: UserEventData = similar_image.into();

        let notification_id = sqlx::query_file_scalar!(
            "queries/user_event/similar_found.sql",
            user_id,
            media_id,
            format!("Found similar image: {}", link),
            data.event_name(),
            serde_json::to_value(data)?,
            created_at.unwrap_or_else(|| chrono::Utc::now()),
        )
        .fetch_one(conn)
        .await?;

        if created_at.is_none() {
            let mut redis = redis.clone();
            redis
                .publish(
                    format!("user-events:{}", user_id),
                    serde_json::to_string(&api::EventMessage::SimilarImage { media_id, link })?,
                )
                .await?;
        }

        Ok(notification_id)
    }

    pub async fn recent_events(conn: &sqlx::PgPool, user_id: Uuid) -> Result<Vec<Self>, Error> {
        let events = sqlx::query_file!("queries/user_event/recent_events.sql", user_id)
            .map(|row| UserEvent {
                id: row.id,
                owner_id: row.owner_id,
                related_to_media_item_id: row.related_to_media_item_id,
                created_at: row.created_at,
                message: row.message,
                data: row.data.and_then(|data| serde_json::from_value(data).ok()),
                last_updated: row.last_updated,
            })
            .fetch_all(conn)
            .await?;

        Ok(events)
    }

    pub async fn count(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        event_name: Option<&str>,
        site: Option<Site>,
        filter_allowlisted: bool,
    ) -> Result<i64, Error> {
        let count = sqlx::query_file_scalar!(
            "queries/user_event/count.sql",
            user_id,
            event_name,
            site.map(|site| site.to_string()),
            filter_allowlisted,
        )
        .fetch_one(conn)
        .await?;

        Ok(count.unwrap_or(0))
    }

    pub async fn feed(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        event_name: &str,
        site: Option<Site>,
        filter_allowlisted: bool,
        page: u32,
    ) -> Result<Vec<EventAndRelatedMedia>, Error> {
        let entries = sqlx::query_file!(
            "queries/user_event/event_feed.sql",
            user_id,
            25,
            page as i64,
            event_name,
            site.map(|site| site.to_string()),
            filter_allowlisted,
        )
        .map(|row| Self {
            id: row.id,
            owner_id: row.owner_id,
            related_to_media_item_id: row.related_to_media_item_id,
            created_at: row.created_at,
            message: row.message,
            data: row.data.and_then(|data| serde_json::from_value(data).ok()),
            last_updated: row.last_updated,
        })
        .fetch_all(conn)
        .await?;

        let media = OwnedMediaItem::resolve(
            conn,
            user_id,
            entries
                .iter()
                .flat_map(|entry| entry.related_to_media_item_id),
        )
        .await?;

        let entries_and_media = entries.into_iter().map(|entry| EventAndRelatedMedia {
            media: entry
                .related_to_media_item_id
                .and_then(|media_id| media.get(&media_id).cloned()),
            event: entry,
        });

        Ok(entries_and_media.collect())
    }

    pub async fn recent_events_for_media(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        media_id: Uuid,
    ) -> Result<Vec<Self>, Error> {
        let events = sqlx::query_file!(
            "queries/user_event/recent_events_for_media.sql",
            user_id,
            media_id
        )
        .map(|row| Self {
            id: row.id,
            owner_id: row.owner_id,
            related_to_media_item_id: row.related_to_media_item_id,
            created_at: row.created_at,
            message: row.message,
            data: row.data.and_then(|data| serde_json::from_value(data).ok()),
            last_updated: row.last_updated,
        })
        .fetch_all(conn)
        .await?;

        Ok(events)
    }

    pub fn display(&self) -> String {
        match self.data.as_ref() {
            Some(UserEventData::SimilarImage(similar)) => similar.display(),
            _ => "Unknown event".to_string(),
        }
    }

    pub async fn resolve(
        conn: &sqlx::PgPool,
        event_ids: impl Iterator<Item = Uuid>,
    ) -> Result<HashMap<Uuid, Self>, Error> {
        let items = sqlx::query_file!(
            "queries/user_event/resolve.sql",
            &event_ids.collect::<Vec<Uuid>>()
        )
        .map(|row| UserEvent {
            id: row.id,
            owner_id: row.owner_id,
            related_to_media_item_id: row.related_to_media_item_id,
            created_at: row.created_at,
            message: row.message,
            data: row.data.and_then(|data| serde_json::from_value(data).ok()),
            last_updated: row.last_updated,
        })
        .fetch_all(conn)
        .await?;

        Ok(items.into_iter().map(|item| (item.id, item)).collect())
    }
}

pub struct AuthState;

impl AuthState {
    pub async fn create(conn: &sqlx::PgPool, user_id: Uuid, state: &str) -> Result<Uuid, Error> {
        let id = sqlx::query_file_scalar!("queries/auth_state/create.sql", user_id, state)
            .fetch_one(conn)
            .await?;

        Ok(id)
    }

    pub async fn lookup(conn: &sqlx::PgPool, user_id: Uuid, state: &str) -> Result<bool, Error> {
        let state = sqlx::query_file_scalar!("queries/auth_state/lookup.sql", user_id, state)
            .fetch_optional(conn)
            .await?;

        Ok(state.is_some())
    }

    pub async fn remove(conn: &sqlx::PgPool, user_id: Uuid, state: &str) -> Result<(), Error> {
        sqlx::query_file!("queries/auth_state/remove.sql", user_id, state)
            .execute(conn)
            .await?;

        Ok(())
    }
}

pub struct PatreonWebhookEvent;

impl PatreonWebhookEvent {
    pub async fn log(
        conn: &sqlx::PgPool,
        linked_account_id: Uuid,
        event: serde_json::Value,
    ) -> Result<Uuid, Error> {
        let id = sqlx::query_file_scalar!(
            "queries/patreon_webhook_event/log.sql",
            linked_account_id,
            event
        )
        .fetch_one(conn)
        .await?;

        Ok(id)
    }
}

#[derive(Serialize)]
pub struct FListFile {
    pub id: i32,
    pub ext: String,
    pub character_name: String,
    pub size: Option<i32>,
    pub sha256: Option<Vec<u8>>,
    pub perceptual_hash: Option<i64>,
}

impl FListFile {
    pub async fn similar_images(
        conn: &sqlx::PgPool,
        perceptual_hash: i64,
    ) -> Result<Vec<Self>, Error> {
        let images =
            sqlx::query_file_as!(Self, "queries/flist/similar_images.sql", perceptual_hash, 3)
                .fetch_all(conn)
                .await?;

        Ok(images)
    }

    pub async fn insert_item(
        conn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        id: i32,
        ext: &str,
        character_name: &str,
    ) -> Result<(), Error> {
        sqlx::query_file!("queries/flist/insert_item.sql", id, ext, character_name)
            .execute(conn)
            .await?;

        Ok(())
    }

    pub async fn get_by_id(conn: &sqlx::PgPool, id: i32) -> Result<Option<Self>, Error> {
        let item = sqlx::query_file_as!(Self, "queries/flist/get_by_id.sql", id)
            .fetch_optional(conn)
            .await?;

        Ok(item)
    }

    pub async fn update(
        conn: &sqlx::PgPool,
        id: i32,
        size: i32,
        sha256: Vec<u8>,
        perceptual_hash: Option<i64>,
    ) -> Result<(), Error> {
        sqlx::query_file!(
            "queries/flist/update.sql",
            id,
            sha256,
            size,
            perceptual_hash
        )
        .execute(conn)
        .await?;

        Ok(())
    }
}

pub struct FListImportRun {
    pub id: Uuid,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    pub starting_id: i32,
    pub max_id: Option<i32>,
}

impl FListImportRun {
    pub async fn previous_run(conn: &sqlx::PgPool) -> Result<Option<Self>, Error> {
        let previous_run = sqlx::query_file_as!(Self, "queries/flist/previous_run.sql")
            .fetch_optional(conn)
            .await?;

        Ok(previous_run)
    }

    pub async fn start(conn: &sqlx::PgPool, starting_id: i32) -> Result<Uuid, Error> {
        let id = sqlx::query_file_scalar!("queries/flist/start.sql", starting_id)
            .fetch_one(conn)
            .await?;

        Ok(id)
    }

    pub async fn complete(conn: &sqlx::PgPool, id: Uuid, max_id: i32) -> Result<(), Error> {
        sqlx::query_file!("queries/flist/complete.sql", id, max_id)
            .execute(conn)
            .await?;

        Ok(())
    }

    pub async fn recent_runs(conn: &sqlx::PgPool) -> Result<Vec<Self>, Error> {
        let runs = sqlx::query_file_as!(Self, "queries/flist/recent_runs.sql")
            .fetch_all(conn)
            .await?;

        Ok(runs)
    }

    pub async fn abort_run(conn: &sqlx::PgPool, id: Uuid) -> Result<(), Error> {
        sqlx::query_file!("queries/flist/abort_run.sql", id)
            .execute(conn)
            .await?;

        Ok(())
    }
}

pub struct RedditSubreddit {
    pub name: String,
    pub last_updated: Option<chrono::DateTime<chrono::Utc>>,
    pub last_page: Option<String>,
    pub disabled: bool,
}

impl RedditSubreddit {
    pub async fn needing_update(conn: &sqlx::PgPool) -> Result<Vec<Self>, Error> {
        let subs = sqlx::query_file_as!(Self, "queries/reddit/needing_update.sql")
            .fetch_all(conn)
            .await?;

        Ok(subs)
    }

    pub async fn get_by_name(conn: &sqlx::PgPool, name: &str) -> Result<Option<Self>, Error> {
        let sub = sqlx::query_file_as!(Self, "queries/reddit/get_by_name.sql", name)
            .fetch_optional(conn)
            .await?;

        Ok(sub)
    }

    pub async fn update_position(
        conn: &sqlx::PgPool,
        name: &str,
        position: &str,
    ) -> Result<(), Error> {
        sqlx::query_file!("queries/reddit/update_position.sql", name, position)
            .execute(conn)
            .await?;

        Ok(())
    }

    pub async fn subreddits(conn: &sqlx::PgPool) -> Result<Vec<Self>, Error> {
        let subreddits = sqlx::query_file_as!(Self, "queries/reddit/subreddits.sql")
            .fetch_all(conn)
            .await?;

        Ok(subreddits)
    }

    pub async fn add(conn: &sqlx::PgPool, name: &str) -> Result<(), Error> {
        sqlx::query_file!("queries/reddit/add_subreddit.sql", name)
            .execute(conn)
            .await?;

        Ok(())
    }

    pub async fn update_state(
        conn: &sqlx::PgPool,
        name: &str,
        new_state: bool,
    ) -> Result<(), Error> {
        sqlx::query_file!("queries/reddit/update_state.sql", name, new_state)
            .execute(conn)
            .await?;

        Ok(())
    }
}

pub struct RedditPost {
    pub fullname: String,
    pub subreddit_name: String,
    pub posted_at: chrono::DateTime<chrono::Utc>,
    pub author: String,
    pub permalink: String,
    pub content_link: String,
}

impl RedditPost {
    pub async fn create(conn: &sqlx::PgPool, post: Self) -> Result<(), Error> {
        sqlx::query_file!(
            "queries/reddit/create_post.sql",
            post.fullname,
            post.subreddit_name,
            post.posted_at,
            post.author,
            post.permalink,
            post.content_link
        )
        .execute(conn)
        .await?;

        Ok(())
    }

    pub async fn exists(conn: &sqlx::PgPool, id: &str) -> Result<bool, Error> {
        let exists = sqlx::query_file_scalar!("queries/reddit/exists.sql", id)
            .fetch_one(conn)
            .await?;

        Ok(exists)
    }
}

pub struct RedditImage {
    pub post: RedditPost,

    pub id: Uuid,
    pub post_fullname: String,
    pub size: i32,
    pub sha256: [u8; 32],
    pub perceptual_hash: Option<[u8; 8]>,
}

impl RedditImage {
    pub async fn create(
        conn: &sqlx::PgPool,
        post_fullname: &str,
        size: i32,
        sha256: [u8; 32],
        perceptual_hash: Option<[u8; 8]>,
    ) -> Result<Option<Uuid>, Error> {
        let id = sqlx::query_file_scalar!(
            "queries/reddit/create_image.sql",
            post_fullname,
            size,
            sha256.to_vec(),
            perceptual_hash.map(|hash| i64::from_be_bytes(hash))
        )
        .fetch_optional(conn)
        .await?;

        Ok(id)
    }

    pub async fn similar_images(
        conn: &sqlx::PgPool,
        perceptual_hash: i64,
    ) -> Result<Vec<Self>, Error> {
        let images = sqlx::query_file!("queries/reddit/similar_images.sql", perceptual_hash, 3)
            .map(|row| Self {
                post: RedditPost {
                    fullname: row.fullname,
                    subreddit_name: row.subreddit_name,
                    posted_at: row.posted_at,
                    author: row.author,
                    permalink: row.permalink,
                    content_link: row.content_link,
                },
                id: row.id,
                post_fullname: row.post_fullname,
                size: row.size,
                sha256: row.sha256.try_into().expect("sha256 was wrong length"),
                perceptual_hash: row.perceptual_hash.map(|hash| hash.to_be_bytes()),
            })
            .fetch_all(conn)
            .await?;

        Ok(images)
    }
}

pub struct UserAllowlist {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub site: Site,
    pub site_username: String,
}

#[derive(Serialize)]
struct AllowlistQueryItem {
    site: String,
    site_username: String,
}

impl UserAllowlist {
    pub async fn is_allowed(
        conn: &sqlx::PgPool,
        owner_id: Uuid,
        site: Site,
        site_username: &str,
    ) -> Result<bool, Error> {
        let exists = sqlx::query_file_scalar!(
            "queries/user_allowlist/is_allowed.sql",
            owner_id,
            site.to_string(),
            site_username,
        )
        .fetch_one(conn)
        .await?;

        Ok(exists.unwrap_or(false))
    }

    pub async fn lookup_many<'a>(
        conn: &sqlx::PgPool,
        owner_id: Uuid,
        items: impl Iterator<Item = (Site, &'a str)>,
    ) -> Result<HashMap<(Site, String), Uuid>, Error> {
        let items: HashSet<_> = items.into_iter().collect();

        let query_items: Vec<_> = items
            .into_iter()
            .map(|item| AllowlistQueryItem {
                site: item.0.to_string(),
                site_username: item.1.to_string(),
            })
            .collect();

        let results: HashMap<_, _> = sqlx::query_file!(
            "queries/user_allowlist/lookup_many.sql",
            owner_id,
            serde_json::to_value(query_items)?
        )
        .map(|row| Self {
            id: row.id,
            owner_id: row.owner_id,
            site: row.site.parse().unwrap(),
            site_username: row.site_username,
        })
        .fetch_all(conn)
        .await?
        .into_iter()
        .map(|row| ((row.site, row.site_username.to_lowercase()), row.id))
        .collect();

        Ok(results)
    }

    pub async fn add(
        conn: &sqlx::PgPool,
        owner_id: Uuid,
        site: Site,
        site_username: &str,
    ) -> Result<Option<Uuid>, Error> {
        let id = sqlx::query_file_scalar!(
            "queries/user_allowlist/add.sql",
            owner_id,
            site.to_string(),
            site_username
        )
        .fetch_optional(conn)
        .await?;

        Ok(id)
    }

    pub async fn remove(
        conn: &sqlx::PgPool,
        owner_id: Uuid,
        site: Site,
        site_username: &str,
    ) -> Result<Option<Uuid>, Error> {
        let id = sqlx::query_file_scalar!(
            "queries/user_allowlist/remove.sql",
            owner_id,
            site.to_string(),
            site_username
        )
        .fetch_optional(conn)
        .await?;

        Ok(id)
    }
}

pub struct PendingNotification {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub user_event_id: Uuid,
}

impl PendingNotification {
    pub async fn create(
        conn: &sqlx::PgPool,
        owner_id: Uuid,
        user_event_id: Uuid,
    ) -> Result<Uuid, Error> {
        let id = sqlx::query_file_scalar!(
            "queries/pending_notification/create.sql",
            owner_id,
            user_event_id
        )
        .fetch_one(conn)
        .await?;

        Ok(id)
    }

    pub async fn ready(
        conn: &sqlx::PgPool,
        frequency: setting::Frequency,
    ) -> Result<Vec<Self>, Error> {
        let notifications = sqlx::query_file_as!(
            Self,
            "queries/pending_notification/ready.sql",
            serde_json::to_value(frequency)?
        )
        .fetch_all(conn)
        .await?;

        Ok(notifications)
    }

    pub async fn remove(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        notification_ids: &[Uuid],
    ) -> Result<(), Error> {
        sqlx::query_file!("queries/pending_notification/remove.sql", notification_ids)
            .execute(tx)
            .await?;

        Ok(())
    }
}

pub struct TwitterAuth {
    pub owner_id: Uuid,
    pub request_key: String,
    pub request_secret: String,
}

impl TwitterAuth {
    pub async fn create(
        conn: &sqlx::PgPool,
        owner_id: Uuid,
        request_key: &str,
        request_secret: &str,
    ) -> Result<(), Error> {
        sqlx::query_file!(
            "queries/twitter_auth/create.sql",
            owner_id,
            request_key,
            request_secret
        )
        .execute(conn)
        .await?;

        Ok(())
    }

    pub async fn find(conn: &sqlx::PgPool, request_key: &str) -> Result<Option<Self>, Error> {
        let twitter_auth = sqlx::query_file_as!(Self, "queries/twitter_auth/find.sql", request_key)
            .fetch_optional(conn)
            .await?;

        Ok(twitter_auth)
    }

    pub async fn remove(conn: &sqlx::PgPool, request_key: &str) -> Result<(), Error> {
        sqlx::query_file!("queries/twitter_auth/remove.sql", request_key,)
            .execute(conn)
            .await?;

        Ok(())
    }
}

pub struct FileUploadChunk;

impl FileUploadChunk {
    pub async fn add(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        collection_id: Uuid,
        size: i32,
    ) -> Result<i64, Error> {
        let sequence_number = sqlx::query_file_scalar!(
            "queries/file_upload_chunk/upload.sql",
            user_id,
            collection_id,
            size
        )
        .fetch_one(conn)
        .await?;

        Ok(sequence_number)
    }

    pub async fn size(conn: &sqlx::PgPool, user_id: Uuid) -> Result<i64, Error> {
        let size = sqlx::query_file_scalar!("queries/file_upload_chunk/size.sql", user_id)
            .fetch_optional(conn)
            .await?
            .unwrap_or_default()
            .unwrap_or(0);

        Ok(size)
    }

    pub async fn chunks(
        conn: &sqlx::PgPool,
        user_id: Uuid,
        collection_id: Uuid,
    ) -> Result<Vec<i64>, Error> {
        let sequence_nums =
            sqlx::query_file_scalar!("queries/file_upload_chunk/get.sql", user_id, collection_id)
                .fetch_all(conn)
                .await?;

        Ok(sequence_nums)
    }
}

pub trait UserSettingItem:
    Clone + Default + serde::Serialize + serde::de::DeserializeOwned
{
    const SETTING_NAME: &'static str;

    fn off_value() -> Self;
}

pub struct UserSetting;

impl UserSetting {
    pub async fn get<S>(conn: &sqlx::PgPool, owner_id: Uuid) -> Result<Option<S>, Error>
    where
        S: UserSettingItem,
    {
        let value =
            sqlx::query_file_scalar!("queries/user_setting/get.sql", owner_id, S::SETTING_NAME)
                .fetch_optional(conn)
                .await?;

        value
            .map(|value| S::deserialize(value))
            .transpose()
            .map_err(Into::into)
    }

    pub async fn set<S>(conn: &sqlx::PgPool, owner_id: Uuid, setting: &S) -> Result<(), Error>
    where
        S: UserSettingItem,
    {
        let value = serde_json::to_value(&setting)?;

        sqlx::query_file!(
            "queries/user_setting/set.sql",
            owner_id,
            S::SETTING_NAME,
            value
        )
        .execute(conn)
        .await?;

        Ok(())
    }
}

pub mod setting {
    use serde::{Deserialize, Serialize};

    use super::UserSettingItem;

    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct EmailNotifications(pub bool);

    impl Default for EmailNotifications {
        fn default() -> Self {
            Self(true)
        }
    }

    impl UserSettingItem for EmailNotifications {
        const SETTING_NAME: &'static str = "email-notifications";

        fn off_value() -> Self {
            Self(false)
        }
    }

    #[derive(Clone, Debug, Default, Serialize, Deserialize)]
    pub struct TelegramNotifications(pub bool);

    impl UserSettingItem for TelegramNotifications {
        const SETTING_NAME: &'static str = "telegram-notifications";

        fn off_value() -> Self {
            Self(false)
        }
    }

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "lowercase")]
    pub enum Frequency {
        Never,
        Instantly,
        Daily,
        Weekly,
    }

    impl Default for Frequency {
        fn default() -> Self {
            Self::Never
        }
    }

    impl Frequency {
        pub fn is_digest(&self) -> bool {
            matches!(self, Frequency::Daily | Frequency::Weekly)
        }
    }

    #[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
    pub struct EmailFrequency(pub Frequency);

    impl UserSettingItem for EmailFrequency {
        const SETTING_NAME: &'static str = "email-frequency";

        fn off_value() -> Self {
            Self(Frequency::Never)
        }
    }
}
