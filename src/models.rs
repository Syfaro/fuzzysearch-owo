use std::{
    fmt::{Debug, Display},
    str::FromStr,
};

use argonautica::{Hasher, Verifier};
use image::GenericImageView;
use redis::AsyncCommands;
use rusoto_s3::S3;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use uuid::Uuid;

use crate::api;

pub struct User {
    pub id: Uuid,
    pub username: String,

    hashed_password: String,
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
    pub async fn lookup_by_id(
        conn: &sqlx::Pool<sqlx::Postgres>,
        id: Uuid,
    ) -> anyhow::Result<Option<User>> {
        let user = sqlx::query_file_as!(User, "queries/auth/user_lookup_id.sql", id)
            .fetch_optional(conn)
            .await?;

        Ok(user)
    }

    pub async fn lookup_by_login(
        conn: &sqlx::Pool<sqlx::Postgres>,
        username: &str,
        password: &str,
    ) -> anyhow::Result<Option<User>> {
        let user = sqlx::query_file_as!(User, "queries/auth/user_lookup_login.sql", username)
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

    pub async fn create(
        conn: &sqlx::Pool<sqlx::Postgres>,
        username: &str,
        password: &str,
    ) -> anyhow::Result<Uuid> {
        let password = Self::hash_password(password);

        let id = sqlx::query_file_scalar!("queries/auth/user_create.sql", username, password)
            .fetch_one(conn)
            .await?;

        Ok(id)
    }

    pub async fn username_exists(
        conn: &sqlx::Pool<sqlx::Postgres>,
        username: &str,
    ) -> anyhow::Result<bool> {
        let exists = sqlx::query_file_scalar!("queries/auth/user_username_exists.sql", username)
            .fetch_one(conn)
            .await?;

        Ok(exists)
    }

    fn verify_password(&self, input: &str) -> bool {
        let mut verifier = Verifier::default();

        verifier
            .with_hash(&self.hashed_password)
            .with_password(input)
            .verify()
            .unwrap_or(false)
    }

    fn hash_password(password: &str) -> String {
        let mut hasher = Hasher::default();
        hasher.opt_out_of_secret_key(true);

        hasher.with_password(password).hash().unwrap()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum SourceSite {
    FurAffinity,
}

impl Display for SourceSite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let display = match self {
            Self::FurAffinity => "FurAffinity",
        };

        f.write_str(display)
    }
}

impl FromStr for SourceSite {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let site = match value {
            "FurAffinity" => SourceSite::FurAffinity,
            _ => return Err("unknown source site"),
        };

