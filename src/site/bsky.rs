use std::{borrow::Cow, num::NonZeroU64};

use actix_http::StatusCode;
use actix_session::Session;
use actix_web::{get, post, services, web, HttpResponse};
use askama::Template;
use async_trait::async_trait;
use foxlib::jobs::{FaktoryForge, FaktoryJob, FaktoryProducer, Job, JobExtra};
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use url::Url;
use uuid::Uuid;

use crate::{
    api::ResolvedDidResult,
    common,
    jobs::{
        self, JobContext, JobInitiator, JobInitiatorExt, NatsNewImage, NewSubmissionJob, Queue,
        SearchExistingSubmissionsJob,
    },
    models::{self, LinkedAccount, Site},
    site::{
        reddit::limited_image_download, CollectedSite, SiteFromConfig, SiteServices, WatchedSite,
    },
    AddFlash, AsUrl, Error, WrappedTemplate,
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
        ResolveDidJob::register(forge, resolve_did_impl);
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
        Queue::OutgoingBulk
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
    let account = models::LinkedAccount::lookup_by_id(&ctx.conn, account_id)
        .await?
        .ok_or(Error::Missing)?;
    let account_data: BlueskyAccountData =
        serde_json::from_value(account.data.ok_or(Error::Missing)?)?;

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
            "{}/xrpc/com.atproto.sync.getBlob?did={}&cid={image_cid}",
            account_data.server, post.author.did
        );

        let image_data = ctx.client.get(url).send().await?.bytes().await?;

        let mut sha256 = sha2::Sha256::new();
        sha256.update(&image_data);
        let sha256: [u8; 32] = sha256.finalize().into();

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

        let (item_id, is_new) = models::OwnedMediaItem::add_item(
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

        if is_new {
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
    }

    super::update_import_progress(&ctx.conn, &ctx.nats, user_id, account_id, post.uri).await?;

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
        let mut data: BlueskyAccountData =
            serde_json::from_value(account.data.unwrap_or_default())?;

        let session = data.session.ok_or(Error::Missing)?;

        let resp: BlueskySession = ctx
            .client
            .post(format!(
                "{}/xrpc/com.atproto.server.refreshSession",
                data.server
            ))
            .bearer_auth(session.refresh_jwt)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let query = &[
            ("actor", session.did.clone()),
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
                .get(format!("{}/xrpc/app.bsky.feed.getAuthorFeed", data.server))
                .bearer_auth(&resp.access_jwt)
                .query(&query)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            for entry in feed.entries {
                tracing::debug!("found post: {}", entry.post.uri);
                posts.push(entry.post);
            }

            match feed.cursor {
                Some(cur) => cursor = Some(cur),
                None => {
                    tracing::debug!("empty cursor, ending loop");
                    break;
                }
            }
        }

        tracing::info!("discovered {} submissions", posts.len());
        let ids = posts.iter().map(|post| &post.uri);

        super::set_loading_submissions(&ctx.conn, &ctx.nats, account.owner_id, account.id, ids)
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

        data.session = None;

        models::LinkedAccount::update_data(
            &ctx.conn,
            account.id,
            Some(serde_json::to_value(data)?),
        )
        .await?;

        Ok(())
    }
}

impl SiteServices for BSky {
    fn services() -> Vec<actix_web::Scope> {
        vec![web::scope("/bluesky").service(services![auth, auth_verify, resolve_did])]
    }
}

#[derive(Debug, Deserialize)]
struct ResolveQuery {
    did: String,
}

#[get("/resolve-did")]
async fn resolve_did(
    faktory: web::Data<FaktoryProducer>,
    user: models::User,
    web::Query(query): web::Query<ResolveQuery>,
) -> Result<HttpResponse, Error> {
    faktory
        .enqueue_job(
            ResolveDidJob {
                user_id: user.id,
                did: query.did.trim().to_string(),
            }
            .initiated_by(JobInitiator::User { user_id: user.id }),
        )
        .await?;

    Ok(HttpResponse::new(StatusCode::NO_CONTENT))
}

#[derive(Deserialize, Serialize)]
pub struct ResolveDidJob {
    user_id: Uuid,
    did: String,
}

