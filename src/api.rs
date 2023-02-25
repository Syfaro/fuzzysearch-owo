use std::time::{Duration, Instant};

use actix::{
    fut::LocalBoxActorFuture, Actor, ActorContext, ActorFutureExt, ActorTryFutureExt, AsyncContext,
    Handler, Message, StreamHandler, WrapFuture,
};
use actix_http::ws::CloseCode;
use actix_session::Session;
use actix_web::{get, post, services, web, HttpRequest, HttpResponse, Scope};
use actix_web_actors::ws;
use actix_web_httpauth::extractors::basic::BasicAuth;
use foxlib::jobs::FaktoryProducer;
use futures::StreamExt;
use futures::TryStreamExt;
use rusoto_s3::S3;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::FuzzySearchSessionToken,
    common,
    jobs::{JobInitiator, JobInitiatorExt, NewSubmissionJob},
    models, Error, UrlUuid,
};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const CLIENT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Message)]
#[rtype(result = "()")]
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventMessage {
    Unauthorized {
        is_unauthorized: bool,
    },
    SimpleMessage {
        id: Uuid,
        message: String,
    },
    LoadingStateChange {
        account_id: Uuid,
        loading_state: String,
    },
    LoadingProgress {
        account_id: Uuid,
        loaded: i32,
        total: i32,
    },
    SimilarImage {
        media_id: Uuid,
        link: String,
    },
    SessionEnded {
        session_id: Uuid,
    },
    AccountVerified {
        account_id: Uuid,
        verified: bool,
    },
}

struct UnauthorizedWsEventSession;

impl Actor for UnauthorizedWsEventSession {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        ctx.text(
            serde_json::to_string(&EventMessage::Unauthorized {
                is_unauthorized: true,
            })
            .expect("could not encode unauthorized json"),
        );

        ctx.close(Some((CloseCode::Normal, "Unauthorized").into()));
    }
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for UnauthorizedWsEventSession {
    fn handle(&mut self, _item: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        ctx.stop();
    }
}

struct WsEventSession {
    user_id: Uuid,
    session_id: Uuid,
    redis: redis::Client,
    hb: Instant,
}

impl WsEventSession {
    fn new(user_id: Uuid, session_id: Uuid, redis: redis::Client) -> Self {
        Self {
            user_id,
            session_id,
            redis,
            hb: Instant::now(),
        }
    }

    fn hb(&self, ctx: &mut ws::WebsocketContext<Self>) {
        ctx.run_interval(HEARTBEAT_INTERVAL, |act, ctx| {
            tracing::trace!("checking last heartbeat");

            if Instant::now().duration_since(act.hb) > CLIENT_TIMEOUT {
                tracing::info!("client heartbeat timed out, disconnecting");
                ctx.stop();
            } else {
                tracing::trace!("no timeout, sending ping");
                ctx.ping(b"");
            }
        });
    }

    fn keep_connected_to_events(&mut self) -> LocalBoxActorFuture<Self, ()> {
        Box::pin(self.attempt_redis_connection().then(|res, this, _| {
            if let Err(err) = res {
                tracing::warn!(
                    "could not connect to redis pubsub for user events: {:?}",
                    err
                );

                futures::future::Either::Left(
                    tokio::time::sleep(Duration::from_secs(1))
                        .into_actor(this)
                        .then(|_, this, _| this.keep_connected_to_events()),
                )
            } else {
                futures::future::Either::Right(futures::future::ready(()))
            }
        }))
    }

