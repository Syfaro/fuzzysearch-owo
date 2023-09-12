use std::num::NonZeroU64;

use actix_web::{get, post, services, web, HttpResponse};
use askama::Template;
use async_trait::async_trait;
use foxlib::jobs::{FaktoryForge, FaktoryJob, FaktoryProducer, Job, JobExtra};
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use uuid::Uuid;

use crate::{
    jobs::{
        self, JobContext, JobInitiator, JobInitiatorExt, NewSubmissionJob, Queue,
        SearchExistingSubmissionsJob,
    },
    models::{self, LinkedAccount, Site},
    site::{
        reddit::limited_image_download, CollectedSite, SiteFromConfig, SiteServices, WatchedSite,
    },
    AsUrl, Error, WrappedTemplate,
};

pub struct BSky;

#[async_trait(?Send)]
impl SiteFromConfig for BSky {
    async fn site_from_config(_config: &crate::Config) -> Result<Self, Error> {
        Ok(Self)
    }
}

#[async_trait(?Send)]
impl WatchedSite for BSky {
    fn register_jobs(&self, forge: &mut FaktoryForge<jobs::JobContext, Error>) {
        LoadBlueskyPostJob::register(forge, load_bluesky_post);
    }
}

#[derive(Deserialize)]
struct BlueskyActorFeed {
    #[serde(rename = "feed")]
    entries: Vec<BlueskyFeedEntry>,
    cursor: Option<String>,
}

#[derive(Deserialize)]
struct BlueskyFeedEntry {
    post: BlueskyPost,
}

#[derive(Serialize, Deserialize)]
struct BlueskyPost {
    uri: String,
    author: BlueskyAuthor,
    record: Post,
}

#[derive(Serialize, Deserialize)]
struct BlueskyAuthor {
    did: String,
}

#[derive(Serialize, Deserialize)]
struct ImportSubmissionBlueskyJob {
    user_id: Uuid,
    account_id: Uuid,
    post: BlueskyPost,
}

impl Job for ImportSubmissionBlueskyJob {
    const NAME: &'static str = "bluesky_import";
    type Data = Self;
    type Queue = Queue;

    fn queue(&self) -> Self::Queue {
        Queue::Outgoing
    }

