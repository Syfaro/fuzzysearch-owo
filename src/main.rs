use std::sync::Arc;

use actix_session::{
    config::{PersistentSession, SessionLifecycle},
    storage::CookieSessionStore,
    SessionMiddleware,
};
use actix_web::{cookie::Key, web, App, FromRequest, HttpResponse, HttpServer};
use askama::Template;
use async_nats::ServerAddr;
use async_trait::async_trait;
use base64::Engine;
use clap::Parser;

mod admin;
mod api;
mod auth;
mod common;
mod error;
mod jobs;
mod models;
mod site;
mod user;

pub use error::Error;
use foxlib::jobs::FaktoryProducer;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

type Mailer = lettre::AsyncSmtpTransport<lettre::Tokio1Executor>;

#[derive(Clone, Parser)]
pub struct WebConfig {
    /// Number of worker threads for HTTP server.
    #[clap(long, env("HTTP_WORKERS"), default_value = "4")]
    pub http_workers: usize,
    /// Address to bind HTTP server to.
    #[clap(long, env("HTTP_HOST"), default_value = "127.0.0.1:8080")]
    pub http_host: String,

    /// Private key to sign and encrypt cookies, in hexadecimal. Must be at
    /// least 32 bytes.
    #[clap(long, env("COOKIE_PRIVATE_KEY"))]
    pub cookie_private_key: String,
    /// If the cookies should be accessible over http.
    #[clap(long, env("COOKIE_INSECURE"))]
    pub cookie_insecure: bool,

    /// If the Cloudflare `cf-connecting-ip` header should be trusted.
    #[clap(long, env("TRUST_CLOUDFLARE"))]
    pub trust_cloudflare: bool,

    #[clap(long, env("TELEGRAM_LOGIN_USERNAME"))]
    pub telegram_login_username: String,
    #[clap(long, env("TELEGRAM_AUTH_URL"))]
    pub telegram_auth_url: String,

    /// Path to static files.
    #[clap(long, env("ASSETS_DIR"), default_value = "./assets")]
    pub assets_dir: String,
}

#[derive(Clone, Parser)]
pub struct WorkerConfig {
    /// Number of workers for background jobs.
    #[clap(long, env("FAKTORY_WORKERS"), default_value = "2")]
    pub faktory_workers: usize,
    /// Queues to fetch jobs from.
    #[clap(long, env("FAKTORY_QUEUES"), value_enum, use_value_delimiter = true)]
    pub faktory_queues: Vec<jobs::Queue>,

    /// NATS hosts.
    #[clap(
        long,
        env("NATS_HOST"),
        use_value_delimiter = true,
        value_delimiter = ','
    )]
    pub nats_host: Vec<ServerAddr>,
    /// NATS NKEY, may be omitted if authentication is not needed.
    #[clap(long, env("NATS_NKEY"))]
    pub nats_nkey: Option<String>,
}

#[derive(Clone, clap::Subcommand)]
pub enum ServiceMode {
    /// Run background tasks. Make sure to also set labels as needed.
    BackgroundWorker(WorkerConfig),
    /// Serve website.
    Web(WebConfig),
}

#[derive(Clone, Parser)]
#[clap(about, version, author)]
pub struct Config {
    /// Full URL to site, including https and excluding trailing slash.
    #[clap(long, env("HOST_URL"))]
    pub host_url: String,

    /// Database URL, in the format `postgres://user:password@host/database`.
    #[clap(long, env("DATABASE_URL"))]
    pub database_url: String,
    /// If should run migrations on start.
    #[clap(long, env("RUN_MIGRATIONS"))]
    pub run_migrations: bool,

    /// If logs should output in JSON format.
    #[clap(long, env("JSON_LOGS"))]
    pub json_logs: bool,
    /// Metrics host for prometheus.
    #[clap(long, env("METRICS_HOST"))]
    pub metrics_host: Option<std::net::SocketAddr>,
    /// Sentry DSN.
    #[clap(long, env("SENTRY_DSN"))]
    pub sentry_dsn: Option<String>,