        Ok(site)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OwnedMediaItem {
    pub id: Uuid,
    pub owner_id: Uuid,

    pub account_id: Option<Uuid>,
    pub source_id: Option<String>,

    pub perceptual_hash: Option<i64>,
    pub sha256_hash: [u8; 32],

    pub link: Option<String>,
    pub title: Option<String>,
    pub posted_at: Option<chrono::DateTime<chrono::Utc>>,

    pub last_modified: chrono::DateTime<chrono::Utc>,

    pub content_url: Option<String>,
    pub content_size: Option<i64>,
    pub thumb_url: Option<String>,
}

impl OwnedMediaItem {
    pub async fn get_by_id(
        conn: &sqlx::Pool<sqlx::Postgres>,
        id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Option<Self>> {
        let item = sqlx::query_file!("queries/owned_media/media_lookup_id.sql", id, user_id)
            .map(|row| OwnedMediaItem {
                id: row.id,
                owner_id: row.owner_id,
                account_id: row.account_id,
                source_id: row.source_id,
                perceptual_hash: row.perceptual_hash,
                sha256_hash: row.sha256_hash.try_into().unwrap(),
                link: row.link,
                title: row.title,
                posted_at: row.posted_at,
                last_modified: row.last_modified,
                content_url: row.content_url,
                content_size: row.content_size,
                thumb_url: row.thumb_url,
            })
            .fetch_optional(conn)
            .await?;

        Ok(item)
    }

    pub async fn user_item_count(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user_id: Uuid,
    ) -> anyhow::Result<(i64, i64)> {
        let stats = sqlx::query_file!("queries/owned_media/media_count.sql", user_id)
            .fetch_one(conn)
            .await?;

        Ok((stats.count, stats.total_content_size))
    }

    pub async fn add_manual_item(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user_id: Uuid,
        perceptual_hash: i64,
        sha256_hash: [u8; 32],
    ) -> anyhow::Result<Uuid> {
        let item_id = sqlx::query_file_scalar!(
            "queries/owned_media/manual_upload.sql",
            user_id,
            perceptual_hash,
            sha256_hash.to_vec()
        )
        .fetch_one(conn)
        .await?;

        Ok(item_id)
    }

    pub async fn add_item<ID: ToString>(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user_id: Uuid,
        account_id: Uuid,
        source_id: ID,
        perceptual_hash: Option<i64>,
        sha256_hash: [u8; 32],
        link: Option<String>,
        title: Option<String>,
        posted_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> anyhow::Result<Uuid> {
        let item = sqlx::query_file_scalar!(
            "queries/owned_media/media_import.sql",
            user_id,
            account_id,
            source_id.to_string(),
            perceptual_hash,
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
        conn: &sqlx::Pool<sqlx::Postgres>,
        s3: &rusoto_s3::S3Client,
        config: &crate::Config,
        id: Uuid,
        full_size: image::DynamicImage,
    ) -> anyhow::Result<()> {
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
            "queries/owned_media/media_set_urls.sql",
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
        conn: &sqlx::Pool<sqlx::Postgres>,
        user_id: Uuid,
    ) -> anyhow::Result<Vec<Self>> {
        let items = sqlx::query_file!("queries/owned_media/media_recent.sql", user_id)
            .map(|row| OwnedMediaItem {
                id: row.id,
                owner_id: row.owner_id,
                account_id: row.account_id,
                source_id: row.source_id,
                perceptual_hash: row.perceptual_hash,
                sha256_hash: row.sha256_hash.try_into().unwrap(),
                link: row.link,
                title: row.title,
                posted_at: row.posted_at,
                last_modified: row.last_modified,
                content_url: row.content_url,
                content_size: row.content_size,
                thumb_url: row.thumb_url,
            })
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
        conn: &sqlx::Pool<sqlx::Postgres>,
        perceptual_hash: i64,
    ) -> anyhow::Result<Vec<Self>> {
        let items = sqlx::query_file!(
            "queries/owned_media/media_similar_search.sql",
            perceptual_hash,
            3
        )
        .map(|row| OwnedMediaItem {
            id: row.id,
            owner_id: row.owner_id,
            account_id: row.account_id,
            source_id: row.source_id,
            perceptual_hash: row.perceptual_hash,
            sha256_hash: row.sha256_hash.try_into().unwrap(),
            link: row.link,
            title: row.title,
            posted_at: row.posted_at,
            last_modified: row.last_modified,
            content_url: row.content_url,
            content_size: row.content_size,
            thumb_url: row.thumb_url,
        })
        .fetch_all(conn)
        .await?;

        Ok(items)
    }

    pub async fn remove(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user_id: Uuid,
        media_id: Uuid,
    ) -> anyhow::Result<()> {
        sqlx::query_file!("queries/owned_media/media_remove.sql", user_id, media_id)
            .execute(conn)
            .await?;

        Ok(())
    }
}

async fn upload_image(
    s3: &rusoto_s3::S3Client,
    config: &crate::Config,
    im: image::DynamicImage,
) -> anyhow::Result<(String, usize)> {
    tracing::debug!("uploading image to s3");

    let mut buf = Vec::new();
    im.write_to(&mut buf, image::ImageOutputFormat::Jpeg(80))?;
    let size = buf.len();

    let mut hasher = sha2::Sha256::default();
    hasher.update(&buf);
    let hash: [u8; 32] = hasher.finalize().try_into().unwrap();
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

    s3.put_object(put).await?;

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
    Complete,
}

impl LoadingState {
    pub fn message(&self) -> String {
        match self {
            LoadingState::Unknown => "Loading Starting".to_string(),
            LoadingState::DiscoveringItems => "Discovering Items".to_string(),
            LoadingState::LoadingItems { known } => format!("Processing {} Items", known),
            LoadingState::Complete => "Loading Complete".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkedAccount {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub source_site: SourceSite,
    pub username: String,
    pub last_update: Option<chrono::DateTime<chrono::Utc>>,
    pub loading_state: Option<LoadingState>,
    pub credentials: Option<serde_json::Value>,
}

impl LinkedAccount {
    pub async fn create(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user_id: Uuid,
        source_site: SourceSite,
        username: &str,
        credentials: Option<serde_json::Value>,
    ) -> anyhow::Result<Uuid> {
        let id = sqlx::query_file_scalar!(
            "queries/account/account_link_create.sql",
            user_id,
            source_site.to_string(),
            username,
            credentials
        )
        .fetch_one(conn)
        .await?;

        Ok(id)
    }

    pub async fn lookup_by_id(
        conn: &sqlx::Pool<sqlx::Postgres>,
        id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Option<Self>> {
        let account = sqlx::query_file!("queries/account/account_link_lookup_id.sql", id, user_id)
            .map(|row| LinkedAccount {
                id: row.id,
                owner_id: row.owner_id,
                source_site: row.source_site.parse().unwrap(),
                username: row.username,
                last_update: row.last_update,
                loading_state: row
                    .loading_state
                    .and_then(|loading_state| serde_json::from_value(loading_state).ok()),
                credentials: row.credentials,
            })
            .fetch_optional(conn)
            .await?;

        Ok(account)
    }

    pub async fn owned_by_user(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user_id: Uuid,
    ) -> anyhow::Result<Vec<Self>> {
        let accounts = sqlx::query_file!("queries/account/account_link_owned_by_user.sql", user_id)
            .map(|row| LinkedAccount {
                id: row.id,
                owner_id: row.owner_id,
                source_site: row.source_site.parse().unwrap(),
                username: row.username,
                last_update: row.last_update,
                loading_state: row
                    .loading_state
                    .and_then(|loading_state| serde_json::from_value(loading_state).ok()),
                credentials: row.credentials,
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

    pub async fn update_loading_state(
        conn: &sqlx::Pool<sqlx::Postgres>,
        redis: &redis::aio::ConnectionManager,
        user_id: Uuid,
        account_id: Uuid,
        loading_state: LoadingState,
    ) -> anyhow::Result<()> {
        sqlx::query_file!(
            "queries/account/account_update_loading_state.sql",
            user_id,
            account_id,
            serde_json::to_value(&loading_state).unwrap(),
        )
        .execute(conn)
        .await?;

        let mut redis = redis.clone();
        redis
            .publish(
                format!("user-events:{}", user_id.to_string()),
                serde_json::to_string(&api::EventMessage::LoadingStateChange {
                    account_id,
                    loading_state: loading_state.message(),
                })
                .unwrap(),
            )
            .await?;

        Ok(())
    }

    pub async fn remove(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user_id: Uuid,
        account_id: Uuid,
    ) -> anyhow::Result<()> {
        sqlx::query_file!("queries/account/account_remove.sql", user_id, account_id)
            .execute(conn)
            .await?;

        Ok(())
    }

    pub async fn items(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user_id: Uuid,
        account_id: Uuid,
    ) -> anyhow::Result<(i64, i64)> {
        let stats = sqlx::query_file!(
            "queries/owned_media/account_media_count.sql",
            user_id,
            account_id
        )
        .fetch_one(conn)
        .await?;

        Ok((stats.count, stats.total_content_size))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Site {
    FurAffinity,
    #[serde(rename = "e621")]
    E621,
    Weasyl,
    Twitter,
}

impl Display for Site {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Site::FurAffinity => write!(f, "FurAffinity"),
            Site::E621 => write!(f, "e621"),
            Site::Weasyl => write!(f, "Weasyl"),
            Site::Twitter => write!(f, "Twitter"),
        }
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

    pub message: String,
    pub data: Option<UserEventData>,
}

impl UserEvent {
    pub async fn notify<M: AsRef<str>>(
        conn: &sqlx::Pool<sqlx::Postgres>,
        redis: &redis::aio::ConnectionManager,
        user_id: Uuid,
        message: M,
    ) -> anyhow::Result<Uuid> {
        let notification_id = sqlx::query_file_scalar!(
            "queries/user_event/event_create.sql",
            user_id,
            message.as_ref(),
            "user"
        )
        .fetch_one(conn)
        .await?;

        let mut redis = redis.clone();
        redis
            .publish(
                format!("user-events:{}", user_id.to_string()),
                serde_json::to_string(&api::EventMessage::SimpleMessage {
                    id: notification_id,
                    message: message.as_ref().to_string(),
                })
                .unwrap(),
            )
            .await?;

        Ok(notification_id)
    }

    pub async fn similar_found(
        conn: &sqlx::Pool<sqlx::Postgres>,
        redis: &redis::aio::ConnectionManager,
        user_id: Uuid,
        media_id: Uuid,
        similar_image: SimilarImage,
        created_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> anyhow::Result<Uuid> {
        let link = similar_image.best_link().to_owned();
        let data: UserEventData = similar_image.into();

        let notification_id = sqlx::query_file_scalar!(
            "queries/user_event/event_similar.sql",
            user_id,
            media_id,
            format!("Found similar image: {}", link),
            data.event_name(),
            serde_json::to_value(data).unwrap(),
            created_at,
        )
        .fetch_one(conn)
        .await?;

        if created_at.is_none() {
            let mut redis = redis.clone();
            redis
                .publish(
                    format!("user-events:{}", user_id.to_string()),
                    serde_json::to_string(&api::EventMessage::SimilarImage { media_id, link })
                        .unwrap(),
                )
                .await?;
        }

        Ok(notification_id)
    }

    pub async fn recent_events(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user_id: Uuid,
    ) -> anyhow::Result<Vec<Self>> {
        let events = sqlx::query_file!("queries/user_event/event_recent.sql", user_id)
            .map(|row| UserEvent {
                id: row.id,
                owner_id: row.owner_id,
                related_to_media_item_id: row.related_to_media_item_id,
                created_at: row.created_at,
                message: row.message,
                data: row.data.and_then(|data| serde_json::from_value(data).ok()),
            })
            .fetch_all(conn)
            .await?;

        Ok(events)
    }

    pub async fn recent_events_for_media(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user_id: Uuid,
        media_id: Uuid,
    ) -> anyhow::Result<Vec<Self>> {
        let events = sqlx::query_file!(
            "queries/user_event/event_recent_media.sql",
            user_id,
            media_id
        )
        .map(|row| UserEvent {
            id: row.id,
            owner_id: row.owner_id,
            related_to_media_item_id: row.related_to_media_item_id,
            created_at: row.created_at,
            message: row.message,
            data: row.data.and_then(|data| serde_json::from_value(data).ok()),
        })
        .fetch_all(conn)
        .await?;

        Ok(events)
    }

    pub fn display(&self) -> String {
        match self.data.as_ref() {
            Some(UserEventData::SimilarImage(similar)) => {
                match (similar.posted_by.as_ref(), similar.page_url.as_ref()) {
                    (Some(posted_by), Some(page_url)) => format!(
                        "Image posted by {} was found on {}: {}",
                        posted_by, similar.site, page_url
                    ),
                    (Some(posted_by), None) => format!(
                        "Image posted by {} was found on {}: {}",
                        posted_by, similar.site, similar.content_url
                    ),
                    (None, Some(page_url)) => {
                        format!("Image was found on {}: {}", similar.site, page_url)
                    }
                    (None, None) => format!(
                        "Image was found on {}: {}",
                        similar.site, similar.content_url
                    ),
                }
            }
            _ => "Unknown event".to_string(),
        }
    }
}