    fn attempt_redis_connection(
        &mut self,
    ) -> LocalBoxActorFuture<Self, Result<(), redis::RedisError>> {
        let redis = self.redis.clone();
        let user_id = self.user_id;

        let (tx, rx) = futures::channel::mpsc::unbounded();

        Box::pin(
            async move {
                let conn = redis.get_async_connection().await?;
                let mut pubsub = conn.into_pubsub();
                pubsub.subscribe(format!("user-events:{user_id}")).await?;

                Ok(pubsub)
            }
            .into_actor(self)
            .map_ok(|mut pubsub, this, ctx| {
                ctx.spawn(
                    async move {
                        let mut stream = pubsub.on_message();

                        while let Some(msg) = stream.next().await {
                            if let Err(err) = tx.unbounded_send(msg) {
                                tracing::error!("could not send pubsub event: {:?}", err);
                                break;
                            }
                        }
                    }
                    .into_actor(this),
                );

                ctx.add_stream(rx);
            }),
        )
    }
}

impl Actor for WsEventSession {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        tracing::info!("starting websocket session");
        self.hb(ctx);

        ctx.wait(self.keep_connected_to_events());
    }

    fn stopping(&mut self, _ctx: &mut Self::Context) -> actix::Running {
        tracing::info!("stopping websocket session");
        actix::Running::Stop
    }
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for WsEventSession {
    fn handle(&mut self, item: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        match item {
            Err(err) => {
                tracing::warn!("got websocket error, stopping: {:?}", err);
                ctx.stop();
            }
            Ok(ws::Message::Ping(msg)) => {
                tracing::trace!("got ping, responding");
                self.hb = Instant::now();
                ctx.pong(&msg);
            }
            Ok(ws::Message::Pong(_)) => {
                tracing::trace!("got pong, updating alive state");
                self.hb = Instant::now();
            }
            Ok(ws::Message::Close(reason)) => {
                tracing::info!("got close, disconnecting: {:?}", reason);
                ctx.stop();
            }
            other => tracing::warn!("got unhandled websocket event: {:?}", other),
        }
    }
}

impl StreamHandler<redis::Msg> for WsEventSession {
    fn handle(&mut self, item: redis::Msg, ctx: &mut Self::Context) {
        tracing::debug!("got redis message for session");

        let payload = item.get_payload_bytes();
        let event: EventMessage =
            serde_json::from_slice(payload).expect("got invalid data from redis");

        ctx.notify(event);
    }
}

impl Handler<EventMessage> for WsEventSession {
    type Result = ();

    fn handle(&mut self, msg: EventMessage, ctx: &mut Self::Context) -> Self::Result {
        // Suppress session ended events for unrelated sessions. This way the
        // client does not need to know it's own session ID to handle the event.
        let session_ended = match msg {
            EventMessage::SessionEnded { session_id } if session_id != self.session_id => return,
            EventMessage::SessionEnded { session_id } if session_id == self.session_id => true,
            _ => false,
        };

        let data = serde_json::to_string(&msg).expect("could not serialize essential data");
        ctx.text(data);

        if session_ended {
            ctx.close(Some((CloseCode::Normal, "session ended").into()));
        }
    }
}

#[get("/events")]
async fn events(
    user: Option<models::User>,
    session: Session,
    req: HttpRequest,
    stream: web::Payload,
    redis: web::Data<redis::Client>,
) -> Result<HttpResponse, Error> {
    if let Some(user) = user {
        let session_token = session
            .get_session_token()?
            .ok_or_else(|| Error::unknown_message("token must exist"))?;

        let session = WsEventSession::new(
            user.id,
            session_token.session_id,
            (*redis.into_inner()).clone(),
        );

        ws::start(session, &req, stream).map_err(Into::into)
    } else {
        ws::start(UnauthorizedWsEventSession, &req, stream).map_err(Into::into)
    }
}

