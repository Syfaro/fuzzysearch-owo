use std::collections::HashMap;

use actix_web::{get, services, web, HttpResponse};
use async_trait::async_trait;
use egg_mode::entities::MediaType;
use foxlib::jobs::{FaktoryForge, FaktoryJob, FaktoryProducer, Job, JobExtra};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use uuid::Uuid;

use crate::{
    jobs::{self, JobContext, JobInitiator, JobInitiatorExt, Queue, SearchExistingSubmissionsJob},
    models::{self, LinkedAccount},
    Config, Error,
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
    }

    async fn add_account(
        &self,
        ctx: &jobs::JobContext,
        account: LinkedAccount,
    ) -> Result<(), Error> {
        models::LinkedAccount::update_loading_state(
            &ctx.conn,
            &ctx.redis,
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
        vec![web::scope("/twitter").service(services![auth, callback])]
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

        tracing::info!("{:#?}", tweet);

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
        .insert_header(("Location", format!("/user/account/{}", id)))
        .finish())
}

#[derive(Default, Serialize, Deserialize)]
struct TwitterData {
    site_id: String,
    access_token: String,
    secret_token: String,
    newest_id: Option<u64>,
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

    if was_import {
        let mut redis = ctx.redis.clone();
        super::update_import_progress(&ctx.conn, &mut redis, user_id, account_id, tweet.id).await?;
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
