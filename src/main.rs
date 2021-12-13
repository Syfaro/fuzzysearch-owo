use actix_session::CookieSession;
use actix_web::{web, App, HttpResponse, HttpServer};
use clap::Parser;

mod api;
mod auth;
mod jobs;
mod models;
mod routes;
mod user;

#[actix_web::get("/")]
async fn index(user: Option<models::User>) -> HttpResponse {
    if user.is_some() {
        HttpResponse::Found()
            .insert_header(("Location", routes::USER_HOME))
            .finish()
    } else {
        HttpResponse::Found()
            .insert_header(("Location", routes::AUTH_LOGIN))
            .finish()
    }
}

#[derive(Clone, Parser)]
#[clap(about, version, author)]
pub struct Config {
    /// Database URL, in the format `postgres://user:password@host/database`.
    #[clap(long, env("DATABASE_URL"))]
    pub database_url: String,

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

    /// Redis DSN, in the format `redis://host/`.
    #[clap(long, env("REDIS_DSN"))]
    pub redis_dsn: String,

    /// FuzzySearch API host, defaults to main instance.
    #[clap(
        long,
        env("FUZZYSEARCH_HOST"),
        default_value = "https://api.fuzzysearch.net"
    )]
    pub fuzzysearch_host: String,
    /// FuzzySearch API key.
    #[clap(long, env("FUZZYSEARCH_API_KEY"))]
    pub fuzzysearch_api_key: String,

    /// Faktory host, in the format `tcp://host`.
    #[clap(long, env("FAKTORY_HOST"))]
    pub faktory_host: String,

    /// Private key to sign and encrypt cookies, in hexadecimal. Must be at
    /// least 32 bytes.
    #[clap(long, env("COOKIE_PRIVATE_KEY"))]
    pub cookie_private_key: String,
    /// If the cookies should be accessible over http.
    #[clap(long, env("COOKIE_INSECURE"))]
    pub cookie_insecure: bool,

    /// User agent to use for outgoing requests.
    #[clap(long, env("USER_AGENT"))]
    pub user_agent: String,

    /// FurAffinity cookie a.
    #[clap(long, env("FURAFFINITY_COOKIE_A"))]
    pub furaffinity_cookie_a: String,
    /// FurAffinity cookie b.
    #[clap(long, env("FURAFFINITY_COOKIE_B"))]
    pub furaffinity_cookie_b: String,
}

#[cfg(feature = "env")]
fn load_config() -> Config {
    dotenv::dotenv().unwrap();
    Config::parse()
}

#[cfg(not(feature = "env"))]
fn load_config() -> Config {
    Config::parse()
}

#[tokio::main]
async fn main() {
    let config = load_config();

    fuzzysearch_common::trace::configure_tracing("fuzzysearch-owo");
    fuzzysearch_common::trace::serve_metrics().await;

    tracing::info!("starting fuzzysearch-owo on http://0.0.0.0:8095");

    let pool = sqlx::PgPool::connect(&config.database_url).await.unwrap();

    let region = rusoto_core::Region::Custom {
        name: config.s3_region_name.clone(),
        endpoint: config.s3_region_endpoint.clone(),
    };
    let client = rusoto_core::request::HttpClient::new().unwrap();
    let provider = rusoto_credential::StaticProvider::new_minimal(
        config.s3_access_key.clone(),
        config.s3_secret_access_key.clone(),
    );
    let s3 = rusoto_s3::S3Client::new_with(client, provider, region);

    let redis_client = redis::Client::open(config.redis_dsn.clone()).unwrap();
    let redis_manager = redis::aio::ConnectionManager::new(redis_client.clone())
        .await
        .unwrap();

    let fuzzysearch = fuzzysearch::FuzzySearch::new_with_opts(fuzzysearch::FuzzySearchOpts {
        endpoint: Some(config.fuzzysearch_host.clone()),
        api_key: config.fuzzysearch_api_key.clone(),
        client: Default::default(),
    });

    let faktory = fuzzysearch_common::faktory::FaktoryClient::connect(config.faktory_host.clone())
        .await
        .unwrap();

    jobs::start_job_processing(jobs::JobContext {
        faktory: faktory.clone(),
        conn: pool.clone(),
        redis: redis_manager.clone(),
        s3: s3.clone(),
        fuzzysearch: std::sync::Arc::new(fuzzysearch),
        config: std::sync::Arc::new(config.clone()),
    })
    .await;

    let cookie_private_key = hex::decode(&config.cookie_private_key).unwrap();
    assert!(
        cookie_private_key.len() >= 32,
        "cookie private key must be greater than 32 bytes"
    );

    HttpServer::new(move || {
        let session = CookieSession::private(&cookie_private_key).secure(!config.cookie_insecure);

        App::new()
            .wrap(tracing_actix_web::TracingLogger::default())
            .wrap(session)
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(s3.clone()))
            .app_data(web::Data::new(redis_client.clone()))
            .app_data(web::Data::new(redis_manager.clone()))
            .app_data(web::Data::new(faktory.clone()))
            .app_data(web::Data::new(config.clone()))
            .service(auth::service())
            .service(user::service())
            .service(api::service())
            .service(actix_files::Files::new("/static", "assets").prefer_utf8(true))
            .service(index)
    })
    .workers(4)
    .bind("0.0.0.0:8095")
    .unwrap()
    .run()
    .await
    .unwrap();
}