#[post("/upload")]
async fn upload(
    pool: web::Data<PgPool>,
    redis: web::Data<redis::aio::ConnectionManager>,
    s3: web::Data<rusoto_s3::S3Client>,
    faktory: web::Data<FaktoryProducer>,
    config: web::Data<crate::Config>,
    auth: BasicAuth,
    form: actix_multipart::Multipart,
) -> Result<web::Json<Vec<Uuid>>, Error> {
    let password = match auth.password() {
        Some(password) => password,
        None => return Err(Error::user_error("basic auth password required")),
    };

    let (user_id, api_token): (UrlUuid, UrlUuid) = match (auth.user_id().parse(), password.parse())
    {
        (Ok(user_id), Ok(api_token)) => (user_id, api_token),
        _ => return Err(Error::user_error("username and password should be uuids")),
    };

    let user =
        match models::User::lookup_by_api_token(&pool, user_id.into(), api_token.into()).await? {
            Some(user) => user,
            None => return Err(Error::Unauthorized),
        };

    let ids =
        common::handle_multipart_upload(&pool, &redis, &s3, &faktory, &config, &user, form).await?;

    Ok(web::Json(ids))
}

#[post("/{collection}/add")]
async fn chunk_add(
    pool: web::Data<PgPool>,
    s3: web::Data<rusoto_s3::S3Client>,
    config: web::Data<crate::Config>,
    user: models::User,
    path: web::Path<Uuid>,
    mut form: actix_multipart::Multipart,
) -> Result<web::Json<Option<String>>, Error> {
    let mut sequence_number = None;

    while let Ok(Some(mut field)) = form.try_next().await {
        if !matches!(field.content_disposition().get_name(), Some("chunk")) {
            continue;
        }

        let mut buf = Vec::new();
        while let Ok(Some(chunk)) = field.try_next().await {
            if buf.len() + chunk.len() > 15_000_000 {
                return Err(Error::user_error("chunk is too large"));
            }

            buf.extend(chunk);
        }

        let total_size = models::FileUploadChunk::size(&pool, user.id).await? as usize;
        if total_size + buf.len() > 1024 * 1024 * 1024 * 5 {
            return Err(Error::user_error("pending chunks are too large"));
        }

        let num = models::FileUploadChunk::add(&pool, user.id, *path, buf.len() as i32).await?;
        sequence_number = Some(num);

        let path = format!("tmp/{}/{}-{}", user.id, path, num);
        let put = rusoto_s3::PutObjectRequest {
            bucket: config.s3_bucket.clone(),
            key: path.clone(),
            content_length: Some(buf.len() as i64),
            body: Some(rusoto_core::ByteStream::from(buf)),
            ..Default::default()
        };

        s3.put_object(put)
            .await
            .map_err(|err| Error::S3(err.to_string()))?;

        tracing::info!(path, "added chunk");

        break;
    }

    Ok(web::Json(sequence_number.map(|num| num.to_string())))
}

#[derive(Deserialize)]
struct FuzzySearchParams {
    secret: String,
}

#[post("/fuzzysearch/{secret}")]
async fn fuzzysearch(
    path: web::Path<FuzzySearchParams>,
    data: web::Json<fuzzysearch_common::faktory::WebHookData>,
    faktory: web::Data<FaktoryProducer>,
    config: web::Data<crate::Config>,
) -> Result<HttpResponse, Error> {
    if path.secret != config.fuzzysearch_api_key {
        return Ok(HttpResponse::Unauthorized().finish());
    }

    tracing::info!("got webhook data: {:?}", data.0);
    faktory
        .enqueue_job(
            NewSubmissionJob(data.0.into()).initiated_by(JobInitiator::external("fuzzysearch")),
        )
        .await?;

    Ok(HttpResponse::Ok().body("OK"))
}

#[derive(Deserialize)]
struct FListLookup {
    id: i32,
}

#[get("/flist/lookup")]
async fn flist_lookup(
    pool: web::Data<PgPool>,
    query: web::Query<FListLookup>,
) -> Result<HttpResponse, Error> {
    let file = models::FListFile::get_by_id(&pool, query.id).await?;

    Ok(HttpResponse::Ok().json(file))
}

pub fn service() -> Scope {
    web::scope("/api")
        .service(services![events, upload, flist_lookup])
        .service(web::scope("/service").service(services![fuzzysearch]))
        .service(web::scope("/chunk").service(services![chunk_add]))
}