    /// S3 region name, can be anything for Minio.
    #[clap(long, env("S3_REGION_NAME"))]
    pub s3_region_name: String,
    /// S3 region endpoint.
    #[clap(long, env("S3_REGION_ENDPOINT"))]
    pub s3_region_endpoint: String,
    /// S3 access key.
    #[clap(long, env("S3_ACCESS_KEY"))]
    pub s3_access_key: String,
    /// S3 secret access key.
    #[clap(long, env("S3_SECRET_ACCESS_KEY"))]
    pub s3_secret_access_key: String,
    /// S3 bucket.
    #[clap(long, env("S3_BUCKET"), default_value = "fuzzysearch-owo")]
    pub s3_bucket: String,
    /// S3 CDN prefix, all uploaded files will be $S3_CDN_PREFIX/object/name.
    #[clap(long, env("S3_CDN_PREFIX"))]
    pub s3_cdn_prefix: String,

    /// Redis DSN, in the format `redis://host/`.
    #[clap(long, env("REDIS_DSN"))]
    pub redis_dsn: String,

    /// FuzzySearch API host.
    #[clap(
        long,
        env("FUZZYSEARCH_HOST"),
        default_value = "https://api.fuzzysearch.net"
    )]
    pub fuzzysearch_host: String,
    /// FuzzySearch API key.
    #[clap(long, env("FUZZYSEARCH_API_KEY"))]
    pub fuzzysearch_api_key: String,

    /// Telegram API token from Botfather, used for login and sending
    /// notification messages.
    #[clap(long, env("TELEGRAM_BOT_TOKEN"))]
    pub telegram_bot_token: String,

    /// Faktory host, in the format `tcp://host`.
    #[clap(long, env("FAKTORY_HOST"))]
    pub faktory_host: String,

    /// SMTP hostname.
    #[clap(long, env("SMTP_HOST"))]
    pub smtp_host: String,
    /// SMTP username.
    #[clap(long, env("SMTP_USERNAME"))]
    pub smtp_username: String,
    /// SMTP password.
    #[clap(long, env("SMTP_PASSWORD"))]
    pub smtp_password: String,
    /// SMTP port.
    #[clap(long, env("SMTP_PORT"), default_value = "465")]
    pub smtp_port: u16,

    /// From address for emails.
    #[clap(long, env("SMTP_FROM"))]
    pub smtp_from: lettre::message::Mailbox,
    /// Reply to address for emails.
    #[clap(long, env("SMTP_REPLY_TO"))]
    pub smtp_reply_to: lettre::message::Mailbox,

    /// User agent to use for outgoing requests.
    #[clap(long, env("USER_AGENT"))]
    pub user_agent: String,

    /// DeviantArt client ID.
    #[clap(long, env("DEVIANTART_CLIENT_ID"))]
    pub deviantart_client_id: String,
    /// DeviantArt client secret.
    #[clap(long, env("DEVIANTART_CLIENT_SECRET"))]
    pub deviantart_client_secret: String,

    /// Patreon client ID.
    #[clap(long, env("PATREON_CLIENT_ID"))]
    pub patreon_client_id: String,
    /// Patreon client secret.
    #[clap(long, env("PATREON_CLIENT_SECRET"))]
    pub patreon_client_secret: String,

    /// FurAffinity cookie a.
    #[clap(long, env("FURAFFINITY_COOKIE_A"))]
    pub furaffinity_cookie_a: String,
    /// FurAffinity cookie b.
    #[clap(long, env("FURAFFINITY_COOKIE_B"))]
    pub furaffinity_cookie_b: String,

    /// F-list username.
    #[clap(long, env("FLIST_USERNAME"))]
    pub flist_username: String,
    /// F-list password.
    #[clap(long, env("FLIST_PASSWORD"))]
    pub flist_password: String,

    /// Weasyl API token.
    #[clap(long, env("WEASYL_API_TOKEN"))]
    pub weasyl_api_token: String,

    /// Reddit client ID (or username).
    #[clap(long, env("REDDIT_CLIENT_ID"))]
    pub reddit_client_id: String,
    /// Reddit client secret (or password).
    #[clap(long, env("REDDIT_CLIENT_SECRET"))]
    pub reddit_client_secret: String,
    /// Reddit username.
    #[clap(long, env("REDDIT_USERNAME"))]
    pub reddit_username: String,
    /// Reddit password.
    #[clap(long, env("REDDIT_PASSWORD"))]
    pub reddit_password: String,

    /// Twitter consumer key.
    #[clap(long, env("TWITTER_CONSUMER_KEY"))]
    pub twitter_consumer_key: String,
    /// Twitter consumer secret.
    #[clap(long, env("TWITTER_CONSUMER_SECRET"))]
    pub twitter_consumer_secret: String,

    /// Skip verifications for account.
    #[clap(long)]
    pub skip_verifications: bool,

    /// Mode to run service.
    #[clap(subcommand)]
    pub service_mode: ServiceMode,
}