impl Job for ResolveDidJob {
    const NAME: &'static str = "resolve_did";
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

async fn resolve_did_impl(
    ctx: JobContext,
    _job: FaktoryJob,
    ResolveDidJob { user_id, did }: ResolveDidJob,
) -> Result<(), Error> {
    let (Ok(event) | Err(event)) = resolve_did_to_event(&did)
        .await
        .map_err(|message| ResolvedDidResult::Error { message });

    common::send_user_event(
        user_id,
        &ctx.nats,
        crate::api::EventMessage::ResolvedDid { did, result: event },
    )
    .await?;

    Ok(())
}

#[tracing::instrument]
async fn resolve_did_to_event(did: &str) -> Result<ResolvedDidResult, String> {
    let client = reqwest::Client::default();

    let identity_doc = if let Some(domain) = did.strip_prefix("did:web:") {
        tracing::debug!("did was web");

        let url = Url::parse(&format!("https://{domain}"))
            .map_err(|err| format!("could not parse web domain: {err}"))?;

        if !matches!(url.origin(), url::Origin::Tuple(scheme, host, 443) if scheme == "https" && host.to_string() == domain)
        {
            return Err("web domain is not same as origin".to_string());
        }

        url.join("/.well-known/did.json")
            .map_err(|err| format!("could not join well-known path to origin: {err}"))?
    } else {
        let did: Cow<'_, _> = if did.starts_with("did:plc:") {
            tracing::debug!("did was already did:plc");

            did.into()
        } else {
            use futures::future::Either;
            use hickory_resolver::config::{ResolverConfig, ResolverOpts};

            tracing::debug!("did appeared to be domain, attempting to resolve");

            let resolver = hickory_resolver::TokioAsyncResolver::tokio(
                ResolverConfig::default(),
                ResolverOpts::default(),
            );

            let lookup_http = client
                .get(format!("https://{did}/.well-known/atproto-did"))
                .send();
            let lookup_dns = resolver.txt_lookup(format!("_atproto.{did}"));
            tokio::pin!(lookup_dns);

            let decode_http = |resp: reqwest::Response| async {
                resp.text()
                    .await
                    .map(Cow::from)
                    .map_err(|err| format!("could not load http response: {err}"))
            };

            let decode_dns = |dns_results: hickory_resolver::lookup::TxtLookup| async {
                let mut iter = dns_results.into_iter();
                let value = iter
                    .next()
                    .ok_or_else(|| "no dns records found".to_string())?
                    .to_string()
                    .strip_prefix("did=")
                    .ok_or_else(|| "unknown did value in dns record".to_string())?
                    .to_string();
                if iter.next().is_some() {
                    return Err("too many dns records".to_string());
                }
                Ok(Cow::from(value))
            };

            match futures::future::select(lookup_http, lookup_dns).await {
                Either::Left((http, dns_fut)) => {
                    tracing::debug!("http finished first");
                    match http.and_then(|resp| resp.error_for_status()) {
                        Ok(resp) => decode_http(resp).await?,
                        Err(err) => {
                            tracing::trace!("http request failed: {err}");
                            let dns_results = dns_fut
                                .await
                                .map_err(|err| format!("could not perform dns query: {err}"))?;
                            decode_dns(dns_results).await?
                        }
                    }
                }
                Either::Right((dns, http_fut)) => {
                    tracing::debug!("dns finished first");
                    match dns {
                        Ok(results) => match decode_dns(results).await {
                            Ok(results) => results,
                            Err(err) => {
                                tracing::debug!("dns lookup failed: {err}");
                                match http_fut.await {
                                    Ok(resp) => decode_http(resp).await?,
                                    Err(err) => {
                                        tracing::trace!("fallback http request failed: {err}");
                                        return Err("dns and http lookup failed".to_string());
                                    }
                                }
                            }
                        },
                        Err(err) => {
                            tracing::trace!("dns lookup failed: {err}");
                            match http_fut.await {
                                Ok(resp) => decode_http(resp).await?,
                                Err(err) => {
                                    tracing::trace!("fallback http request failed: {err}");
                                    return Err("dns and http lookup failed".to_string());
                                }
                            }
                        }
                    }
                }
            }
        };

        Url::parse(&format!("https://plc.directory/{did}"))
            .map_err(|err| format!("could not construct plc request: {err}"))?
    };

    tracing::info!(%identity_doc, "found identity doc");

    let identity: IdentityDocument = client
        .get(identity_doc)
        .send()
        .await
        .map_err(|err| format!("could not load identity document: {err}"))?
        .json()
        .await
        .map_err(|err| format!("identity document was not valid json: {err}"))?;

    if did.starts_with("did:") && identity.id != did {
        return Err("returned id did not match provided id".to_string());
    }

    let also_known_as = identity
        .also_known_as
        .first()
        .map(|s| s.as_str())
        .unwrap_or(did)
        .to_string();

    let service_endpoint = identity
        .service
        .into_iter()
        .find(|service| service.id == "#atproto_pds")
        .ok_or_else(|| "missing atproto service".to_string())?
        .service_endpoint;

