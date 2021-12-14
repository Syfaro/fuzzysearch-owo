use std::time::{Duration, Instant};

use actix::{
    fut::LocalBoxActorFuture, Actor, ActorContext, ActorFutureExt, ActorTryFutureExt, AsyncContext,
    Handler, Message, StreamHandler, WrapFuture,
};
use actix_http::ws::CloseCode;
use actix_web::{get, post, services, web, HttpRequest, HttpResponse, Scope};
use actix_web_actors::ws;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{jobs, models, Error};

const HEARTHEAT_INTERVAL: Duration = Duration::from_secs(5);
const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

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
    redis: redis::Client,
    hb: Instant,
}

impl WsEventSession {
    async fn new(user_id: Uuid, redis: redis::Client) -> Self {
        Self {
            user_id,
            redis,
            hb: Instant::now(),
        }
    }

    fn hb(&self, ctx: &mut ws::WebsocketContext<Self>) {
        ctx.run_interval(HEARTHEAT_INTERVAL, |act, ctx| {
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
                pubsub
                    .subscribe(format!("user-events:{}", user_id.to_string()))
                    .await?;

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
        let data = serde_json::to_string(&msg).expect("could not serialize essential data");
        ctx.text(data)
    }
}

#[get("/events")]
async fn events(
    user: Option<models::User>,
    req: HttpRequest,
    stream: web::Payload,
    redis: web::Data<redis::Client>,
) -> Result<HttpResponse, Error> {
    if let Some(user) = user {
        let session = WsEventSession::new(user.id, (*redis.into_inner()).clone()).await;
        ws::start(session, &req, stream).map_err(Into::into)
    } else {
        ws::start(UnauthorizedWsEventSession, &req, stream).map_err(Into::into)
    }
}

#[derive(Deserialize)]
struct FuzzySearchParams {
    secret: String,
}

#[post("/fuzzysearch/{secret}")]
async fn fuzzysearch(
    path: web::Path<FuzzySearchParams>,
    data: web::Json<fuzzysearch_common::faktory::WebHookData>,
    faktory: web::Data<jobs::FaktoryClient>,
    config: web::Data<crate::Config>,
) -> Result<HttpResponse, Error> {
    if path.secret != config.fuzzysearch_api_key {
        return Ok(HttpResponse::Unauthorized().finish());
    }

    tracing::info!("got webhook data: {:?}", data.0);
    faktory
        .enqueue_job(
            jobs::JobInitiator::external("fuzzysearch"),
            jobs::new_submission_job(data.0)?,
        )
        .await?;

    Ok(HttpResponse::Ok().body("OK"))
}

pub fn service() -> Scope {
    web::scope("/api")
        .service(services![events])
        .service(web::scope("/service").service(services![fuzzysearch]))
}
