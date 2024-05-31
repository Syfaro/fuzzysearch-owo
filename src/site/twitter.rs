use std::{
    collections::HashMap,
    io::{Read, Seek},
};

use actix_web::{get, post, services, web, HttpResponse};
use async_trait::async_trait;
use egg_mode::entities::MediaType;
use foxlib::jobs::{FaktoryForge, FaktoryJob, FaktoryProducer, Job, JobExtra, JobQueue};
use rusoto_s3::S3;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use uuid::Uuid;

use crate::{
    jobs::{self, JobContext, JobInitiator, JobInitiatorExt, Queue, SearchExistingSubmissionsJob},
    models::{self, LinkedAccount},
    AddFlash, AsUrl, Config, Error,
};

use super::{CollectedSite, SiteFromConfig, SiteServices};

pub struct Twitter {
    consumer_key: String,
    consumer_secret: String,

    redirect_url: String,
}

#[async_trait(?Send)]
impl SiteFromConfig for Twitter {
    async fn site_from_config(config: &Config) -> Result<Self, Error> {
        Ok(Self {
            consumer_key: config.twitter_consumer_key.clone(),
            consumer_secret: config.twitter_consumer_secret.clone(),
            redirect_url: format!("{}/twitter/callback", config.host_url),
        })
    }
}

#[async_trait(?Send)]
impl CollectedSite for Twitter {
    fn oauth_page(&self) -> Option<&'static str> {
        Some("/twitter/auth")
    }

    fn register_jobs(&self, forge: &mut FaktoryForge<jobs::JobContext, Error>) {
        AddSubmissionTwitterJob::register(forge, add_submission_twitter);
        CollectAccountsJob::register(forge, collect_accounts);
        UpdateAccountJob::register(forge, update_account);
        LoadTwitterArchiveJob::register(forge, load_archive);
    }

    async fn add_account(
        &self,
        ctx: &jobs::JobContext,
        account: LinkedAccount,
    ) -> Result<(), Error> {
        models::LinkedAccount::update_loading_state(
            &ctx.conn,
            &ctx.nats,
            account.owner_id,
            account.id,
            models::LoadingState::DiscoveringItems,
        )
        .await?;

        let data: TwitterData = serde_json::from_value(account.data.ok_or(Error::Missing)?)?;

        let consumer =
            egg_mode::KeyPair::new(self.consumer_key.clone(), self.consumer_secret.clone());
        let access = egg_mode::KeyPair::new(data.access_token.clone(), data.secret_token.clone());

        let token = egg_mode::Token::Access { consumer, access };

        let user_id = data
            .site_id
            .parse::<u64>()
            .map_err(|_err| Error::unknown_message("Twitter ID was not number"))?;

        let mut prev_timeline =
            egg_mode::tweet::user_timeline(user_id, true, true, &token).with_page_size(200);

        let mut tweets = Vec::new();
        let mut checked_tweets = 0;

        let mut max_id = None;

        loop {
            let (timeline, feed) = prev_timeline.older(None).await?;
            tracing::info!(
                "loaded timeline, min_id: {:?}, max_id: {:?}",
                timeline.min_id,
                timeline.max_id
            );
            max_id = match (timeline.max_id, max_id) {
                (Some(tl_max_id), Some(prev_max_id)) if tl_max_id > prev_max_id => Some(tl_max_id),
                (Some(tl_max_id), None) => Some(tl_max_id),
                _ => max_id,
            };
            tracing::debug!("updated max_id to {:?}", max_id);

            if feed.is_empty() {
                break;
            }

            for tweet in feed {
                checked_tweets += 1;

                let tweet_id = tweet.id.to_string();
                let screen_name = tweet
                    .user
                    .as_ref()
                    .map(|user| user.screen_name.clone())
                    .unwrap_or_default();
                let posted_at = tweet.created_at;

                let photos = Self::filter_tweets(user_id, tweet.response);

                if photos.is_empty() {
                    continue;
                }

                tracing::trace!("discovered tweet with {} photos", photos.len());

                tweets.push(SavedTweet {
                    id: tweet_id,
                    screen_name,
                    posted_at,
                    photos,
                });
            }

            tracing::debug!(
                "discovered {} tweets with photos in {} tweets",
                tweets.len(),
                checked_tweets
            );

            prev_timeline = timeline;
        }

        let data = serde_json::to_value(TwitterData {
            newest_id: max_id,
            ..data
        })?;

        models::LinkedAccount::update_data(&ctx.conn, account.id, Some(data)).await?;

        let mut redis = ctx.redis.clone();

        super::set_loading_submissions(
            &ctx.conn,
            &mut redis,
            &ctx.nats,
            account.owner_id,
            account.id,
            tweets.iter().map(|tweet| tweet.id.clone()),
        )
        .await?;

        super::queue_new_submissions(
            &ctx.producer,
            account.owner_id,
            account.id,
            tweets,
            |user_id, account_id, tweet| AddSubmissionTwitterJob {
                user_id,
                account_id,
                tweet,
                was_import: true,
            },
        )
        .await?;

        Ok(())
    }
}