#[cfg(feature = "env")]
fn load_config() -> Config {
    dotenv::dotenv().expect("running with env feature with no or invalid .env file");
    Config::parse()
}

#[cfg(not(feature = "env"))]
fn load_config() -> Config {
    Config::parse()
}

/// Something that can be converted into a format suitable for URLs.
pub trait AsUrl {
    fn as_url(&self) -> String;
}

/// A Uuid that gets base64 encoded for usage in URLs.
#[derive(Clone, Copy, Debug)]
pub struct UrlUuid(Uuid);

impl AsUrl for UrlUuid {
    fn as_url(&self) -> String {
        self.to_string()
    }
}

impl AsUrl for Uuid {
    fn as_url(&self) -> String {
        UrlUuid(*self).to_string()
    }
}

impl std::fmt::Display for UrlUuid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let data = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.0);

        write!(f, "{data}")
    }
}

impl std::str::FromStr for UrlUuid {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(uuid) = Uuid::from_str(s) {
            return Ok(Self(uuid));
        }

        let data = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(s)
            .map_err(|_err| "not base64")?;
        let data: [u8; 16] = data.try_into().map_err(|_err| "not 16 bytes")?;

        Ok(Self(Uuid::from_bytes(data)))
    }
}

impl From<Uuid> for UrlUuid {
    fn from(value: Uuid) -> Self {
        Self(value)
    }
}

impl From<UrlUuid> for Uuid {
    fn from(value: UrlUuid) -> Self {
        value.0
    }
}

impl Serialize for UrlUuid {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for UrlUuid {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let input: &str = Deserialize::deserialize(deserializer)?;

        if let Ok(uuid) = Uuid::try_parse(input) {
            return Ok(Self(uuid));
        }

        let data = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(input)
            .map_err(serde::de::Error::custom)?;

        let data: [u8; 16] = data
            .try_into()
            .map_err(|_err| serde::de::Error::custom("incorrectly sized base64 data"))?;

        Ok(Self(Uuid::from_bytes(data)))
    }
}

#[derive(Template)]
#[template(path = "base.html")]
pub struct BaseTemplate<'a, T: std::fmt::Display + askama::Template> {
    pub user: Option<&'a models::User>,
    pub uri: &'a actix_web::http::Uri,
    pub flashes: Option<Vec<FlashMessage>>,

    pub content: &'a T,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct FlashMessage {
    pub classes: String,
    pub message: String,
}

pub enum FlashStyle {
    Info,
    Success,
    Warning,
    Error,
}

impl FlashStyle {
    fn classes(&self) -> &'static str {
        match self {
            Self::Info => "",
            Self::Success => "is-success",
            Self::Warning => "is-warning",
            Self::Error => "is-danger",
        }
    }
}

pub trait AddFlash {
    fn add_flash<M: ToString>(&self, style: FlashStyle, message: M);
}

impl AddFlash for actix_session::Session {
    fn add_flash<M: ToString>(&self, style: FlashStyle, message: M) {
        let flash_message = FlashMessage {
            classes: style.classes().to_string(),
            message: message.to_string(),
        };
        self.insert("flashes", vec![flash_message])
            .expect("could not insert to session");
    }
}

#[async_trait(?Send)]
pub trait WrappedTemplate: Sized + std::fmt::Display + askama::Template {
    async fn wrap<'a>(
        &'a self,
        request: &'a actix_web::HttpRequest,
        user: Option<&'a models::User>,
    ) -> BaseTemplate<'a, Self>;
}

#[async_trait(?Send)]
impl<T: Sized + std::fmt::Display + askama::Template> WrappedTemplate for T {
    async fn wrap<'a>(
        &'a self,
        request: &'a actix_web::HttpRequest,
        user: Option<&'a models::User>,
    ) -> BaseTemplate<'a, Self> {
        let session = actix_session::Session::extract(request).await.ok().unwrap();
        let flashes = match session.get::<Vec<FlashMessage>>("flashes") {
            Ok(flashes @ Some(_)) => {
                session.remove("flashes");
                flashes
            }
            Ok(None) => None,
            Err(err) => {
                tracing::error!("could not get flashes: {err}");
                None
            }
        };

        BaseTemplate {
            user,
            uri: request.uri(),
            flashes,
            content: self,
        }
    }
}

#[derive(Template)]
#[template(path = "index.html")]
struct Home;