    fn extra(&self) -> Result<Option<JobExtra>, serde_json::Error> {
        Ok(None)
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        Ok(vec![serde_json::to_value(self)?])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::Data, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

async fn import_submission(
    ctx: JobContext,
    _job: FaktoryJob,
    ImportSubmissionBlueskyJob {
        user_id,
        account_id,
        post,
    }: ImportSubmissionBlueskyJob,
) -> Result<(), Error> {
    let (_, rkey) = post.uri.rsplit_once('/').ok_or(Error::Missing)?;

    for image in post
        .record
        .embed
        .and_then(|embed| match embed {
            PostEmbed::Images { images } => Some(images),
            _ => None,
        })
        .unwrap_or_default()
    {
        let image_cid = match image.image {
            File::Blob { link, .. } => link.link,
            File::Cid { cid, .. } => cid,
        };

        let url = format!(
            "https://bsky.social/xrpc/com.atproto.sync.getBlob?did={}&cid={image_cid}",
            post.author.did
        );

        let image_data = ctx.client.get(url).send().await?.bytes().await?;

        let mut sha256 = sha2::Sha256::new();
        sha256.update(&image_data);
        let sha256: [u8; 32] = sha256
            .finalize()
            .try_into()
            .expect("sha256 was wrong length");

        let (im, perceptual_hash) = if let Ok(im) = image::load_from_memory(&image_data) {
            let hasher = fuzzysearch_common::get_hasher();
            let hash: [u8; 8] = hasher
                .hash_image(&im)
                .as_bytes()
                .try_into()
                .expect("perceptual hash was wrong length");
            let perceptual_hash = i64::from_be_bytes(hash);

            (Some(im), Some(perceptual_hash))
        } else {
            (None, None)
        };

        let item_id = models::OwnedMediaItem::add_item(
            &ctx.conn,
            user_id,
            account_id,
            image_cid,
            perceptual_hash,
            sha256,
            Some(format!(
                "https://bsky.app/profile/{}/post/{}",
                post.author.did, rkey
            )),
            None,
            parse_time(&post.record.created_at),
        )
        .await?;

        if let Some(im) = im {
            models::OwnedMediaItem::update_media(&ctx.conn, &ctx.s3, &ctx.config, item_id, im)
                .await?;

            ctx.producer
                .enqueue_job(
                    SearchExistingSubmissionsJob {
                        user_id,
                        media_id: item_id,
                    }
                    .initiated_by(JobInitiator::user(user_id)),
                )
                .await?;
        }
    }

    let mut redis = ctx.redis.clone();
    super::update_import_progress(&ctx.conn, &mut redis, user_id, account_id, post.uri).await?;

    Ok(())
}

#[async_trait(?Send)]
impl CollectedSite for BSky {
    fn oauth_page(&self) -> Option<&'static str> {
        Some("/bluesky/auth")
    }

    fn register_jobs(&self, forge: &mut FaktoryForge<jobs::JobContext, Error>) {
        ImportSubmissionBlueskyJob::register(forge, import_submission)
    }

    #[tracing::instrument(skip_all, fields(user_id = %account.owner_id, account_id = %account.id))]
    async fn add_account(&self, ctx: &JobContext, account: LinkedAccount) -> Result<(), Error> {
        let data: BlueskySession = serde_json::from_value(account.data.unwrap_or_default())?;

        let resp: BlueskySession = ctx
            .client
            .post("https://bsky.social/xrpc/com.atproto.server.refreshSession")
            .bearer_auth(data.refresh_jwt)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let query = &[
            ("actor", data.did.clone()),
            ("limit", "100".to_string()),
            ("filter", "posts_with_media".to_string()),
        ];

        let mut cursor: Option<String> = None;

        let mut posts = Vec::new();

        loop {
            let mut query = query.to_vec();
            if let Some(cursor) = cursor {
                query.push(("cursor", cursor.to_string()))
            }
            tracing::debug!("loading feed with query: {query:?}");

            let feed: BlueskyActorFeed = ctx
                .client
                .get("https://bsky.social/xrpc/app.bsky.feed.getAuthorFeed")
                .bearer_auth(&resp.access_jwt)
                .query(&query)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            for entry in feed.entries {
                tracing::info!("found post: {}", entry.post.uri);
                posts.push(entry.post);
            }

            match feed.cursor {
                Some(cur) => cursor = Some(cur),
                None => {
                    tracing::info!("empty cursor, ending loop");
                    break;
                }
            }
        }

        tracing::info!("discovered {} submissions", posts.len());
        let ids = posts.iter().map(|post| &post.uri);

        let mut redis = ctx.redis.clone();

        super::set_loading_submissions(&ctx.conn, &mut redis, account.owner_id, account.id, ids)
            .await?;

        super::queue_new_submissions(
            &ctx.producer,
            account.owner_id,
            account.id,
            posts,
            |user_id, account_id, post| ImportSubmissionBlueskyJob {
                user_id,
                account_id,
                post,
            },
        )
        .await?;

        models::LinkedAccount::update_data(&ctx.conn, account.id, None).await?;

        Ok(())
    }
}

impl SiteServices for BSky {
    fn services() -> Vec<actix_web::Scope> {
        vec![web::scope("/bluesky").service(services![auth, auth_verify])]
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Post {
    pub created_at: String,
    pub text: String,
    pub embed: Option<PostEmbed>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", tag = "$type")]
pub enum PostEmbed {
    #[serde(rename = "app.bsky.embed.images")]
    Images { images: Vec<PostImage> },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostImage {
    pub alt: Option<String>,
    pub image: File,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum File {
    Blob {
        #[serde(rename = "ref")]
        link: Link,
        #[serde(rename = "mimeType")]
        mime_type: String,
        size: NonZeroU64,
    },
    Cid {
        cid: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    #[serde(rename = "$link")]
    pub link: String,
}

#[derive(Deserialize, Serialize)]
pub struct LoadBlueskyPostJob(PayloadData<Post>);

impl Job for LoadBlueskyPostJob {
    const NAME: &'static str = "bluesky_post";
    type Data = Self;
    type Queue = Queue;

    fn queue(&self) -> Self::Queue {
        Queue::Outgoing
    }

    fn extra(&self) -> Result<Option<JobExtra>, serde_json::Error> {
        Ok(None)
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        Ok(vec![serde_json::to_value(self)?])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::Data, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

fn parse_time(created_at: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    if let Ok(created_at) = chrono::DateTime::parse_from_rfc3339(created_at) {
        Some(created_at.into())
    } else if let Ok(created_at) = chrono::NaiveDateTime::parse_from_str(created_at, "%FT%T.%f") {
        Some(created_at.and_utc())
    } else {
        tracing::warn!("could not parse date: {}", created_at);
        None
    }
}

async fn load_bluesky_post(
    ctx: JobContext,
    _job: FaktoryJob,
    LoadBlueskyPostJob(payload): LoadBlueskyPostJob,
) -> Result<(), Error> {
    if let Some(PostEmbed::Images { images }) = payload.data.embed {
        tracing::debug!(repo = payload.repo, "got images: {images:?}");

        let (_collection, cid) = payload.path.split_once('/').ok_or(Error::Missing)?;

        let mut tx = ctx.conn.begin().await?;

        let created_at = parse_time(&payload.data.created_at);

        models::BlueskyPost::create_post(&mut tx, &payload.repo, cid, created_at).await?;

        for image in images {
            let image_cid = match image.image {
                File::Blob { link, .. } => link.link,
                File::Cid { cid, .. } => cid,
            };

            let url = format!(
                "https://bsky.social/xrpc/com.atproto.sync.getBlob?did={}&cid={image_cid}",
                payload.repo
            );

            let (sha256, data) =
                match limited_image_download(&ctx.client, &url, 50 * 1024 * 1024).await {
                    Ok(data) => data,
                    Err(err) => {
                        tracing::warn!("post did not appear to be image: {:?}", err);
                        continue;
                    }
                };

            let hash = if let Ok(im) = image::load_from_memory(&data) {
                let hasher = fuzzysearch_common::get_hasher();
                let hash: [u8; 8] = hasher
                    .hash_image(&im)
                    .as_bytes()
                    .try_into()
                    .expect("perceptual hash was wrong length");

                Some(hash)
            } else {
                tracing::warn!("could not calculate perceptual hash");

                None
            };

            models::BlueskyImage::create_image(
                &mut tx,
                &payload.repo,
                cid,
                &image_cid,
                data.len() as i64,
                &sha256,
                hash.map(i64::from_be_bytes),
            )
            .await?;

            let data = jobs::IncomingSubmission {
                site: models::Site::Bluesky,
                site_id: format!("{}:{}", payload.repo, payload.path),
                page_url: Some(format!(
                    "https://bsky.app/profile/{}/post/{}",
                    payload.repo, payload.path
                )),
                posted_by: Some(payload.repo.clone()),
                sha256: Some(sha256),
                perceptual_hash: hash,
                content_url: url,
                posted_at: None,
            };

            ctx.producer
                .enqueue_job(NewSubmissionJob(data).initiated_by(JobInitiator::external("bluesky")))
                .await?;

            tracing::info!("processed image {}, {hash:?}", hex::encode(sha256));
        }

        tx.commit().await?;
    }

    Ok(())
}

pub async fn ingest_bsky(ctx: JobContext) {
    tracing::info!("starting bsky ingester");

    let jetstream = async_nats::jetstream::new(ctx.nats);

    let stream = jetstream
        .get_stream("bsky-ingest")
        .await
        .expect("could not get ingest stream");

    let consumer = stream
        .get_or_create_consumer(
            "bsky-posts-owo",
            async_nats::jetstream::consumer::pull::Config {
                durable_name: Some("bsky-posts-owo".to_string()),
                filter_subject: "bsky.ingest.commit.*.app.bsky.feed.post".to_string(),
                ..Default::default()
            },
        )
        .await
        .expect("could not create consumer");

    let mut messages = consumer
        .messages()
        .await
        .expect("could not start messages from consumer");

    while let Ok(Some(message)) = messages.try_next().await {
        match message.subject.as_str() {
            "bsky.ingest.commit.create.app.bsky.feed.post" => {
                let payload: PayloadData<Post> = match serde_json::from_slice(&message.payload) {
                    Ok(data) => data,
                    Err(err) => {
                        tracing::warn!("{err}: {}", String::from_utf8_lossy(&message.payload));
                        continue;
                    }
                };

                if !matches!(payload.data.embed, Some(PostEmbed::Images { .. })) {
                    tracing::trace!("post did not contain images, skipping");
                    continue;
                }

                if let Err(err) = ctx
                    .producer
                    .enqueue_job(
                        LoadBlueskyPostJob(payload).initiated_by(JobInitiator::external("bluesky")),
                    )
                    .await
                {
                    tracing::error!("could not enqueue load post job: {err}");
                    message
                        .ack_with(async_nats::jetstream::AckKind::Nak(None))
                        .await
                        .expect("could not nak message");
                } else {
                    message.ack().await.expect("could not ack message");
                }
            }
            "bsky.ingest.commit.delete.app.bsky.feed.post" => {
                let payload: PayloadData<Option<serde_json::Value>> =
                    match serde_json::from_slice(&message.payload) {
                        Ok(data) => data,
                        Err(err) => {
                            tracing::warn!("{err}: {}", String::from_utf8_lossy(&message.payload));
                            continue;
                        }
                    };

                let Some((_collection, cid)) = payload.path.split_once('/') else {
                    tracing::error!("payload path was unexpected");
                    continue;
                };

                if let Err(err) =
                    models::BlueskyPost::delete_post(&ctx.conn, &payload.repo, cid).await
                {
                    tracing::error!("could not mark post as deleted: {err}");
                }

                message.ack().await.expect("could not ack message");
            }
            _ => continue,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct PayloadData<D> {
    repo: String,
    path: String,
    data: D,
}

#[derive(Template)]
#[template(path = "user/account/bluesky.html")]
struct BlueskyLink {}

#[get("/auth")]
async fn auth(request: actix_web::HttpRequest, user: models::User) -> Result<HttpResponse, Error> {
    let body = BlueskyLink {}.wrap(&request, Some(&user)).await.render()?;

    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[derive(Deserialize)]
struct BlueskyAuthForm {
    username: String,
    password: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BlueskySession {
    did: String,
    access_jwt: String,
    refresh_jwt: String,
}

#[post("/auth")]
async fn auth_verify(
    conn: web::Data<sqlx::PgPool>,
    faktory: web::Data<FaktoryProducer>,
    request: actix_web::HttpRequest,
    user: models::User,
    form: web::Form<BlueskyAuthForm>,
) -> Result<HttpResponse, Error> {
    let client = reqwest::Client::default();
    let resp: BlueskySession = client
        .post("https://bsky.social/xrpc/com.atproto.server.createSession")
        .json(&serde_json::json!({
            "identifier": form.username,
            "password": form.password,
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    tracing::debug!(did = resp.did, "got session for user credentials");

    let linked_account =
        match models::LinkedAccount::lookup_by_site_id(&conn, user.id, Site::Bluesky, &resp.did)
            .await?
        {
            Some(account) => account,
            None => {
                tracing::info!("got new account");

                let account = models::LinkedAccount::create(
                    &conn,
                    user.id,
                    Site::Bluesky,
                    &resp.did.clone(),
                    Some(serde_json::to_value(resp)?),
                )
                .await?;

                faktory
                    .enqueue_job(
                        jobs::AddAccountJob {
                            user_id: user.id,
                            account_id: account.id,
                        }
                        .initiated_by(jobs::JobInitiator::user(user.id)),
                    )
                    .await?;

                account
            }
        };

    Ok(HttpResponse::Found()
        .insert_header((
            "Location",
            request
                .url_for("user_account", [linked_account.id.as_url()])?
                .as_str(),
        ))
        .finish())
}