impl SiteServices for Twitter {
    fn services() -> Vec<actix_web::Scope> {
        vec![web::scope("/twitter").service(services![auth, callback, archive_post])]
    }
}

impl Twitter {
    fn filter_tweets(user_id: u64, tweet: egg_mode::tweet::Tweet) -> HashMap<u64, String> {
        let tweet = match &tweet.retweeted_status {
            Some(status) => status,
            _ => &tweet,
        };

        if tweet.user.as_ref().map(|user| user.id) != Some(user_id) {
            return Default::default();
        }

        let extended_entities = tweet
            .extended_entities
            .as_ref()
            .map(|entities| &entities.media as &[egg_mode::entities::MediaEntity])
            .unwrap_or_default();

        extended_entities
            .iter()
            .filter(|entity| entity.media_type == MediaType::Photo)
            .map(|photo| (photo.id, photo.media_url_https.clone()))
            .collect()
    }
}

#[get("/auth")]
async fn auth(
    config: web::Data<Config>,
    conn: web::Data<sqlx::PgPool>,
    user: models::User,
) -> Result<HttpResponse, Error> {
    let twitter = Twitter::site_from_config(&config).await?;

    let token = egg_mode::KeyPair::new(twitter.consumer_key, twitter.consumer_secret);
    let request_token = egg_mode::auth::request_token(&token, &twitter.redirect_url).await?;

    models::TwitterAuth::create(&conn, user.id, &request_token.key, &request_token.secret).await?;

    let url = egg_mode::auth::authorize_url(&request_token);

    Ok(HttpResponse::Found()
        .insert_header(("Location", url))
        .finish())
}

#[derive(Deserialize)]
struct TwitterVerifier {
    oauth_token: Option<String>,
    oauth_verifier: Option<String>,
}