#[actix_web::get("/", name = "index")]
async fn index(
    request: actix_web::HttpRequest,
    user: Option<models::User>,
) -> Result<HttpResponse, Error> {
    if user.is_some() {
        return Ok(HttpResponse::Found()
            .insert_header(("Location", request.url_for_static("user_home")?.as_str()))
            .finish());
    }

    let body = Home.wrap(&request, user.as_ref()).await.render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[derive(Template)]
#[template(path = "markdown.html")]
struct MarkdownPage {
    content: &'static str,
}

#[actix_web::get("/changelog")]
async fn changelog(
    request: actix_web::HttpRequest,
    user: Option<models::User>,
) -> Result<HttpResponse, Error> {
    let body = MarkdownPage {
        content: include_str!("../content/CHANGELOG.md"),
    }
    .wrap(&request, user.as_ref())
    .await
    .render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[actix_web::get("/faq")]
async fn faq(
    request: actix_web::HttpRequest,
    user: Option<models::User>,
) -> Result<HttpResponse, Error> {
    let body = MarkdownPage {
        content: include_str!("../content/FAQ.md"),
    }
    .wrap(&request, user.as_ref())
    .await
    .render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[actix_web::get("/takedown")]
async fn takedown(
    request: actix_web::HttpRequest,
    user: Option<models::User>,
) -> Result<HttpResponse, Error> {
    let body = MarkdownPage {
        content: include_str!("../content/takedowns.md"),
    }
    .wrap(&request, user.as_ref())
    .await
    .render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

async fn not_found() -> Result<HttpResponse, Error> {
    Err(Error::Missing)
}

struct ClientIpAddr {
    ip_addr: Option<String>,
}

impl actix_web::FromRequest for ClientIpAddr {
    type Error = Error;
    type Future = std::pin::Pin<Box<dyn futures::Future<Output = Result<Self, Self::Error>>>>;

    fn from_request(
        req: &actix_web::HttpRequest,
        payload: &mut actix_http::Payload,
    ) -> Self::Future {
        let trust_cloudflare = match &req
            .app_data::<web::Data<Config>>()
            .expect("app missing config")
            .service_mode
        {
            ServiceMode::Web(web_config) => web_config.trust_cloudflare,
            _ => unreachable!("web requests should always be in web service mode"),
        };

        let cf_connecting_ip = req
            .headers()
            .get("cf-connecting-ip")
            .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string());

        let real_ip = actix_web::dev::ConnectionInfo::from_request(req, payload);

        Box::pin(async move {
            let ip_addr = if trust_cloudflare && cf_connecting_ip.is_some() {
                cf_connecting_ip
            } else {
                let real_ip = real_ip.await.expect("connectioninfo issue");
                real_ip.realip_remote_addr().map(|addr| addr.to_string())
            };

            Ok(Self { ip_addr })
        })
    }
}

#[tokio::main]
async fn main() {
    let config = load_config();

    let name = match config.service_mode {
        ServiceMode::Web(ref _config) => "fuzzysearch-owo-web",
        ServiceMode::BackgroundWorker(ref _config) => "fuzzysearch-owo-worker",
    };

    foxlib::trace::init(foxlib::trace::TracingConfig {
        namespace: env!("CARGO_PKG_NAME"),
        name,
        version: env!("CARGO_PKG_VERSION"),
        otlp: config.json_logs,
    });

    if let Some(host) = config.metrics_host {
        foxlib::MetricsServer::serve(host, true).await;
    }

    let pool = sqlx::PgPool::connect(&config.database_url)
        .await
        .expect("could not connect to database");

    if config.run_migrations {
        tracing::info!("running migrations");

        sqlx::migrate!()
            .run(&pool)
            .await
            .expect("database migrations failed");
    }

    let region = rusoto_core::Region::Custom {
        name: config.s3_region_name.clone(),
        endpoint: config.s3_region_endpoint.clone(),
    };
    let client =
        rusoto_core::request::HttpClient::new().expect("could not create rusoto http client");
    let provider = rusoto_credential::StaticProvider::new_minimal(
        config.s3_access_key.clone(),
        config.s3_secret_access_key.clone(),
    );
    let s3 = rusoto_s3::S3Client::new_with(client, provider, region);

    let redis_client =
        redis::Client::open(config.redis_dsn.clone()).expect("could not create redis client");
    let redis_manager = redis::aio::ConnectionManager::new(redis_client.clone())
        .await
        .expect("could not create redis connection manager");

    let redlock = redlock::RedLock::new(vec![config.redis_dsn.as_ref()]);

    let fuzzysearch = fuzzysearch::FuzzySearch::new_with_opts(fuzzysearch::FuzzySearchOpts {
        endpoint: Some(config.fuzzysearch_host.clone()),
        api_key: config.fuzzysearch_api_key.clone(),
        client: Default::default(),
    });

    let client = reqwest::ClientBuilder::default()
        .user_agent(&config.user_agent)
        .build()
        .expect("could not create http client");

    let producer = FaktoryProducer::connect(Some(config.faktory_host.clone()))
        .await
        .expect("could not connect to faktory");

    let _guard = config.sentry_dsn.as_deref().map(|dsn| {
        sentry::init((
            dsn,
            sentry::ClientOptions {
                release: sentry::release_name!(),
                session_mode: sentry::SessionMode::Request,
                auto_session_tracking: true,
                ..Default::default()
            },
        ))
    });

    match config.service_mode.clone() {
        ServiceMode::BackgroundWorker(worker_config) => {
            let creds = lettre::transport::smtp::authentication::Credentials::new(
                config.smtp_username.clone(),
                config.smtp_password.clone(),
            );

            let mailer: Mailer =
                lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::relay(&config.smtp_host)
                    .unwrap()
                    .credentials(creds)
                    .build();

            let telegram = Arc::new(tgbotapi::Telegram::new(config.telegram_bot_token.clone()));

            let nats_opts = if let Some(nats_nkey) = &worker_config.nats_nkey {
                async_nats::ConnectOptions::with_nkey(nats_nkey.clone())
            } else {
                async_nats::ConnectOptions::default()
            };

            let nats = nats_opts
                .connect(&worker_config.nats_host)
                .await
                .expect("could not connect to nats");

            jobs::start_job_processing(jobs::JobContext {
                producer,
                conn: pool,
                redis: redis_manager,
                redlock: std::sync::Arc::new(redlock),
                s3,
                fuzzysearch: std::sync::Arc::new(fuzzysearch),
                mailer,
                config: std::sync::Arc::new(config.clone()),
                worker_config: std::sync::Arc::new(worker_config.clone()),
                client,
                telegram,
                nats,
            })
            .await
            .expect("could not run background worker");
        }
        ServiceMode::Web(web_config) => {
            let cookie_private_key = hex::decode(&web_config.cookie_private_key)
                .expect("cookie secret was not hex data");
            assert!(
                cookie_private_key.len() >= 64,
                "cookie private key must be greater than 64 bytes"
            );

            let telegram_login = auth::TelegramLoginConfig {
                bot_username: web_config.telegram_login_username,
                auth_url: web_config.telegram_auth_url,
                token: config.telegram_bot_token.clone(),
            };

            let (http_host, http_workers) = (web_config.http_host.clone(), web_config.http_workers);
            tracing::info!("starting fuzzysearch-owo on http://{}", http_host);

            HttpServer::new(move || {
                let key = Key::from(&cookie_private_key);

                let session = SessionMiddleware::builder(CookieSessionStore::default(), key)
                    .cookie_name("owo-session".to_string())
                    .cookie_secure(!web_config.cookie_insecure)
                    .cookie_http_only(true)
                    .session_lifecycle(SessionLifecycle::PersistentSession(
                        PersistentSession::default()
                            .session_ttl(actix_web::cookie::time::Duration::days(365)),
                    ))
                    .build();

                let files =
                    actix_files::Files::new("/static", &web_config.assets_dir).prefer_utf8(true);

                App::new()
                    .wrap(tracing_actix_web::TracingLogger::default())
                    .wrap(sentry_actix::Sentry::new())
                    .wrap(session)
                    .app_data(web::Data::new(pool.clone()))
                    .app_data(web::Data::new(s3.clone()))
                    .app_data(web::Data::new(redis_client.clone()))
                    .app_data(web::Data::new(redis_manager.clone()))
                    .app_data(web::Data::new(config.clone()))
                    .app_data(web::Data::new(telegram_login.clone()))
                    .app_data(web::Data::new(producer.clone()))
                    .service(auth::service())
                    .service(user::service())
                    .service(api::service())
                    .service(site::service())
                    .service(admin::service())
                    .service(files)
                    .service(index)
                    .service(changelog)
                    .service(faq)
                    .service(takedown)
                    .default_service(web::to(not_found))
            })
            .workers(http_workers)
            .bind(http_host)
            .expect("could not bind server")
            .run()
            .await
            .expect("server failed");
        }
    }
}