    Ok(ResolvedDidResult::Success {
        also_known_as,
        service_endpoint,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdentityDocument {
    id: String,
    also_known_as: Vec<String>,
    service: Vec<IdentityService>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdentityService {
    id: String,
    service_endpoint: String,
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
        Queue::OutgoingBulk
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

        let (_collection, rkey) = payload.path.split_once('/').ok_or(Error::Missing)?;

        let mut tx = ctx.conn.begin().await?;

        let created_at = parse_time(&payload.data.created_at);

        if !models::BlueskyPost::create_post(&mut tx, &payload.repo, rkey, created_at).await? {
            tracing::debug!("post already existed, skipping");
            return Ok(());
        }

        for image in images {
            let image_cid = match image.image {
                File::Blob { link, .. } => link.link,
                File::Cid { cid, .. } => cid,
            };

            let url = format!(
                "https://cdn.bsky.app/img/feed_fullsize/plain/{repo}/{image_cid}@jpeg",
                repo = payload.repo
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
                rkey,
                &image_cid,
                data.len() as i64,
                &sha256,
                hash.map(i64::from_be_bytes),
            )
            .await?;

            let data = jobs::IncomingSubmission {
                site: models::Site::Bluesky,
                site_id: format!("{}:{}", payload.repo, rkey),
                page_url: Some(format!(
                    "https://bsky.app/profile/{}/post/{}",
                    payload.repo, rkey
                )),
                posted_by: Some(payload.repo.clone()),
                sha256: Some(sha256),
                perceptual_hash: hash,
                content_url: url.clone(),
                posted_at: created_at,
            };

            ctx.producer
                .enqueue_job(NewSubmissionJob(data).initiated_by(JobInitiator::external("bluesky")))
                .await?;

            ctx.nats_new_image(NatsNewImage {
                site: jobs::NatsSite::Bluesky,
                image_url: url,
                page_url: Some(format!(
                    "https://bsky.app/profile/{}/post/{}",
                    payload.repo, rkey
                )),
                posted_by: Some(payload.repo.clone()),
                perceptual_hash: hash,
                sha256_hash: Some(sha256),
            })
            .await;

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
                        if let Err(err) =
                            message.ack_with(async_nats::jetstream::AckKind::Term).await
                        {
                            tracing::error!("could not term message: {err:?}");
                        }
                        continue;
                    }
                };

                if !matches!(payload.data.embed, Some(PostEmbed::Images { .. })) {
                    tracing::trace!("post did not contain images, skipping");
                    if let Err(err) = message.ack().await {
                        tracing::error!("could not ack empty message: {err:?}");
                    }
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
                            if let Err(err) =
                                message.ack_with(async_nats::jetstream::AckKind::Term).await
                            {
                                tracing::error!("could not term message: {err:?}");
                            }
                            continue;
                        }
                    };

                let Some((_collection, rkey)) = payload.path.split_once('/') else {
                    tracing::error!("payload path was unexpected");
                    if let Err(err) = message.ack_with(async_nats::jetstream::AckKind::Term).await {
                        tracing::error!("could not term message: {err:?}");
                    }
                    continue;
                };

                if let Err(err) =
                    models::BlueskyPost::delete_post(&ctx.conn, &payload.repo, rkey).await
                {
                    tracing::error!("could not mark post as deleted: {err}");
                }

                if let Err(err) = message.ack().await {
                    tracing::error!("could not ack message: {err:?}");
                };
            }
            subject => {
                if let Err(err) = message.ack().await {
                    tracing::error!(subject, "could not ack message: {err:?}");
                };
            }
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
    server: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BlueskySession {
    did: String,
    access_jwt: String,
    refresh_jwt: String,
}

#[derive(Serialize, Deserialize)]
struct BlueskyAccountData {
    site_id: String,
    server: String,
    session: Option<BlueskySession>,
}

#[post("/auth")]
async fn auth_verify(
    conn: web::Data<sqlx::PgPool>,
    faktory: web::Data<FaktoryProducer>,
    request: actix_web::HttpRequest,
    session: Session,
    user: models::User,
    form: web::Form<BlueskyAuthForm>,
) -> Result<HttpResponse, Error> {
    let client = reqwest::Client::default();

    let resp = client
        .post(format!(
            "{}/xrpc/com.atproto.server.createSession",
            form.server
        ))
        .json(&serde_json::json!({
            "identifier": form.username,
            "password": form.password,
        }))
        .send()
        .await?;

    if resp.status() == StatusCode::UNAUTHORIZED {
        session.add_flash(
            crate::FlashStyle::Error,
            "Unauthorized, check that your username and password are correct.",
        );

        let body = BlueskyLink {}.wrap(&request, Some(&user)).await.render()?;

        return Ok(HttpResponse::Ok().content_type("text/html").body(body));
    }

    let resp: BlueskySession = resp.error_for_status()?.json().await?;
    tracing::debug!(did = resp.did, "got session for user credentials");

    let linked_account =
        match models::LinkedAccount::lookup_by_site_id(&conn, user.id, Site::Bluesky, &resp.did)
            .await?
        {
            Some(mut account) => {
                tracing::info!("account already existed, updating data");

                let data = serde_json::to_value(BlueskyAccountData {
                    server: form.server.clone(),
                    site_id: resp.did,
                    session: None,
                })?;

                models::LinkedAccount::update_data(&**conn, account.id, Some(data.clone())).await?;
                account.data = Some(data);
                account
            }
            None => {
                tracing::info!("got new account");

                let account = models::LinkedAccount::create(
                    &conn,
                    user.id,
                    Site::Bluesky,
                    &resp.did.clone(),
                    Some(serde_json::to_value(BlueskyAccountData {
                        site_id: resp.did.clone(),
                        server: form.server.clone(),
                        session: Some(resp),
                    })?),
                    None,
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