#[get("/callback")]
async fn callback(
    config: web::Data<Config>,
    conn: web::Data<sqlx::PgPool>,
    faktory: web::Data<FaktoryProducer>,
    request: actix_web::HttpRequest,
    user: models::User,
    web::Query(verifier): web::Query<TwitterVerifier>,
) -> Result<HttpResponse, Error> {
    let twitter = Twitter::site_from_config(&config).await?;

    let (oauth_token, oauth_verifier) = match (verifier.oauth_token, verifier.oauth_verifier) {
        (Some(oauth_token), Some(oauth_verifier)) => (oauth_token, oauth_verifier),
        _ => {
            return Err(Error::user_error(
                "Twitter did not provide authorization data",
            ));
        }
    };

    let twitter_auth = models::TwitterAuth::find(&conn, &oauth_token)
        .await?
        .ok_or_else(|| Error::unknown_message("Unknown Twitter token"))?;

    let con_token = egg_mode::KeyPair::new(twitter.consumer_key, twitter.consumer_secret);
    let request_token = egg_mode::KeyPair::new(
        twitter_auth.request_key.clone(),
        twitter_auth.request_secret,
    );

    let (token, user_id, username) =
        egg_mode::auth::access_token(con_token, &request_token, oauth_verifier).await?;

    models::TwitterAuth::remove(&conn, &twitter_auth.request_key).await?;

    let access = match token {
        egg_mode::Token::Access { access, .. } => access,
        _ => unreachable!("token should always be accesss"),
    };

    let user_id = user_id.to_string();

    let saved_data = serde_json::to_value(TwitterData {
        site_id: user_id.clone(),
        access_token: access.key.to_string(),
        secret_token: access.secret.to_string(),
        ..Default::default()
    })?;

    let account =
        models::LinkedAccount::lookup_by_site_id(&conn, user.id, models::Site::Twitter, &user_id)
            .await?;

    let id = match account {
        Some(account) => {
            tracing::info!("found existing account");
            let saved_data = match account.data.map(serde_json::from_value) {
                Some(Ok(data)) => serde_json::to_value(TwitterData {
                    access_token: access.key.to_string(),
                    secret_token: access.secret.to_string(),
                    ..data
                })?,
                Some(Err(err)) => return Err(err.into()),
                None => saved_data,
            };
            models::LinkedAccount::update_data(&conn, account.id, Some(saved_data)).await?;

            account.id
        }
        None => {
            tracing::info!("new account");
            let account = models::LinkedAccount::create(
                &conn,
                user.id,
                models::Site::Twitter,
                &username,
                Some(saved_data),
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

            account.id
        }
    };

    Ok(HttpResponse::Found()
        .insert_header((
            "Location",
            request.url_for("user_account", [id.as_url()])?.as_str(),
        ))
        .finish())
}

#[derive(Deserialize)]
struct TwitterArchiveForm {
    collection_id: Uuid,
    account_id: Uuid,
}

#[post("/archive")]
async fn archive_post(
    conn: web::Data<sqlx::PgPool>,
    nats: web::Data<async_nats::Client>,
    faktory: web::Data<FaktoryProducer>,
    request: actix_web::HttpRequest,
    session: actix_session::Session,
    user: models::User,
    form: web::Form<TwitterArchiveForm>,
) -> Result<HttpResponse, Error> {
    let account = models::LinkedAccount::lookup_by_id(&conn, form.account_id)
        .await?
        .ok_or(Error::Missing)?;

    if account.owner_id != user.id {
        return Err(Error::Unauthorized);
    }

    if account.source_site != models::Site::Twitter {
        return Err(Error::unknown_message(
            "Attempted to load Twitter archive on non-Twitter site",
        ));
    }

    let data: TwitterData = serde_json::from_value(account.data.ok_or(Error::Missing)?)?;

    faktory
        .enqueue_job(
            LoadTwitterArchiveJob {
                user_id: user.id,
                account_id: account.id,
                twitter_user_id: data.site_id.clone(),
                collection_id: form.collection_id,
            }
            .initiated_by(jobs::JobInitiator::user(user.id)),
        )
        .await?;

    let data = serde_json::to_value(TwitterData {
        has_imported_archive: Some(true),
        ..data
    })?;

    models::LinkedAccount::update_data(&conn, account.id, Some(data)).await?;
    models::LinkedAccount::update_loading_state(
        &conn,
        &nats,
        user.id,
        account.id,
        models::LoadingState::Custom {
            message: "Importing Archive".to_string(),
        },
    )
    .await?;

    session.add_flash(crate::FlashStyle::Success, "Successfully uploaded Twitter archive! It may take up to an hour for all of your photos to be imported.".to_string());

    Ok(HttpResponse::Found()
        .insert_header((
            "Location",
            request
                .url_for("user_account", [account.id.as_url()])?
                .as_str(),
        ))
        .finish())
}

#[derive(Default, Serialize, Deserialize)]
struct TwitterData {
    site_id: String,
    access_token: String,
    secret_token: String,
    newest_id: Option<u64>,
    has_imported_archive: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SavedTweet {
    pub id: String,
    pub screen_name: String,
    pub posted_at: chrono::DateTime<chrono::Utc>,
    pub photos: HashMap<u64, String>,
}

#[derive(Serialize, Deserialize)]
pub struct AddSubmissionTwitterJob {
    pub user_id: Uuid,
    pub account_id: Uuid,
    pub tweet: SavedTweet,
    pub was_import: bool,
}

impl Job for AddSubmissionTwitterJob {
    const NAME: &'static str = "add_submission_twitter";
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

#[tracing::instrument(skip(ctx, job), fields(job_id = job.id()))]
async fn add_submission_twitter(
    ctx: JobContext,
    job: FaktoryJob,
    AddSubmissionTwitterJob {
        user_id,
        account_id,
        tweet,
        was_import,
    }: AddSubmissionTwitterJob,
) -> Result<(), Error> {
    for (photo_id, photo_url) in tweet.photos {
        let image_data = ctx.client.get(photo_url).send().await?.bytes().await?;

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
            format!("{}-{}", tweet.id, photo_id),
            perceptual_hash,
            sha256,
            Some(format!(
                "https://twitter.com/{}/status/{}",
                tweet.screen_name, tweet.id
            )),
            None,
            Some(tweet.posted_at),
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

    if was_import {
        let mut redis = ctx.redis.clone();
        super::update_import_progress(
            &ctx.conn, &mut redis, &ctx.nats, user_id, account_id, tweet.id,
        )
        .await?;
    }

    Ok(())
}

pub struct CollectAccountsJob;

impl Job for CollectAccountsJob {
    const NAME: &'static str = "twitter_accounts";
    type Data = ();
    type Queue = Queue;

    fn queue(&self) -> Self::Queue {
        Queue::Core
    }

    fn extra(&self) -> Result<Option<JobExtra>, serde_json::Error> {
        Ok(None)
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        Ok(vec![])
    }

    fn deserialize(_args: Vec<serde_json::Value>) -> Result<Self::Data, serde_json::Error> {
        Ok(())
    }
}

async fn collect_accounts(ctx: JobContext, _job: FaktoryJob, _args: ()) -> Result<(), Error> {
    let account_ids =
        models::LinkedAccount::all_site_accounts(&ctx.conn, models::Site::Twitter).await?;

    for account_id in account_ids {
        if let Err(err) = ctx
            .producer
            .enqueue_job(UpdateAccountJob(account_id).initiated_by(JobInitiator::Schedule))
            .await
        {
            tracing::error!("could not enqueue twitter account check: {:?}", err);
        }
    }

    Ok(())
}

pub struct UpdateAccountJob(Uuid);

impl Job for UpdateAccountJob {
    const NAME: &'static str = "twitter_account_update";
    type Data = Uuid;
    type Queue = Queue;

    fn queue(&self) -> Self::Queue {
        Queue::Outgoing
    }

    fn extra(&self) -> Result<Option<JobExtra>, serde_json::Error> {
        Ok(None)
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        Ok(vec![serde_json::to_value(self.0)?])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::Data, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

async fn update_account(ctx: JobContext, _job: FaktoryJob, account_id: Uuid) -> Result<(), Error> {
    let account = models::LinkedAccount::lookup_by_id(&ctx.conn, account_id)
        .await?
        .ok_or(Error::Missing)?;

    let data: TwitterData = serde_json::from_value(account.data.ok_or(Error::Missing)?)?;

    let twitter = Twitter::site_from_config(&ctx.config).await?;

    let consumer = egg_mode::KeyPair::new(
        twitter.consumer_key.clone(),
        twitter.consumer_secret.clone(),
    );
    let access = egg_mode::KeyPair::new(data.access_token.clone(), data.secret_token.clone());

    let token = egg_mode::Token::Access { consumer, access };

    let user_id = data
        .site_id
        .parse::<u64>()
        .map_err(|_err| Error::unknown_message("Twitter ID was not number"))?;

    let mut prev_timeline =
        egg_mode::tweet::user_timeline(user_id, true, true, &token).with_page_size(200);

    let mut max_id = data.newest_id;

    loop {
        let (timeline, feed) = prev_timeline.older(data.newest_id).await?;
        tracing::info!(
            "loaded timeline, min_id: {:?}, max_id: {:?}",
            timeline.min_id,
            timeline.max_id
        );
        max_id = match (timeline.max_id, max_id) {
            (Some(tl_max_id), Some(prev_max_id)) if tl_max_id > prev_max_id => Some(tl_max_id),
            (Some(tl_max_id), None) => Some(tl_max_id),
            _ => max_id,
        };
        tracing::debug!("updated max_id to {:?}", max_id);

        if feed.is_empty() {
            break;
        }

        for tweet in feed {
            let tweet_id = tweet.id.to_string();
            let screen_name = tweet
                .user
                .as_ref()
                .map(|user| user.screen_name.clone())
                .unwrap_or_default();
            let posted_at = tweet.created_at;

            let photos = Twitter::filter_tweets(user_id, tweet.response);

            if photos.is_empty() {
                continue;
            }

            let saved_tweet = SavedTweet {
                id: tweet_id,
                screen_name,
                posted_at,
                photos,
            };

            ctx.producer
                .enqueue_job(
                    AddSubmissionTwitterJob {
                        user_id: account.owner_id,
                        account_id,
                        tweet: saved_tweet,
                        was_import: false,
                    }
                    .initiated_by(JobInitiator::Schedule),
                )
                .await?;
        }

        prev_timeline = timeline;
    }

    tracing::debug!("updating account data max id: {:?}", max_id);
    let data = serde_json::to_value(TwitterData {
        newest_id: max_id,
        ..data
    })?;

    models::LinkedAccount::update_data(&ctx.conn, account.id, Some(data)).await?;

    Ok(())
}

#[derive(Serialize, Deserialize)]
pub struct LoadTwitterArchiveJob {
    user_id: Uuid,
    account_id: Uuid,
    twitter_user_id: String,
    collection_id: Uuid,
}

impl Job for LoadTwitterArchiveJob {
    const NAME: &'static str = "twitter_load_archive";
    type Data = Self;
    type Queue = Queue;

    fn queue(&self) -> Self::Queue {
        Queue::Core
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

    fn job(self) -> Result<FaktoryJob, serde_json::Error> {
        let queue = self.queue().queue_name();
        let custom = foxlib::jobs::job_custom(self.extra()?.unwrap_or_default());
        let args = self.args()?;

        let mut job = FaktoryJob::new(Self::NAME, args).on_queue(queue.as_ref());
        job.custom = custom;
        job.reserve_for = Some(60 * 60);

        Ok(job)
    }
}

#[derive(Debug, Deserialize)]
struct ArchiveEntry {
    tweet: ArchiveTweet,
}

#[derive(Debug, Deserialize)]
struct ArchiveTweet {
    id: String,
    created_at: String,
    full_text: String,
    entities: Option<ArchiveTweetEntities>,
}

#[derive(Debug, Default, Deserialize)]
struct ArchiveTweetEntities {
    media: Option<Vec<TwitterArchiveMedia>>,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum TwitterArchiveMediaType {
    Photo,
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct TwitterArchiveMedia {
    #[serde(rename = "type")]
    media_type: TwitterArchiveMediaType,
    id: String,
    media_url_https: String,
}

#[derive(Debug)]
struct TweetToSave {
    tweet_id: u64,
    photo_id: u64,
    posted_at: chrono::DateTime<chrono::Utc>,
    screen_name: String,
    sha256: [u8; 32],
    perceptual_hash: Option<i64>,
    im: Option<image::DynamicImage>,
}

#[derive(Debug, thiserror::Error)]
enum TweetError {
    #[error("missing")]
    Missing,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid data: {0}")]
    InvalidData(&'static str),
}

async fn load_archive(
    ctx: JobContext,
    _job: FaktoryJob,
    LoadTwitterArchiveJob {
        user_id,
        account_id,
        collection_id,
        ..
    }: LoadTwitterArchiveJob,
) -> Result<(), Error> {
    let account = models::LinkedAccount::lookup_by_id(&ctx.conn, account_id)
        .await?
        .ok_or(Error::Missing)?;

    let chunks = models::FileUploadChunk::chunks(&ctx.conn, user_id, collection_id).await?;
    tracing::info!("attempting to load archive from {} chunks", chunks.len());

    let mut file = tokio::fs::File::from_std(web::block(tempfile::tempfile).await.unwrap()?);

    for chunk in chunks.iter() {
        tracing::trace!("downloading chunk {chunk}");

        let path = format!("tmp/{user_id}/{collection_id}-{chunk}");

        let get = rusoto_s3::GetObjectRequest {
            bucket: ctx.config.s3_bucket.clone(),
            key: path.clone(),
            ..Default::default()
        };

        let obj = ctx
            .s3
            .get_object(get)
            .await
            .map_err(|err| Error::S3(err.to_string()))?;

        let mut body = obj.body.ok_or(Error::Missing)?.into_async_read();

        tokio::io::copy(&mut body, &mut file).await?;
    }

    let mut file = file.into_std().await;

    let (tx, mut rx) = tokio::sync::mpsc::channel(4);

    let ctx_clone = ctx.clone();
    tokio::task::spawn(async move {
        while let Some(tweet) = rx.recv().await {
            if let Err(err) = add_tweet(&ctx_clone, user_id, account_id, tweet).await {
                tracing::error!("could not add tweet: {err}");
            }
        }
    });

    web::block(move || -> Result<(), TweetError> {
        file.rewind().unwrap();

        let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file)).unwrap();

        let matcher = regex::Regex::new(r"/tweet(?:-part\d+)?\.js$").unwrap();
        let tweet_files = archive
            .file_names()
            .filter(|file_name| matcher.is_match(file_name))
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        for tweet_file in tweet_files {
            tracing::info!("finding tweets in {}", tweet_file);
            let file = archive.by_name(&tweet_file).unwrap();
            let tweets = read_tweets_from_file(file, &account.username)?;
            tracing::debug!("found {} tweets", tweets.len());

            for tweet in tweets {
                for (photo_id, photo_url) in &tweet.photos {
                    match process_tweet(&mut archive, &tweet, *photo_id, photo_url) {
                        Ok(tweet) => tx
                            .blocking_send(tweet)
                            .expect("could not send processed tweet"),
                        Err(err) => {
                            tracing::error!("could not process tweet: {err}");
                        }
                    }
                }
            }
        }

        Ok(())
    })
    .await
    .map_err(|_err| Error::unknown_message("Could not join blocking task"))?
    .map_err(|_err| Error::unknown_message("Error parsing archive"))?;

    let objects: Vec<_> = chunks
        .into_iter()
        .map(|chunk| format!("tmp/{user_id}/{collection_id}-{chunk}"))
        .map(|path| rusoto_s3::ObjectIdentifier {
            key: path,
            version_id: None,
        })
        .collect();

    let delete = rusoto_s3::DeleteObjectsRequest {
        bucket: ctx.config.s3_bucket.clone(),
        delete: rusoto_s3::Delete {
            objects,
            ..Default::default()
        },
        ..Default::default()
    };

    if let Err(err) = ctx.s3.delete_objects(delete).await {
        tracing::error!("could not delete chunks from s3: {err}");
    }

    models::LinkedAccount::update_loading_state(
        &ctx.conn,
        &ctx.nats,
        user_id,
        account_id,
        models::LoadingState::Complete,
    )
    .await?;

    Ok(())
}

async fn add_tweet(
    ctx: &jobs::JobContext,
    user_id: Uuid,
    account_id: Uuid,
    tweet: TweetToSave,
) -> Result<Uuid, Error> {
    let source_id = format!("{}-{}", tweet.tweet_id, tweet.photo_id);

    if let Ok(Some(media)) = models::OwnedMediaItem::lookup_by_site_id(
        &ctx.conn,
        user_id,
        models::Site::Twitter,
        &source_id,
    )
    .await
    {
        return Ok(media.id);
    }

    let (item_id, is_new) = models::OwnedMediaItem::add_item(
        &ctx.conn,
        user_id,
        account_id,
        source_id,
        tweet.perceptual_hash,
        tweet.sha256,
        Some(format!(
            "https://twitter.com/{}/status/{}",
            tweet.screen_name, tweet.tweet_id,
        )),
        None,
        Some(tweet.posted_at),
    )
    .await?;

    if is_new {
        if let Some(im) = tweet.im {
            models::OwnedMediaItem::update_media(&ctx.conn, &ctx.s3, &ctx.config, item_id, im)
                .await
                .unwrap();

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

    Ok(item_id)
}

fn read_tweets_from_file(
    mut file: zip::read::ZipFile,
    screen_name: &str,
) -> Result<Vec<SavedTweet>, TweetError> {
    let mut comma_buf = [0u8; 1];
    let mut extra_data_buf = [0u8; 26];
    file.read_exact(&mut extra_data_buf)?;

    let mut tweets = Vec::new();

    loop {
        let mut decoder =
            serde_json::Deserializer::from_reader(&mut file).into_iter::<ArchiveEntry>();
        match decoder.next() {
            Some(Ok(entry)) => {
                file.read_exact(&mut comma_buf).unwrap();

                if entry.tweet.full_text.starts_with("RT @") {
                    continue;
                }

                let posted_at =
                    chrono::DateTime::parse_from_str(&entry.tweet.created_at, "%a %b %d %T %z %Y")
                        .unwrap();

                let photos = entry
                    .tweet
                    .entities
                    .unwrap_or_default()
                    .media
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|media| media.media_type == TwitterArchiveMediaType::Photo)
                    .filter_map(|photo| Some((photo.id.parse().ok()?, photo.media_url_https)))
                    .collect();

                tweets.push(SavedTweet {
                    id: entry.tweet.id,
                    posted_at: posted_at.into(),
                    screen_name: screen_name.to_string(),
                    photos,
                });
            }
            Some(Err(err)) => {
                tracing::warn!("could not decode entry: {}", err);
                break;
            }
            None => break,
        }
    }

    Ok(tweets)
}

fn process_tweet<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    tweet: &SavedTweet,
    photo_id: u64,
    photo_url: &str,
) -> Result<TweetToSave, TweetError> {
    let (_prefix, name) = photo_url.rsplit_once('/').expect("media url was not url");
    let path = format!("data/tweet_media/{}-{}", tweet.id, name);
    tracing::trace!("looking for photo {path}");

    let mut photo_file = archive.by_name(&path).map_err(|_err| TweetError::Missing)?;
    let mut image_data = Vec::new();
    let _read_bytes = photo_file.read_to_end(&mut image_data)?;

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

    let saved = TweetToSave {
        tweet_id: tweet
            .id
            .parse()
            .map_err(|_err| TweetError::InvalidData("tweet_id"))?,
        photo_id,
        posted_at: tweet.posted_at,
        screen_name: tweet.screen_name.clone(),
        sha256,
        perceptual_hash,
        im,
    };

    Ok(saved)
}
